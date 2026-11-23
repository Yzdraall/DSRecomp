use std::collections::BTreeSet;

use nitro_arm::CpuMode;
use nitro_rom::NdsRom;
use serde::{Deserialize, Serialize};

use crate::cpu::CpuContext;

pub const MAIN_RAM_SIZE: usize = 4 * 1024 * 1024;
pub const ITCM_SIZE: usize = 32 * 1024;
pub const DTCM_SIZE: usize = 16 * 1024;
pub const WRAM_SIZE: usize = 64 * 1024;
pub const PALETTE_SIZE: usize = 2 * 1024;
pub const VRAM_TOTAL_SIZE: usize = 656 * 1024;
pub const OAM_SIZE: usize = 2 * 1024;

/// Real-time button & touch-stylus input state fed into the NDS hardware registers.
#[derive(Debug, Clone, Copy)]
pub struct NdsInputState {
    /// Active-low `KEYINPUT` (`0x0400_0130`): bit0=A, 1=B, 2=Select, 3=Start, 4=Right, 5=Left, 6=Up, 7=Down, 8=R, 9=L.
    pub keyinput: u16,
    /// Active-low `EXTKEYIN` (`0x0400_0136`): bit0=X, bit1=Y, bit6=PenDown(0 when pressed!).
    pub extkeyin: u16,
    pub stylus_x: u8,
    pub stylus_y: u8,
    pub stylus_down: bool,
}

impl Default for NdsInputState {
    fn default() -> Self {
        Self {
            keyinput: 0x03FF,
            extkeyin: 0x007F,
            stylus_x: 128,
            stylus_y: 96,
            stylus_down: false,
        }
    }
}

/// Recorded indirect jump target for `dsrecomp heal`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TracedTarget {
    pub overlay_id: Option<u32>,
    pub addr: u32,
    pub mode: CpuMode,
}

/// Complete Nintendo DS memory bus, MMIO registers, DMA controller, and HLE BIOS/Overlay services.
pub struct NitroBus {
    pub rom: NdsRom,
    pub itcm: Box<[u8; ITCM_SIZE]>,
    pub dtcm: Box<[u8; DTCM_SIZE]>,
    pub main_ram: Vec<u8>,
    pub wram: Box<[u8; WRAM_SIZE]>,
    pub io_regs: Box<[u8; 0x2000]>,
    pub palette: Box<[u8; PALETTE_SIZE]>,
    pub vram: Vec<u8>,
    pub oam: Box<[u8; OAM_SIZE]>,
    pub input: NdsInputState,
    pub frame_counter: u32,
    active_overlay: Option<u32>,
    pub traced_jumps: BTreeSet<TracedTarget>,
}

impl NitroBus {
    pub fn from_rom(rom: NdsRom) -> Self {
        let mut main_ram = vec![0u8; MAIN_RAM_SIZE];

        // Load decompressed static `arm9.bin` into Main RAM at `header.arm9_ram_address`
        let ram_base = (rom.header.arm9_ram_address & 0x003F_FFFF) as usize;
        let max_copy = rom
            .arm9_code
            .len()
            .min(MAIN_RAM_SIZE.saturating_sub(ram_base));
        main_ram[ram_base..ram_base + max_copy].copy_from_slice(&rom.arm9_code[..max_copy]);

        // Copy cartridge header into `0x023FFE00` (standard NitroSDK boot memory location)
        if rom.raw.len() >= 0x200 {
            main_ram[0x003F_FE00..0x0040_0000].copy_from_slice(&rom.raw[..0x200]);
        }

        let mut io_regs = Box::new([0u8; 0x2000]);
        // POWCNT1 (`0x0400_0304`): enable both LCDs & Engine A on Top + Engine B on Bottom (`0x8203`)
        io_regs[0x0304..0x0306].copy_from_slice(&0x8203u16.to_le_bytes());

        let mut bus = Self {
            rom,
            itcm: Box::new([0u8; ITCM_SIZE]),
            dtcm: Box::new([0u8; DTCM_SIZE]),
            main_ram,
            wram: Box::new([0u8; WRAM_SIZE]),
            io_regs,
            palette: Box::new([0u8; PALETTE_SIZE]),
            vram: vec![0u8; VRAM_TOTAL_SIZE],
            oam: Box::new([0u8; OAM_SIZE]),
            input: NdsInputState::default(),
            frame_counter: 0,
            active_overlay: None,
            traced_jumps: BTreeSet::new(),
        };
        bus.sync_input_mmio();
        bus
    }

