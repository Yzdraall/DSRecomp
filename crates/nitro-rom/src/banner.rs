use serde::Serialize;

use crate::error::{Result, RomError};
use crate::header::{crc16_nds, read_u16_le};

pub const BANNER_V1_SIZE: usize = 0x840;

/// Parsed Nintendo DS Icon/Title Banner (`0x840` bytes).
#[derive(Debug, Clone, Serialize)]
pub struct NdsBanner {
    pub version: u16,
    pub crc16_expected: u16,
    pub crc16_computed: u16,
    pub crc16_valid: bool,
    pub title_japanese: String,
    pub title_english: String,
    pub title_french: String,
    pub title_german: String,
    pub title_italian: String,
    pub title_spanish: String,
    #[serde(skip)]
    pub icon_rgba8: [u8; 32 * 32 * 4],
}

impl NdsBanner {
    pub fn parse(rom: &[u8], offset: u32) -> Result<Option<Self>> {
        if offset == 0 {
            return Ok(None);
        }
        let start = offset as usize;
        let end = start
            .checked_add(BANNER_V1_SIZE)
            .ok_or(RomError::SliceOutOfBounds {
                context: "icon_banner",
                offset: u64::from(offset),
                size: BANNER_V1_SIZE as u64,
                rom_len: rom.len() as u64,
            })?;
        if end > rom.len() {
            return Err(RomError::SliceOutOfBounds {
                context: "icon_banner",
                offset: u64::from(offset),
                size: BANNER_V1_SIZE as u64,
                rom_len: rom.len() as u64,
            });
        }

        let b = &rom[start..end];
        let version = read_u16_le(b, 0x000);
        let crc16_expected = read_u16_le(b, 0x002);
        let crc16_computed = crc16_nds(&b[0x020..BANNER_V1_SIZE]);

        let icon_rgba8 = decode_icon_rgba8(&b[0x020..0x220], &b[0x220..0x240]);

        Ok(Some(Self {
            version,
            crc16_expected,
            crc16_computed,
            crc16_valid: crc16_expected == crc16_computed,
            title_japanese: decode_utf16le_title(&b[0x240..0x340]),
            title_english: decode_utf16le_title(&b[0x340..0x440]),
            title_french: decode_utf16le_title(&b[0x440..0x540]),
            title_german: decode_utf16le_title(&b[0x540..0x640]),
            title_italian: decode_utf16le_title(&b[0x640..0x740]),
            title_spanish: decode_utf16le_title(&b[0x740..0x840]),
            icon_rgba8,
        }))
    }
}

fn decode_utf16le_title(raw: &[u8]) -> String {
    let mut units = Vec::with_capacity(raw.len() / 2);
    for pair in raw.as_chunks::<2>().0 {
        let u = u16::from_le_bytes(*pair);
        if u == 0 {
            break;
        }
        units.push(u);
    }
    String::from_utf16_lossy(&units).trim().to_string()
}

fn rgb555_to_rgba(c: u16, transparent: bool) -> [u8; 4] {
    if transparent {
        return [0, 0, 0, 0];
    }
    let r5 = (c & 0x1F) as u8;
    let g5 = ((c >> 5) & 0x1F) as u8;
    let b5 = ((c >> 10) & 0x1F) as u8;
    let r = (r5 << 3) | (r5 >> 2);
    let g = (g5 << 3) | (g5 >> 2);
    let b = (b5 << 3) | (b5 >> 2);
    [r, g, b, 255]
}

fn decode_icon_rgba8(tiles_4bpp: &[u8], pal_bytes: &[u8]) -> [u8; 32 * 32 * 4] {
    let mut palette = [[0u8; 4]; 16];
    for (idx, pair) in pal_bytes.as_chunks::<2>().0.iter().take(16).enumerate() {
        let c = u16::from_le_bytes(*pair);
        palette[idx] = rgb555_to_rgba(c, idx == 0);
    }

    let mut rgba = [0u8; 32 * 32 * 4];
    for tile_y in 0..4usize {
        for tile_x in 0..4usize {
            let tile_idx = tile_y * 4 + tile_x;
            let tile_off = tile_idx * 32;
            for py in 0..8usize {
                for px_pair in 0..4usize {
                    let byte = tiles_4bpp[tile_off + py * 4 + px_pair];
                    let p0 = usize::from(byte & 0x0F);
                    let p1 = usize::from(byte >> 4);

                    let x0 = tile_x * 8 + px_pair * 2;
                    let y = tile_y * 8 + py;
                    let out0 = (y * 32 + x0) * 4;
                    rgba[out0..out0 + 4].copy_from_slice(&palette[p0]);
                    rgba[out0 + 4..out0 + 8].copy_from_slice(&palette[p1]);
                }
            }
        }
    }
    rgba
}
