use std::collections::BTreeMap;

use crate::banner::BANNER_V1_SIZE;
use crate::blz::blz_compress;
use crate::header::{crc16_nds, NDS_HEADER_SIZE};
use crate::overlay::OVERLAY_ENTRY_SIZE;

/// Overlay specification for building a clean-room `.nds` cartridge image.
#[derive(Debug, Clone)]
pub struct OverlaySpec {
    pub overlay_id: u32,
    pub ram_address: u32,
    pub bss_size: u32,
    pub code: Vec<u8>,
    pub compress_blz: bool,
}

/// Clean-room Nintendo DS (`.nds`) Cartridge Image Builder.
///
/// Produces hardware-accurate `.nds` images complete with CRC16-MODBUS header & banner checksums,
/// optional BLZ-compressed ARM9 Overlays, and a valid flat `NitroFS` (`FNT` + `FAT`) filesystem.
#[derive(Debug, Clone)]
pub struct NdsRomBuilder {
    pub game_title: String,
    pub game_code: [u8; 4],
    pub maker_code: [u8; 2],
    pub banner_title: String,
    pub banner_accent_rgb555: u16,
    pub arm9_entry_address: u32,
    pub arm9_ram_address: u32,
    pub arm9_code: Vec<u8>,
    pub arm7_entry_address: u32,
    pub arm7_ram_address: u32,
    pub arm7_code: Vec<u8>,
    pub overlays: Vec<OverlaySpec>,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Default for NdsRomBuilder {
    fn default() -> Self {
        // Minimal ARM7 infinite wait loop (`b .` = 0xEAFFFFFE)
        Self {
            game_title: "NITRO_DEMO".to_string(),
            game_code: *b"NDMO",
            maker_code: *b"NR",
            banner_title: "NitroRecomp Clean-Room Demo\nAhead-of-Time Safe Rust".to_string(),
            banner_accent_rgb555: 0x7E10,
            arm9_entry_address: 0x0200_0800,
            arm9_ram_address: 0x0200_0800,
            arm9_code: vec![0xFE, 0xFF, 0xFF, 0xEA],
            arm7_entry_address: 0x0238_0000,
            arm7_ram_address: 0x0238_0000,
            arm7_code: vec![0xFE, 0xFF, 0xFF, 0xEA],
            overlays: Vec::new(),
            files: BTreeMap::new(),
        }
    }
}

impl NdsRomBuilder {
    pub fn new(title: &str, game_code: [u8; 4]) -> Self {
        Self {
            game_title: title.to_string(),
            game_code,
            ..Self::default()
        }
    }

    pub fn with_arm9(mut self, ram_addr: u32, entry_addr: u32, code: Vec<u8>) -> Self {
        self.arm9_ram_address = ram_addr;
        self.arm9_entry_address = entry_addr;
        self.arm9_code = code;
        self
    }

    pub fn add_overlay(mut self, spec: OverlaySpec) -> Self {
        self.overlays.push(spec);
        self
    }

    pub fn add_file(mut self, name: &str, data: Vec<u8>) -> Self {
        self.files
            .insert(name.trim_start_matches('/').to_string(), data);
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];

        // 1. Append ARM9 binary at 0x4000
        let arm9_rom_offset = rom.len() as u32;
        let arm9_size = self.arm9_code.len() as u32;
        rom.extend_from_slice(&self.arm9_code);
        align_to(&mut rom, 0x200);

        // 2. Prepare overlay payloads + ARM9 Overlay Table (OVT)
        let mut fat_blobs: Vec<Vec<u8>> = Vec::new();
        let mut ovt_bytes = Vec::new();
        for ov in &self.overlays {
            let file_id = fat_blobs.len() as u32;
            let ram_size = ov.code.len() as u32;
            let (stored, is_blz) = if ov.compress_blz {
                let c = blz_compress(&ov.code);
                if c.len() < ov.code.len() {
                    (c, true)
                } else {
                    (ov.code.clone(), false)
                }
            } else {
                (ov.code.clone(), false)
            };
            let comp_size = stored.len() as u32;
            fat_blobs.push(stored);

            let mut rec = [0u8; OVERLAY_ENTRY_SIZE];
            rec[0x00..0x04].copy_from_slice(&ov.overlay_id.to_le_bytes());
            rec[0x04..0x08].copy_from_slice(&ov.ram_address.to_le_bytes());
            rec[0x08..0x0C].copy_from_slice(&ram_size.to_le_bytes());
            rec[0x0C..0x10].copy_from_slice(&ov.bss_size.to_le_bytes());
            rec[0x10..0x14].copy_from_slice(&0u32.to_le_bytes());
            rec[0x14..0x18].copy_from_slice(&0u32.to_le_bytes());
            rec[0x18..0x1C].copy_from_slice(&file_id.to_le_bytes());
            let word7 = (comp_size & 0x00FF_FFFF) | (u32::from(u8::from(is_blz)) << 24);
            rec[0x1C..0x20].copy_from_slice(&word7.to_le_bytes());
            ovt_bytes.extend_from_slice(&rec);
        }

