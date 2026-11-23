use crate::isa::Cond;

/// Ergonomic clean-room 32-bit `ARMv5TE` machine-code emitter for unit tests and NDS demo ROMs.
#[derive(Debug, Default, Clone)]
pub struct ArmAsm {
    bytes: Vec<u8>,
}

impl ArmAsm {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn offset(&self) -> u32 {
        self.bytes.len() as u32
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn emit_u32(&mut self, word: u32) {
        self.bytes.extend_from_slice(&word.to_le_bytes());
    }

    pub fn nop(&mut self) {
        self.emit_u32(0xE1A0_0000);
    }

    /// Emits a 32-bit constant load into `rd` using `MOV`/`ORR` immediate byte lanes (1 to 4 instructions).
    pub fn mov_const32(&mut self, rd: u8, val: u32) {
        if let Some(enc) = encode_arm_imm12(val) {
            self.dp_imm(Cond::Al, 0xD, false, 0, rd, enc);
            return;
        }
        if let Some(enc) = encode_arm_imm12(!val) {
            self.dp_imm(Cond::Al, 0xF, false, 0, rd, enc); // MVN
            return;
        }
        let mut initialized = false;
        for byte_idx in 0..4u32 {
            let byte = (val >> (byte_idx * 8)) & 0xFF;
            if byte == 0 && (initialized || byte_idx < 3) {
                continue;
            }
            let rot_bits = (32 - byte_idx * 8) & 0x1F;
            let rot = rot_bits / 2;
            let imm12 = ((rot as u16) << 8) | (byte as u16);
            if !initialized {
                self.dp_imm(Cond::Al, 0xD, false, 0, rd, imm12); // MOV
                initialized = true;
            } else {
                self.dp_imm(Cond::Al, 0xC, false, rd, rd, imm12); // ORR
            }
        }
    }

    pub fn dp_imm(&mut self, cond: Cond, opcode: u8, s: bool, rn: u8, rd: u8, imm12: u16) {
        let w = (u32::from(cond as u8) << 28)
            | (1 << 25)
            | (u32::from(opcode & 0x0F) << 21)
            | (u32::from(u8::from(s)) << 20)
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | u32::from(imm12 & 0x0FFF);
        self.emit_u32(w);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dp_reg(
        &mut self,
        cond: Cond,
        opcode: u8,
        s: bool,
        rn: u8,
        rd: u8,
        rm: u8,
        shift_type: u8,
        shift_imm: u8,
    ) {
        let w = (u32::from(cond as u8) << 28)
            | (u32::from(opcode & 0x0F) << 21)
            | (u32::from(u8::from(s)) << 20)
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | (u32::from(shift_imm & 0x1F) << 7)
            | (u32::from(shift_type & 0x03) << 5)
            | u32::from(rm & 0x0F);
        self.emit_u32(w);
    }

    pub fn add_imm(&mut self, rd: u8, rn: u8, imm8: u8) {
        self.dp_imm(Cond::Al, 0x4, false, rn, rd, u16::from(imm8));
    }

    pub fn sub_imm(&mut self, rd: u8, rn: u8, imm8: u8) {
        self.dp_imm(Cond::Al, 0x2, false, rn, rd, u16::from(imm8));
    }

    pub fn subs_imm(&mut self, rd: u8, rn: u8, imm8: u8) {
        self.dp_imm(Cond::Al, 0x2, true, rn, rd, u16::from(imm8));
    }

    pub fn cmp_imm(&mut self, rn: u8, imm8: u8) {
        self.dp_imm(Cond::Al, 0xA, true, rn, 0, u16::from(imm8));
    }

    pub fn cmp_reg(&mut self, rn: u8, rm: u8) {
        self.dp_reg(Cond::Al, 0xA, true, rn, 0, rm, 0, 0);
    }

    pub fn add_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp_reg(Cond::Al, 0x4, false, rn, rd, rm, 0, 0);
    }

    pub fn sub_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp_reg(Cond::Al, 0x2, false, rn, rd, rm, 0, 0);
    }

