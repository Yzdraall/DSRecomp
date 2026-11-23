//! Pure-Rust Nintendo DS (`.nds`) cartridge parser, Backward LZ77 (`BLZ`) codec,
//! dynamic Overlay table loader, `NitroFS` virtual filesystem, and clean-room ROM builder.

pub mod banner;
pub mod blz;
pub mod builder;
pub mod error;
pub mod header;
pub mod nitrofs;
pub mod overlay;

use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub use banner::NdsBanner;
pub use blz::{blz_compress, blz_decompress, decode_arm9_binary, ModuleParams};
pub use builder::{NdsRomBuilder, OverlaySpec};
pub use error::{Result, RomError};
pub use header::{crc16_nds, NdsHeader, UnitCode};
pub use nitrofs::{FatEntry, NitroFileEntry, NitroFs};
pub use overlay::OverlayEntry;

/// High-level parsed representation of a Nintendo DS cartridge image.
#[derive(Debug, Clone)]
pub struct NdsRom {
    pub raw: Vec<u8>,
    pub sha256_hex: String,
    pub header: NdsHeader,
    pub banner: Option<NdsBanner>,
    pub module_params: Option<ModuleParams>,
    pub arm9_was_blz_compressed: bool,
    pub arm9_code: Vec<u8>,
    pub arm7_code: Vec<u8>,
    pub arm9_overlays: Vec<OverlayEntry>,
    pub arm7_overlays: Vec<OverlayEntry>,
    pub nitrofs: NitroFs,
}

/// Serializable inspection summary for CLI & JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct RomSummary {
    pub file_size_bytes: usize,
    pub sha256_hex: String,
    pub header: NdsHeader,
    pub banner_title_english: Option<String>,
    pub banner_crc16_valid: Option<bool>,
    pub module_params: Option<ModuleParams>,
    pub arm9_was_blz_compressed: bool,
    pub arm9_raw_size: u32,
    pub arm9_decompressed_size: usize,
    pub arm9_overlay_count: usize,
    pub arm7_overlay_count: usize,
    pub nitrofs_file_count: usize,
}

impl NdsRom {
    pub fn from_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::parse(bytes)
    }

    pub fn parse(raw: Vec<u8>) -> Result<Self> {
        let header = NdsHeader::parse(&raw)?;
        let sha256_hex = hex::encode(Sha256::digest(&raw));

        let arm9_raw = slice_rom(
            &raw,
            header.arm9_rom_offset,
            header.arm9_size,
            "arm9_binary",
        )?;
        let (arm9_code, arm9_was_blz_compressed, module_params) =
            decode_arm9_binary(arm9_raw, header.arm9_ram_address)?;

        let arm7_code = slice_rom(
            &raw,
            header.arm7_rom_offset,
            header.arm7_size,
            "arm7_binary",
        )?
        .to_vec();

        let banner = NdsBanner::parse(&raw, header.icon_banner_offset)?;
        let nitrofs = NitroFs::parse(
            &raw,
            header.fnt_offset,
            header.fnt_size,
            header.fat_offset,
            header.fat_size,
        )?;
        let arm9_overlays =
            OverlayEntry::parse_table(&raw, header.arm9_overlay_offset, header.arm9_overlay_size)?;
        let arm7_overlays =
            OverlayEntry::parse_table(&raw, header.arm7_overlay_offset, header.arm7_overlay_size)?;

        Ok(Self {
            raw,
            sha256_hex,
            header,
            banner,
            module_params,
            arm9_was_blz_compressed,
            arm9_code,
            arm7_code,
            arm9_overlays,
            arm7_overlays,
            nitrofs,
        })
    }

    pub fn load_arm9_overlay_code(
        &self,
        overlay_id: u32,
    ) -> Result<Option<(OverlayEntry, Vec<u8>)>> {
        let Some(entry) = self
            .arm9_overlays
            .iter()
            .find(|o| o.overlay_id == overlay_id)
            .cloned()
        else {
            return Ok(None);
        };
        let code = entry.load_code(&self.raw, &self.nitrofs.fat)?;
        Ok(Some((entry, code)))
    }

    pub fn summary(&self) -> RomSummary {
        RomSummary {
            file_size_bytes: self.raw.len(),
            sha256_hex: self.sha256_hex.clone(),
            header: self.header.clone(),
            banner_title_english: self.banner.as_ref().map(|b| b.title_english.clone()),
            banner_crc16_valid: self.banner.as_ref().map(|b| b.crc16_valid),
            module_params: self.module_params,
            arm9_was_blz_compressed: self.arm9_was_blz_compressed,
            arm9_raw_size: self.header.arm9_size,
            arm9_decompressed_size: self.arm9_code.len(),
            arm9_overlay_count: self.arm9_overlays.len(),
            arm7_overlay_count: self.arm7_overlays.len(),
            nitrofs_file_count: self.nitrofs.files.len(),
        }
    }
}

