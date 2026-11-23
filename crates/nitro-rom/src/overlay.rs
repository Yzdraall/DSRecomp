use serde::Serialize;

use crate::blz::blz_decompress;
use crate::error::{Result, RomError};
use crate::header::read_u32_le;
use crate::nitrofs::FatEntry;

pub const OVERLAY_ENTRY_SIZE: usize = 32;

/// Parsed 32-byte Nintendo DS Overlay Table (`OVT`) record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlayEntry {
    pub overlay_id: u32,
    pub ram_address: u32,
    pub ram_size: u32,
    pub bss_size: u32,
    pub static_init_start: u32,
    pub static_init_end: u32,
    pub file_id: u32,
    pub compressed_size: u32,
    pub flags: u8,
    pub is_blz_compressed: bool,
}

impl OverlayEntry {
    pub fn parse_table(rom: &[u8], offset: u32, size: u32) -> Result<Vec<Self>> {
        if offset == 0 || size == 0 {
            return Ok(Vec::new());
        }
        let start = offset as usize;
        let len = size as usize;
        let end = start.checked_add(len).ok_or(RomError::SliceOutOfBounds {
            context: "overlay_table",
            offset: u64::from(offset),
            size: u64::from(size),
            rom_len: rom.len() as u64,
        })?;
        if end > rom.len() {
            return Err(RomError::SliceOutOfBounds {
                context: "overlay_table",
                offset: u64::from(offset),
                size: u64::from(size),
                rom_len: rom.len() as u64,
            });
        }

        let raw = &rom[start..end];
        let mut entries = Vec::with_capacity(raw.len() / OVERLAY_ENTRY_SIZE);
        for rec in raw.as_chunks::<OVERLAY_ENTRY_SIZE>().0 {
            let word7 = read_u32_le(rec, 0x1C);
            let compressed_size = word7 & 0x00FF_FFFF;
            let flags = ((word7 >> 24) & 0xFF) as u8;
            entries.push(Self {
                overlay_id: read_u32_le(rec, 0x00),
                ram_address: read_u32_le(rec, 0x04),
                ram_size: read_u32_le(rec, 0x08),
                bss_size: read_u32_le(rec, 0x0C),
                static_init_start: read_u32_le(rec, 0x10),
                static_init_end: read_u32_le(rec, 0x14),
                file_id: read_u32_le(rec, 0x18),
                compressed_size,
                flags,
                is_blz_compressed: (flags & 0x01) != 0,
            });
        }
        Ok(entries)
    }

    /// Extracts and (if flagged) BLZ-decompresses this overlay's executable image from the ROM.
    pub fn load_code(&self, rom: &[u8], fat: &[FatEntry]) -> Result<Vec<u8>> {
        let fid = self.file_id as usize;
        let fat_entry = fat.get(fid).ok_or(RomError::SliceOutOfBounds {
            context: "overlay_fat_id",
            offset: u64::from(self.file_id),
            size: 1,
            rom_len: fat.len() as u64,
        })?;

        let start = fat_entry.start_offset as usize;
        let end = fat_entry.end_offset as usize;
        if start > end || end > rom.len() {
            return Err(RomError::SliceOutOfBounds {
                context: "overlay_fat_data",
                offset: u64::from(fat_entry.start_offset),
                size: u64::from(fat_entry.end_offset.saturating_sub(fat_entry.start_offset)),
                rom_len: rom.len() as u64,
            });
        }

        let raw_slice = &rom[start..end];
        if self.is_blz_compressed && !raw_slice.is_empty() {
            let comp_end =
                if self.compressed_size > 0 && (self.compressed_size as usize) <= raw_slice.len() {
                    self.compressed_size as usize
                } else {
                    raw_slice.len()
                };
            blz_decompress(&raw_slice[..comp_end])
        } else {
            Ok(raw_slice.to_vec())
        }
    }
}