        let (arm9_ov_offset, arm9_ov_size) = if ovt_bytes.is_empty() {
            (0u32, 0u32)
        } else {
            let off = rom.len() as u32;
            let sz = ovt_bytes.len() as u32;
            rom.extend_from_slice(&ovt_bytes);
            align_to(&mut rom, 0x200);
            (off, sz)
        };

        // 3. Append ARM7 stub
        let arm7_rom_offset = rom.len() as u32;
        let arm7_size = self.arm7_code.len() as u32;
        rom.extend_from_slice(&self.arm7_code);
        align_to(&mut rom, 0x200);

        // 4. Build single-root FNT (supports flat filenames cleanly; slashes flattened to '_')
        let first_named_file_id = fat_blobs.len() as u16;
        let mut fnt = vec![0u8; 8];
        fnt[0..4].copy_from_slice(&8u32.to_le_bytes()); // root subtable at offset 8
        fnt[4..6].copy_from_slice(&first_named_file_id.to_le_bytes());
        fnt[6..8].copy_from_slice(&1u16.to_le_bytes()); // 1 directory (root)

        for (raw_name, data) in &self.files {
            let clean_name = raw_name.replace('/', "_");
            let name_bytes = clean_name.as_bytes();
            let len = name_bytes.len().min(127);
            fnt.push(len as u8);
            fnt.extend_from_slice(&name_bytes[..len]);
            fat_blobs.push(data.clone());
        }
        fnt.push(0x00); // end of root subtable

        let fnt_offset = rom.len() as u32;
        let fnt_size = fnt.len() as u32;
        rom.extend_from_slice(&fnt);
        align_to(&mut rom, 0x200);

        // 5. Reserve space for FAT, place Banner + File Blobs, then patch FAT
        let fat_offset = rom.len() as u32;
        let fat_size = (fat_blobs.len() * 8) as u32;
        rom.resize(rom.len() + fat_size as usize, 0);
        align_to(&mut rom, 0x200);

        let banner_offset = rom.len() as u32;
        let banner = build_banner(&self.banner_title, self.banner_accent_rgb555);
        rom.extend_from_slice(&banner);
        align_to(&mut rom, 0x200);

        let mut fat_bytes = Vec::with_capacity(fat_size as usize);
        for blob in &fat_blobs {
            let start = rom.len() as u32;
            rom.extend_from_slice(blob);
            let end = rom.len() as u32;
            fat_bytes.extend_from_slice(&start.to_le_bytes());
            fat_bytes.extend_from_slice(&end.to_le_bytes());
            align_to(&mut rom, 0x200);
        }
        let fat_start_usize = fat_offset as usize;
        rom[fat_start_usize..fat_start_usize + fat_bytes.len()].copy_from_slice(&fat_bytes);

        let total_used_rom_size = rom.len() as u32;

        // Compute minimal cartridge capacity shift: 128 KiB << shift >= rom.len()
        let mut shift = 0u8;
        while ((128usize * 1024) << shift) < rom.len() && shift < 12 {
            shift += 1;
        }

        // 6. Populate 512-byte NDS header
        let title_bytes = self.game_title.as_bytes();
        let copy_len = title_bytes.len().min(12);
        rom[0x00..copy_len].copy_from_slice(&title_bytes[..copy_len]);
        rom[0x0C..0x10].copy_from_slice(&self.game_code);
        rom[0x10..0x12].copy_from_slice(&self.maker_code);
        rom[0x12] = 0x00; // NDS
        rom[0x14] = shift;

        rom[0x20..0x24].copy_from_slice(&arm9_rom_offset.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&self.arm9_entry_address.to_le_bytes());
        rom[0x28..0x2C].copy_from_slice(&self.arm9_ram_address.to_le_bytes());
        rom[0x2C..0x30].copy_from_slice(&arm9_size.to_le_bytes());