fn slice_rom<'a>(rom: &'a [u8], offset: u32, size: u32, context: &'static str) -> Result<&'a [u8]> {
    let start = offset as usize;
    let end = start
        .checked_add(size as usize)
        .ok_or(RomError::SliceOutOfBounds {
            context,
            offset: u64::from(offset),
            size: u64::from(size),
            rom_len: rom.len() as u64,
        })?;
    if end > rom.len() {
        return Err(RomError::SliceOutOfBounds {
            context,
            offset: u64::from(offset),
            size: u64::from(size),
            rom_len: rom.len() as u64,
        });
    }
    Ok(&rom[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blz_roundtrip_compresses_and_decompresses_accurately() {
        let mut sample = Vec::with_capacity(2048);
        for i in 0..512u32 {
            sample.extend_from_slice(&(0xE3A0_0000u32 | (i & 0x0F)).to_le_bytes());
        }
        let compressed = blz_compress(&sample);
        assert!(compressed.len() < sample.len());
        let restored = blz_decompress(&compressed).expect("BLZ decompress failed");
        assert_eq!(restored, sample);
    }

    #[test]
    fn builder_emits_valid_nds_with_crc16_overlays_and_nitrofs() {
        let ov_code = [0x1Eu8, 0xFF, 0x2F, 0xE1].repeat(64); // repeated `bx lr`
        let raw_rom = NdsRomBuilder::new("UNIT_TEST", *b"UTST")
            .with_arm9(
                0x0200_0800,
                0x0200_0800,
                [0x00u8, 0x00, 0xA0, 0xE1].repeat(16),
            )
            .add_overlay(OverlaySpec {
                overlay_id: 0,
                ram_address: 0x0202_0000,
                bss_size: 128,
                code: ov_code.clone(),
                compress_blz: true,
            })
            .add_file("palette.bin", vec![1, 2, 3, 4, 5, 6, 7, 8])
            .add_file("level_01.dat", b"HELLO_NITROFS".to_vec())
            .build();

        let parsed = NdsRom::parse(raw_rom).expect("parse built ROM");
        assert!(parsed.header.header_crc16_valid);
        assert_eq!(parsed.header.game_title, "UNIT_TEST");
        assert_eq!(parsed.header.game_code, "UTST");
        assert_eq!(parsed.banner.as_ref().map(|b| b.crc16_valid), Some(true));
        assert_eq!(parsed.arm9_overlays.len(), 1);
        assert!(parsed.arm9_overlays[0].is_blz_compressed);

        let (_, loaded_ov) = parsed
            .load_arm9_overlay_code(0)
            .expect("load overlay")
            .expect("overlay 0 exists");
        assert_eq!(loaded_ov, ov_code);

        assert_eq!(parsed.nitrofs.files.len(), 2);
        let level = parsed
            .nitrofs
            .read_file(&parsed.raw, "level_01.dat")
            .expect("read nitrofs")
            .expect("file exists");
        assert_eq!(level, b"HELLO_NITROFS");

        let tmp = tempfile::tempdir().unwrap();
        let count = parsed
            .nitrofs
            .extract_to_dir(&parsed.raw, tmp.path(), None)
            .expect("extract");
        assert_eq!(count, 2);
        assert!(tmp.path().join("palette.bin").is_file());
    }

    #[test]
    fn optional_external_nds_rom_smoke_test() {
        let Ok(path) = std::env::var("DSRECOMP_TEST_ROM") else {
            return;
        };
        let p = Path::new(&path);
        if !p.is_file() {
            return;
        }
        let rom = NdsRom::from_file(p).expect("parse external BYO-ROM");
        assert!(rom.header.header_crc16_valid);
        assert!(rom.arm9_code.len() >= rom.header.arm9_size as usize);
        println!(
            "[BYO-ROM OK] title={} code={} blz={} arm9_raw={} arm9_dec={} overlays={} files={}",
            rom.header.game_title,
            rom.header.game_code,
            rom.arm9_was_blz_compressed,
            rom.header.arm9_size,
            rom.arm9_code.len(),
            rom.arm9_overlays.len(),
            rom.nitrofs.files.len()
        );
        for ov in &rom.arm9_overlays {
            let (_, dec) = rom
                .load_arm9_overlay_code(ov.overlay_id)
                .expect("decompress overlay")
                .expect("overlay present");
            assert_eq!(dec.len(), ov.ram_size as usize);
        }
    }
}