    pub fn and_imm(&mut self, rd: u8, rn: u8, imm8: u8) {
        self.dp_imm(Cond::Al, 0x0, false, rn, rd, u16::from(imm8));
    }

    pub fn eor_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp_reg(Cond::Al, 0x1, false, rn, rd, rm, 0, 0);
    }

    pub fn orr_reg(&mut self, rd: u8, rn: u8, rm: u8) {
        self.dp_reg(Cond::Al, 0xC, false, rn, rd, rm, 0, 0);
    }

    pub fn lsl_imm(&mut self, rd: u8, rm: u8, amount: u8) {
        self.dp_reg(Cond::Al, 0xD, false, 0, rd, rm, 0, amount);
    }

    pub fn lsr_imm(&mut self, rd: u8, rm: u8, amount: u8) {
        self.dp_reg(Cond::Al, 0xD, false, 0, rd, rm, 1, amount);
    }

    pub fn mul(&mut self, rd: u8, rm: u8, rs: u8) {
        let w = 0xE000_0090
            | (u32::from(rd & 0x0F) << 16)
            | (u32::from(rs & 0x0F) << 8)
            | u32::from(rm & 0x0F);
        self.emit_u32(w);
    }

    pub fn str_u32_imm(&mut self, rd: u8, rn: u8, offset: u16) {
        let w = 0xE580_0000
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | u32::from(offset & 0x0FFF);
        self.emit_u32(w);
    }

    pub fn ldr_u32_imm(&mut self, rd: u8, rn: u8, offset: u16) {
        let w = 0xE590_0000
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | u32::from(offset & 0x0FFF);
        self.emit_u32(w);
    }

    pub fn str_u16_imm(&mut self, rd: u8, rn: u8, offset: u8) {
        let hi = (offset >> 4) & 0x0F;
        let lo = offset & 0x0F;
        let w = 0xE1C0_00B0
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | (u32::from(hi) << 8)
            | u32::from(lo);
        self.emit_u32(w);
    }

    pub fn str_u16_post(&mut self, rd: u8, rn: u8, step: u8) {
        let hi = (step >> 4) & 0x0F;
        let lo = step & 0x0F;
        let w = 0xE0C0_00B0
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | (u32::from(hi) << 8)
            | u32::from(lo);
        self.emit_u32(w);
    }

    pub fn ldr_u16_imm(&mut self, rd: u8, rn: u8, offset: u8) {
        let hi = (offset >> 4) & 0x0F;
        let lo = offset & 0x0F;
        let w = 0xE1D0_00B0
            | (u32::from(rn & 0x0F) << 16)
            | (u32::from(rd & 0x0F) << 12)
            | (u32::from(hi) << 8)
            | u32::from(lo);
        self.emit_u32(w);
    }

    pub fn push(&mut self, mask: u16) {
        self.emit_u32(0xE92D_0000 | u32::from(mask));
    }

    pub fn pop(&mut self, mask: u16) {
        self.emit_u32(0xE8BD_0000 | u32::from(mask));
    }

    pub fn bx(&mut self, rm: u8) {
        self.emit_u32(0xE12F_FF10 | u32::from(rm & 0x0F));
    }

    pub fn swi(&mut self, num: u8) {
        self.emit_u32(0xEF00_0000 | (u32::from(num) << 16));
    }

    /// Emits `B<cond>` or `BL<cond>` from current `self.offset()` to `target_offset`.
    pub fn branch_to_offset(&mut self, cond: Cond, link: bool, target_offset: u32) {
        let cur = self.offset() as i32;
        let rel = ((target_offset as i32) - (cur + 8)) >> 2;
        let imm24 = (rel as u32) & 0x00FF_FFFF;
        let w = (u32::from(cond as u8) << 28)
            | (0b101 << 25)
            | (u32::from(u8::from(link)) << 24)
            | imm24;
        self.emit_u32(w);
    }

    /// Emits `BLX <thumb_offset>` (`ARM -> Thumb` immediate call).
    pub fn blx_to_thumb_offset(&mut self, thumb_offset: u32) {
        let cur = self.offset() as i32;
        let diff = (thumb_offset as i32) - (cur + 8);
        let h = ((diff >> 1) & 1) as u32;
        let imm24 = ((diff >> 2) as u32) & 0x00FF_FFFF;
        let w = 0xFA00_0000 | (h << 24) | imm24;
        self.emit_u32(w);
    }

    /// Emits `LDRLS PC, [PC, Rn, LSL #2]` for switch jump-tables.
    pub fn ldrls_pc_jump_table(&mut self, idx_reg: u8) {
        let w = 0x979F_F100 | u32::from(idx_reg & 0x0F);
        self.emit_u32(w);
    }
}