        rom[0x30..0x34].copy_from_slice(&arm7_rom_offset.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&self.arm7_entry_address.to_le_bytes());
        rom[0x38..0x3C].copy_from_slice(&self.arm7_ram_address.to_le_bytes());
        rom[0x3C..0x40].copy_from_slice(&arm7_size.to_le_bytes());

        rom[0x40..0x44].copy_from_slice(&fnt_offset.to_le_bytes());
        rom[0x44..0x48].copy_from_slice(&fnt_size.to_le_bytes());
        rom[0x48..0x4C].copy_from_slice(&fat_offset.to_le_bytes());
        rom[0x4C..0x50].copy_from_slice(&fat_size.to_le_bytes());

        rom[0x50..0x54].copy_from_slice(&arm9_ov_offset.to_le_bytes());
        rom[0x54..0x58].copy_from_slice(&arm9_ov_size.to_le_bytes());

        rom[0x68..0x6C].copy_from_slice(&banner_offset.to_le_bytes());
        rom[0x80..0x84].copy_from_slice(&total_used_rom_size.to_le_bytes());
        rom[0x84..0x88].copy_from_slice(&(NDS_HEADER_SIZE as u32).to_le_bytes());

        let hdr_crc = crc16_nds(&rom[0x000..0x15E]);
        rom[0x15E..0x160].copy_from_slice(&hdr_crc.to_le_bytes());

        rom
    }
}

fn align_to(buf: &mut Vec<u8>, align: usize) {
    let rem = buf.len() % align;
    if rem != 0 {
        buf.resize(buf.len() + (align - rem), 0);
    }
}

fn build_banner(title: &str, accent: u16) -> [u8; BANNER_V1_SIZE] {
    let mut b = [0u8; BANNER_V1_SIZE];
    b[0..2].copy_from_slice(&0x0001u16.to_le_bytes()); // v1

    // Procedural geometric 32x32 icon (16-color palette)
    let pal: [u16; 16] = [
        0x0000, // transparent
        0x0842, // dark slate bg
        accent, // primary accent
        0x7FFF, // white highlight
        0x03E0, // emerald
        0x7C1F, // magenta
        0x03FF, // gold/yellow
        0x7FE0, // cyan
        0x4210, 0x5294, 0x6318, 0x739C, 0x1084, 0x2108, 0x318C, 0x294A,
    ];
    for (i, &c) in pal.iter().enumerate() {
        let off = 0x220 + i * 2;
        b[off..off + 2].copy_from_slice(&c.to_le_bytes());
    }

    for y in 0..32usize {
        for x_pair in 0..16usize {
            let x0 = x_pair * 2;
            let x1 = x0 + 1;
            let p0 = procedural_icon_pixel(x0, y);
            let p1 = procedural_icon_pixel(x1, y);

            let tile_idx = (y / 8) * 4 + (x0 / 8);
            let py = y % 8;
            let px_pair = (x0 % 8) / 2;
            b[0x020 + tile_idx * 32 + py * 4 + px_pair] = (p1 << 4) | (p0 & 0x0F);
        }
    }

    // Write UTF-16LE title across all 6 language slots
    let mut utf16 = Vec::new();
    for u in title.encode_utf16().take(127) {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    for lang in 0..6usize {
        let off = 0x240 + lang * 0x100;
        b[off..off + utf16.len()].copy_from_slice(&utf16);
    }

    let crc = crc16_nds(&b[0x020..BANNER_V1_SIZE]);
    b[0x002..0x004].copy_from_slice(&crc.to_le_bytes());
    b
}

fn procedural_icon_pixel(x: usize, y: usize) -> u8 {
    if x == 0 || x == 31 || y == 0 || y == 31 {
        return 3; // border
    }
    // Dual-screen silhouette inside 32x32 icon!
    if (6..=25).contains(&x) && (4..=14).contains(&y) {
        if x == 6 || x == 25 || y == 4 || y == 14 {
            return 2;
        }
        return if (x + y).is_multiple_of(5) { 7 } else { 1 };
    }
    if (6..=25).contains(&x) && (17..=27).contains(&y) {
        if x == 6 || x == 25 || y == 17 || y == 27 {
            return 6;
        }
        return if (x ^ y).is_multiple_of(4) { 4 } else { 1 };
    }
    1
}