    #[inline]
    pub fn active_arm9_overlay(&self) -> Option<u32> {
        self.active_overlay
    }

    /// Loads an ARM9 dynamic Overlay from the cartridge `FAT` into `Main RAM` and activates it in the AOT dispatcher.
    pub fn load_arm9_overlay(&mut self, overlay_id: u32) -> bool {
        let Ok(Some((entry, code))) = self.rom.load_arm9_overlay_code(overlay_id) else {
            return false;
        };
        let dst_off = (entry.ram_address & 0x003F_FFFF) as usize;
        let copy_len = code.len().min(MAIN_RAM_SIZE.saturating_sub(dst_off));
        self.main_ram[dst_off..dst_off + copy_len].copy_from_slice(&code[..copy_len]);

        let bss_start = dst_off.saturating_add(copy_len);
        let bss_end = bss_start
            .saturating_add(entry.bss_size as usize)
            .min(MAIN_RAM_SIZE);
        if bss_end > bss_start {
            self.main_ram[bss_start..bss_end].fill(0);
        }

        self.active_overlay = Some(overlay_id);
        true
    }

    pub fn sync_input_mmio(&mut self) {
        self.io_regs[0x0130..0x0132].copy_from_slice(&self.input.keyinput.to_le_bytes());
        let mut ext = self.input.extkeyin | (1 << 6);
        if self.input.stylus_down {
            ext &= !(1 << 6); // Active-low PenDown bit!
        }
        self.io_regs[0x0136..0x0138].copy_from_slice(&ext.to_le_bytes());

        // Populate standard shared IPC touch region (`0x027FFFAA` mirrored at `0x003FFFAA`)
        self.main_ram[0x003F_FFAA] = self.input.stylus_x;
        self.main_ram[0x003F_FFAB] = self.input.stylus_y;
        self.main_ram[0x003F_FFAC] = u8::from(self.input.stylus_down);
    }

    pub fn read8(&self, addr: u32) -> u8 {
        match addr >> 24 {
            0x00 | 0x01 => self.itcm[(addr as usize) & (ITCM_SIZE - 1)],
            0x02 => self.main_ram[(addr as usize) & (MAIN_RAM_SIZE - 1)],
            0x03 => {
                if addr >= 0x0380_0000 {
                    self.dtcm[(addr as usize) & (DTCM_SIZE - 1)]
                } else {
                    self.wram[(addr as usize) & (WRAM_SIZE - 1)]
                }
            }
            0x04 => self.read_mmio8(addr),
            0x05 => self.palette[(addr as usize) & (PALETTE_SIZE - 1)],
            0x06 => {
                let off = map_vram_offset(addr);
                self.vram[off]
            }
            0x07 => self.oam[(addr as usize) & (OAM_SIZE - 1)],
            _ => 0,
        }
    }

