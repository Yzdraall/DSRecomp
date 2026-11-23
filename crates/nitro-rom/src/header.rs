use serde::Serialize;

use crate::error::{Result, RomError};

pub const NDS_HEADER_SIZE: usize = 0x200;

/// Computes the standard Nintendo DS CRC16-MODBUS checksum (`poly = 0xA001`, `init = 0xFFFF`).
pub fn crc16_nds(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in bytes {
        crc ^= u16::from(b);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

#[inline]
pub(crate) fn read_u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

#[inline]
pub(crate) fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Nintendo DS unit target flag (`header[0x12]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum UnitCode {
    NdsOnly,
    NdsAndDsi,
    DsiOnly,
    Unknown(u8),
}

impl From<u8> for UnitCode {
    fn from(v: u8) -> Self {
        match v {
            0x00 => Self::NdsOnly,
            0x02 => Self::NdsAndDsi,
            0x03 => Self::DsiOnly,
            other => Self::Unknown(other),
        }
    }
}

/// Parsed 512-byte Nintendo DS cartridge header.
#[derive(Debug, Clone, Serialize)]
pub struct NdsHeader {
    pub game_title: String,
    pub game_code: String,
    pub maker_code: String,
    pub unit_code: UnitCode,
    pub rom_version: u8,
    pub capacity_shift: u8,
    pub cartridge_capacity_bytes: u64,
    pub arm9_rom_offset: u32,
    pub arm9_entry_address: u32,
    pub arm9_ram_address: u32,
    pub arm9_size: u32,
    pub arm7_rom_offset: u32,
    pub arm7_entry_address: u32,
    pub arm7_ram_address: u32,
    pub arm7_size: u32,
    pub fnt_offset: u32,
    pub fnt_size: u32,
    pub fat_offset: u32,
    pub fat_size: u32,
    pub arm9_overlay_offset: u32,
    pub arm9_overlay_size: u32,
    pub arm7_overlay_offset: u32,
    pub arm7_overlay_size: u32,
    pub icon_banner_offset: u32,
    pub secure_area_crc16: u16,
    pub total_used_rom_size: u32,
    pub header_size: u32,
    pub header_crc16_expected: u16,
    pub header_crc16_computed: u16,
    pub header_crc16_valid: bool,
}

impl NdsHeader {
    pub fn parse(rom: &[u8]) -> Result<Self> {
        if rom.len() < NDS_HEADER_SIZE {
            return Err(RomError::TooSmall {
                actual: rom.len(),
                minimum: NDS_HEADER_SIZE,
            });
        }

        let game_title = decode_ascii_trimmed(&rom[0x00..0x0C]);
        let game_code = decode_ascii_trimmed(&rom[0x0C..0x10]);
        let maker_code = decode_ascii_trimmed(&rom[0x10..0x12]);
        let unit_code = UnitCode::from(rom[0x12]);
        let capacity_shift = rom[0x14];
        let cartridge_capacity_bytes = 128u64
            .saturating_mul(1024)
            .checked_shl(u32::from(capacity_shift))
            .unwrap_or(0);
        let rom_version = rom[0x1E];

        let arm9_rom_offset = read_u32_le(rom, 0x20);
        let arm9_entry_address = read_u32_le(rom, 0x24);
        let arm9_ram_address = read_u32_le(rom, 0x28);
        let arm9_size = read_u32_le(rom, 0x2C);

        let arm7_rom_offset = read_u32_le(rom, 0x30);
        let arm7_entry_address = read_u32_le(rom, 0x34);
        let arm7_ram_address = read_u32_le(rom, 0x38);
        let arm7_size = read_u32_le(rom, 0x3C);

        let fnt_offset = read_u32_le(rom, 0x40);
        let fnt_size = read_u32_le(rom, 0x44);
        let fat_offset = read_u32_le(rom, 0x48);
        let fat_size = read_u32_le(rom, 0x4C);

        let arm9_overlay_offset = read_u32_le(rom, 0x50);
        let arm9_overlay_size = read_u32_le(rom, 0x54);
        let arm7_overlay_offset = read_u32_le(rom, 0x58);
        let arm7_overlay_size = read_u32_le(rom, 0x5C);

        let icon_banner_offset = read_u32_le(rom, 0x68);
        let secure_area_crc16 = read_u16_le(rom, 0x6C);
        let total_used_rom_size = read_u32_le(rom, 0x80);
        let header_size = read_u32_le(rom, 0x84);

        let header_crc16_expected = read_u16_le(rom, 0x15E);
        let header_crc16_computed = crc16_nds(&rom[0x000..0x15E]);

        Ok(Self {
            game_title,
            game_code,
            maker_code,
            unit_code,
            rom_version,
            capacity_shift,
            cartridge_capacity_bytes,
            arm9_rom_offset,
            arm9_entry_address,
            arm9_ram_address,
            arm9_size,
            arm7_rom_offset,
            arm7_entry_address,
            arm7_ram_address,
            arm7_size,
            fnt_offset,
            fnt_size,
            fat_offset,
            fat_size,
            arm9_overlay_offset,
            arm9_overlay_size,
            arm7_overlay_offset,
            arm7_overlay_size,
            icon_banner_offset,
            secure_area_crc16,
            total_used_rom_size,
            header_size,
            header_crc16_expected,
            header_crc16_computed,
            header_crc16_valid: header_crc16_expected == header_crc16_computed,
        })
    }
}

fn decode_ascii_trimmed(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}