/// Ergonomic clean-room 16-bit `Thumb` machine-code emitter.
#[derive(Debug, Default, Clone)]
pub struct ThumbAsm {
    bytes: Vec<u8>,
}

impl ThumbAsm {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn offset(&self) -> u32 {
        self.bytes.len() as u32
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn emit_u16(&mut self, hw: u16) {
        self.bytes.extend_from_slice(&hw.to_le_bytes());
    }

    pub fn mov_imm(&mut self, rd: u8, imm8: u8) {
        self.emit_u16(0x2000 | (u16::from(rd & 7) << 8) | u16::from(imm8));
    }

    pub fn add_imm(&mut self, rd: u8, imm8: u8) {
        self.emit_u16(0x3000 | (u16::from(rd & 7) << 8) | u16::from(imm8));
    }

    pub fn sub_imm(&mut self, rd: u8, imm8: u8) {
        self.emit_u16(0x3800 | (u16::from(rd & 7) << 8) | u16::from(imm8));
    }

    pub fn lsl_imm(&mut self, rd: u8, rm: u8, imm5: u8) {
        self.emit_u16((u16::from(imm5 & 0x1F) << 6) | (u16::from(rm & 7) << 3) | u16::from(rd & 7));
    }

    pub fn lsr_imm(&mut self, rd: u8, rm: u8, imm5: u8) {
        self.emit_u16(
            0x0800 | (u16::from(imm5 & 0x1F) << 6) | (u16::from(rm & 7) << 3) | u16::from(rd & 7),
        );
    }

    pub fn add_reg3(&mut self, rd: u8, rn: u8, rm: u8) {
        self.emit_u16(
            0x1800 | (u16::from(rm & 7) << 6) | (u16::from(rn & 7) << 3) | u16::from(rd & 7),
        );
    }

    pub fn eor_reg(&mut self, rd: u8, rm: u8) {
        self.emit_u16(0x4040 | (u16::from(rm & 7) << 3) | u16::from(rd & 7));
    }

    pub fn orr_reg(&mut self, rd: u8, rm: u8) {
        self.emit_u16(0x4300 | (u16::from(rm & 7) << 3) | u16::from(rd & 7));
    }

    pub fn mul_reg(&mut self, rd: u8, rm: u8) {
        self.emit_u16(0x4340 | (u16::from(rm & 7) << 3) | u16::from(rd & 7));
    }

    pub fn push(&mut self, rlist: u8, lr: bool) {
        self.emit_u16(0xB400 | (u16::from(u8::from(lr)) << 8) | u16::from(rlist));
    }

    pub fn pop(&mut self, rlist: u8, pc: bool) {
        self.emit_u16(0xBC00 | (u16::from(u8::from(pc)) << 8) | u16::from(rlist));
    }

    pub fn bx(&mut self, rm: u8) {
        self.emit_u16(0x4700 | (u16::from(rm & 0x0F) << 3));
    }
}

fn encode_arm_imm12(val: u32) -> Option<u16> {
    for rot in 0..16u32 {
        let candidate = val.rotate_left(rot * 2);
        if candidate <= 0xFF {
            return Some(((rot as u16) << 8) | (candidate as u16));
        }
    }
    None
}