    pub fn read16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        u16::from_le_bytes([self.read8(a), self.read8(a.wrapping_add(1))])
    }

    pub fn read32(&self, addr: u32) -> u32 {
        let a = addr & !3;
        u32::from_le_bytes([
            self.read8(a),
            self.read8(a.wrapping_add(1)),
            self.read8(a.wrapping_add(2)),
            self.read8(a.wrapping_add(3)),
        ])
    }

    pub fn write8(&mut self, addr: u32, val: u8) {
        match addr >> 24 {
            0x00 | 0x01 => self.itcm[(addr as usize) & (ITCM_SIZE - 1)] = val,
            0x02 => self.main_ram[(addr as usize) & (MAIN_RAM_SIZE - 1)] = val,
            0x03 => {
                if addr >= 0x0380_0000 {
                    self.dtcm[(addr as usize) & (DTCM_SIZE - 1)] = val;
                } else {
                    self.wram[(addr as usize) & (WRAM_SIZE - 1)] = val;
                }
            }
            0x04 => {
                let reg_off = (addr & 0x1FFF) as usize;
                if (addr & 0xFFFF_0000) == 0x0400_0000 {
                    self.io_regs[reg_off] = val;
                }
            }
            0x05 => self.palette[(addr as usize) & (PALETTE_SIZE - 1)] = val,
            0x06 => {
                let off = map_vram_offset(addr);
                self.vram[off] = val;
            }
            0x07 => self.oam[(addr as usize) & (OAM_SIZE - 1)] = val,
            _ => {}
        }
    }

    pub fn write16(&mut self, addr: u32, val: u16) {
        let a = addr & !1;
        let bytes = val.to_le_bytes();
        if (a & 0xFFFF_0000) == 0x0400_0000 {
            let off = (a & 0x1FFF) as usize;
            self.io_regs[off..off + 2].copy_from_slice(&bytes);
            self.check_dma_trigger(off);
            return;
        }
        if (a & 0xFFFF_FF00) == 0x04FF_0000 {
            if a == 0x04FF_0010 {
                let _ = self.load_arm9_overlay(u32::from(val));
            }
            return;
        }
        self.write8(a, bytes[0]);
        self.write8(a.wrapping_add(1), bytes[1]);
    }

    pub fn write32(&mut self, addr: u32, val: u32) {
        let a = addr & !3;
        let bytes = val.to_le_bytes();
        if (a & 0xFFFF_0000) == 0x0400_0000 {
            let off = (a & 0x1FFF) as usize;
            if off + 4 <= self.io_regs.len() {
                self.io_regs[off..off + 4].copy_from_slice(&bytes);
                self.check_dma_trigger(off + 2);
            }
            return;
        }
        if a == 0x04FF_0010 {
            let _ = self.load_arm9_overlay(val);
            return;
        }
        self.write8(a, bytes[0]);
        self.write8(a.wrapping_add(1), bytes[1]);
        self.write8(a.wrapping_add(2), bytes[2]);
        self.write8(a.wrapping_add(3), bytes[3]);
    }

    fn read_mmio8(&self, addr: u32) -> u8 {
        // Direct DSRecomp high-level Stylus & Frame Counter MMIO (`0x04FF_0000..0x04FF_000F`)
        if (addr & 0xFFFF_FF00) == 0x04FF_0000 {
            return match addr & 0xFF {
                0x00 => self.input.stylus_x,
                0x01 => 0,
                0x02 => self.input.stylus_y,
                0x03 => 0,
                0x04 => u8::from(self.input.stylus_down),
                0x05..=0x07 => 0,
                0x08..=0x0B => {
                    let b = self.frame_counter.to_le_bytes();
                    b[(addr & 3) as usize]
                }
                0x10..=0x13 => {
                    let id = self.active_overlay.unwrap_or(u32::MAX).to_le_bytes();
                    id[(addr & 3) as usize]
                }
                _ => 0,
            };
        }
        if (addr & 0xFFFF_E000) == 0x0400_0000 {
            return self.io_regs[(addr & 0x1FFF) as usize];
        }
        0
    }

    fn check_dma_trigger(&mut self, written_halfword_off: usize) {
        // DMA0..3 control halfwords sit at `0xBA`, `0xC6`, `0xD2`, `0xDE`
        let ch = match written_halfword_off {
            0x00BA => 0usize,
            0x00C6 => 1usize,
            0x00D2 => 2usize,
            0x00DE => 3usize,
            _ => return,
        };
        let base = 0x00B0 + ch * 12;
        let sad = u32::from_le_bytes([
            self.io_regs[base],
            self.io_regs[base + 1],
            self.io_regs[base + 2],
            self.io_regs[base + 3],
        ]);
        let dad = u32::from_le_bytes([
            self.io_regs[base + 4],
            self.io_regs[base + 5],
            self.io_regs[base + 6],
            self.io_regs[base + 7],
        ]);
        let cnt_l = u16::from_le_bytes([self.io_regs[base + 8], self.io_regs[base + 9]]);
        let cnt_h = u16::from_le_bytes([self.io_regs[base + 10], self.io_regs[base + 11]]);

        if (cnt_h & 0x8000) == 0 {
            return;
        }
        // Only run immediate start mode (`bits[13:11] == 0`) synchronously
        if ((cnt_h >> 11) & 0x07) != 0 {
            return;
        }

        let is_32bit = ((cnt_h >> 10) & 1) != 0;
        let step: i32 = if is_32bit { 4 } else { 2 };
        let dst_ctrl = (cnt_h >> 5) & 3;
        let src_ctrl = (cnt_h >> 7) & 3;
        let count: u32 = if cnt_l == 0 {
            if ch == 3 {
                0x1_0000
            } else {
                0x4000
            }
        } else {
            u32::from(cnt_l)
        };

        let mut s = sad;
        let mut d = dad;
        for _ in 0..count {
            if is_32bit {
                let v = self.read32(s);
                self.write32(d, v);
            } else {
                let v = self.read16(s);
                self.write16(d, v);
            }
            s = match src_ctrl {
                0 => s.wrapping_add(step as u32),
                1 => s.wrapping_sub(step as u32),
                _ => s,
            };
            d = match dst_ctrl {
                0 | 3 => d.wrapping_add(step as u32),
                1 => d.wrapping_sub(step as u32),
                _ => d,
            };
        }

        // Clear DMA Enable bit if Repeat (`bit 9`) is unset
        if (cnt_h & (1 << 9)) == 0 {
            let cleared = cnt_h & !0x8000;
            self.io_regs[base + 10..base + 12].copy_from_slice(&cleared.to_le_bytes());
        }
    }

    /// Executes Nintendo DS BIOS `SWI` HLE services + DSRecomp Overlay/NitroFS fast-calls.
    pub fn swi_hle(&mut self, cpu: &mut CpuContext, raw_swi: u32) {
        // In ARM mode, NitroSDK encodes the SWI index in bits[23:16] (`swi 0x050000`); in Thumb, bits[7:0].
        let swi_num = if !cpu.thumb && raw_swi >= 0x100 {
            (raw_swi >> 16) & 0xFF
        } else {
            raw_swi & 0xFF
        };

        match swi_num {
            // 0x03: WaitByLoop
            0x03 => {
                cpu.regs[0] = 0;
            }
            // 0x04 / 0x05: IntrWait / VBlankIntrWait -> yield current frame cleanly!
            0x04 | 0x05 => {
                cpu.vblank_requested = true;
            }
            // 0x06: Halt
            0x06 => {
                cpu.vblank_requested = true;
            }
            // 0x09: Div (`R0 / R1 -> R0`, `R0 % R1 -> R1`, `|R0 / R1| -> R3`)
            0x09 => {
                let num = cpu.regs[0] as i32;
                let den = cpu.regs[1] as i32;
                if den != 0 {
                    let q = num.wrapping_div(den);
                    let r = num.wrapping_rem(den);
                    cpu.regs[0] = q as u32;
                    cpu.regs[1] = r as u32;
                    cpu.regs[3] = q.wrapping_abs() as u32;
                }
            }
            // 0x0B: CpuSet (copy/fill 16-bit or 32-bit units)
            0x0B => {
                let mut src = cpu.regs[0];
                let mut dst = cpu.regs[1];
                let cnt = cpu.regs[2];
                let count = cnt & 0x001F_FFFF;
                let fixed = ((cnt >> 24) & 1) != 0;
                let is_32 = ((cnt >> 26) & 1) != 0;
                for _ in 0..count {
                    if is_32 {
                        let v = self.read32(src);
                        self.write32(dst, v);
                        if !fixed {
                            src = src.wrapping_add(4);
                        }
                        dst = dst.wrapping_add(4);
                    } else {
                        let v = self.read16(src);
                        self.write16(dst, v);
                        if !fixed {
                            src = src.wrapping_add(2);
                        }
                        dst = dst.wrapping_add(2);
                    }
                }
            }
            // 0x0C: CpuFastSet (32-bit words)
            0x0C => {
                let mut src = cpu.regs[0] & !3;
                let mut dst = cpu.regs[1] & !3;
                let cnt = cpu.regs[2];
                let count = cnt & 0x001F_FFFF;
                let fixed = ((cnt >> 24) & 1) != 0;
                for _ in 0..count {
                    let v = self.read32(src);
                    self.write32(dst, v);
                    if !fixed {
                        src = src.wrapping_add(4);
                    }
                    dst = dst.wrapping_add(4);
                }
            }
            // 0x0D: Sqrt (`isqrt(R0) -> R0`)
            0x0D => {
                cpu.regs[0] = cpu.regs[0].isqrt();
            }
            // 0xFE: DSRecomp HLE NitroFS file loader (`R0 = file_id`, `R1 = dst_addr`, `R2 = max_len` -> `R0 = bytes_read`)
            0xFE => {
                let fid = cpu.regs[0] as usize;
                let dst = cpu.regs[1];
                let max_len = cpu.regs[2] as usize;
                if let Some(fe) = self.rom.nitrofs.fat.get(fid).copied() {
                    let start = fe.start_offset as usize;
                    let end = (fe.end_offset as usize).min(self.rom.raw.len());
                    if end > start {
                        let len = (end - start).min(max_len);
                        let blob = self.rom.raw[start..start + len].to_vec();
                        for (i, b) in blob.into_iter().enumerate() {
                            self.write8(dst.wrapping_add(i as u32), b);
                        }
                        cpu.regs[0] = len as u32;
                    } else {
                        cpu.regs[0] = 0;
                    }
                } else {
                    cpu.regs[0] = 0;
                }
            }
            // 0xFF: DSRecomp HLE Overlay loader (`R0 = overlay_id` -> `R0 = 1 if ok`)
            0xFF => {
                let ok = self.load_arm9_overlay(cpu.regs[0]);
                cpu.regs[0] = u32::from(u8::from(ok));
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn coproc_move(
        &mut self,
        cpu: &mut CpuContext,
        to_coproc: bool,
        cp: u8,
        _op1: u8,
        rd: usize,
        crn: u8,
        crm: u8,
        op2: u8,
    ) {
        // CP15 `MCR p15, 0, Rd, c7, c0, 4` == ARM946E-S Wait-For-Interrupt (`Halt` until VBlank!)
        if cp == 15 && to_coproc && crn == 7 && crm == 0 && op2 == 4 {
            cpu.vblank_requested = true;
        } else if cp == 15 && !to_coproc {
            // Return reasonable default CP15 ID/control register value
            cpu.regs[rd] = match crn {
                0 => 0x4105_9461, // ARM946E-S Main ID
                1 => 0x0005_2078, // Control register
                _ => 0,
            };
        }
    }
}

/// Maps NDS VRAM virtual regions into our contiguous `656 KiB` backing array:
/// - `0x0600_0000..0x0603_FFFF` (Engine A BG, 256 KiB)  -> `[0x00000..0x40000]` (shared with LCDC VRAM A+B!)
/// - `0x0680_0000..0x0687_FFFF` (LCDC VRAM A..D, 512 KiB)-> `[0x00000..0x80000]`
/// - `0x0620_0000..0x0621_FFFF` (Engine B BG, 128 KiB)  -> `[0x40000..0x60000]` (shared with VRAM C!)
/// - `0x0640_0000..0x0641_FFFF` (Engine A OBJ, 128 KiB) -> `[0x80000..0x90000]` (64 KiB)
/// - `0x0660_0000..0x0661_FFFF` (Engine B OBJ, 128 KiB) -> `[0x90000..0xA0000]` (64 KiB)
#[inline]
pub fn map_vram_offset(addr: u32) -> usize {
    let sub = (addr >> 20) & 0x0F;
    match sub {
        // Engine A BG (`0x060x_xxxx` / `0x061x_xxxx`) -> first 256 KiB
        0x0 | 0x1 => (addr as usize) & 0x3_FFFF,
        // Engine B BG (`0x062x_xxxx` / `0x063x_xxxx`) -> VRAM C (`0x40000..0x60000`)
        0x2 | 0x3 => 0x4_0000 + ((addr as usize) & 0x1_FFFF),
        // Engine A OBJ (`0x064x_xxxx`) -> `0x80000..0x90000`
        0x4 | 0x5 => 0x8_0000 + ((addr as usize) & 0x0_FFFF),
        // Engine B OBJ (`0x066x_xxxx`) -> `0x90000..0xA0000`
        0x6 | 0x7 => 0x9_0000 + ((addr as usize) & 0x0_FFFF),
        // Direct LCDC VRAM Banks A..D (`0x0680_0000..0x0687_FFFF`) -> `0x00000..0x80000`
        0x8 => (addr as usize) & 0x7_FFFF,
        _ => (addr as usize) % VRAM_TOTAL_SIZE,
    }
}
