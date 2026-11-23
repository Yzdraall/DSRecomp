use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::error::{Result, RomError};
use crate::header::{read_u16_le, read_u32_le};

/// File Allocation Table (`FAT`) record mapping a 16-bit `file_id` to `[start_offset, end_offset)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FatEntry {
    pub file_id: u16,
    pub start_offset: u32,
    pub end_offset: u32,
}

impl FatEntry {
    pub fn size(&self) -> u32 {
        self.end_offset.saturating_sub(self.start_offset)
    }
}

/// Named file discovered inside the `NitroFS` (`FNT` + `FAT`) tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NitroFileEntry {
    pub path: String,
    pub file_id: u16,
    pub offset: u32,
    pub size: u32,
}

/// Parsed `NitroFS` virtual file system.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NitroFs {
    pub fat: Vec<FatEntry>,
    pub files: Vec<NitroFileEntry>,
}

impl NitroFs {
    pub fn parse(
        rom: &[u8],
        fnt_offset: u32,
        fnt_size: u32,
        fat_offset: u32,
        fat_size: u32,
    ) -> Result<Self> {
        let fat = parse_fat(rom, fat_offset, fat_size)?;
        if fnt_offset == 0 || fnt_size < 8 {
            return Ok(Self {
                fat,
                files: Vec::new(),
            });
        }

        let fnt_start = fnt_offset as usize;
        let fnt_end =
            fnt_start
                .checked_add(fnt_size as usize)
                .ok_or(RomError::SliceOutOfBounds {
                    context: "fnt",
                    offset: u64::from(fnt_offset),
                    size: u64::from(fnt_size),
                    rom_len: rom.len() as u64,
                })?;
        if fnt_end > rom.len() {
            return Err(RomError::SliceOutOfBounds {
                context: "fnt",
                offset: u64::from(fnt_offset),
                size: u64::from(fnt_size),
                rom_len: rom.len() as u64,
            });
        }

        let fnt = &rom[fnt_start..fnt_end];
        let total_dirs = usize::from(read_u16_le(fnt, 6));
        if total_dirs == 0 || total_dirs.saturating_mul(8) > fnt.len() {
            return Err(RomError::InvalidNitroFs(format!(
                "invalid FNT root total_dirs={total_dirs} for fnt_len={}",
                fnt.len()
            )));
        }

        let mut files = Vec::new();
        walk_fnt_dir(fnt, &fat, 0xF000, "", 0, total_dirs, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self { fat, files })
    }

    /// Reads the raw bytes of a `NitroFS` file by virtual path (case-insensitive leading `/` tolerated).
    pub fn read_file<'a>(&self, rom: &'a [u8], query_path: &str) -> Result<Option<&'a [u8]>> {
        let normalized = query_path.trim_start_matches('/').replace('\\', "/");
        let found = self
            .files
            .iter()
            .find(|f| f.path.eq_ignore_ascii_case(&normalized));
        let Some(entry) = found else {
            return Ok(None);
        };
        let start = entry.offset as usize;
        let end = start
            .checked_add(entry.size as usize)
            .ok_or(RomError::SliceOutOfBounds {
                context: "nitrofs_file",
                offset: u64::from(entry.offset),
                size: u64::from(entry.size),
                rom_len: rom.len() as u64,
            })?;
        if end > rom.len() {
            return Err(RomError::SliceOutOfBounds {
                context: "nitrofs_file",
                offset: u64::from(entry.offset),
                size: u64::from(entry.size),
                rom_len: rom.len() as u64,
            });
        }
        Ok(Some(&rom[start..end]))
    }

    /// Safely extracts matching `NitroFS` files into `dest_dir` with strict path-traversal guards.
    pub fn extract_to_dir(
        &self,
        rom: &[u8],
        dest_dir: &Path,
        prefix_filter: Option<&str>,
    ) -> Result<usize> {
        let mut extracted = 0usize;
        let clean_filter = prefix_filter.map(|p| p.trim_start_matches('/').replace('\\', "/"));

        for entry in &self.files {
            if let Some(ref pref) = clean_filter {
                if !entry.path.starts_with(pref) {
                    continue;
                }
            }
            let safe_rel = sanitize_relative_path(&entry.path)?;
            let out_path = dest_dir.join(safe_rel);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let start = entry.offset as usize;
            let end = start
                .checked_add(entry.size as usize)
                .ok_or(RomError::SliceOutOfBounds {
                    context: "nitrofs_extract",
                    offset: u64::from(entry.offset),
                    size: u64::from(entry.size),
                    rom_len: rom.len() as u64,
                })?;
            if end > rom.len() {
                return Err(RomError::SliceOutOfBounds {
                    context: "nitrofs_extract",
                    offset: u64::from(entry.offset),
                    size: u64::from(entry.size),
                    rom_len: rom.len() as u64,
                });
            }
            std::fs::write(&out_path, &rom[start..end])?;
            extracted += 1;
        }

        Ok(extracted)
    }
}

pub fn parse_fat(rom: &[u8], fat_offset: u32, fat_size: u32) -> Result<Vec<FatEntry>> {
    if fat_offset == 0 || fat_size == 0 {
        return Ok(Vec::new());
    }
    let start = fat_offset as usize;
    let end = start
        .checked_add(fat_size as usize)
        .ok_or(RomError::SliceOutOfBounds {
            context: "fat",
            offset: u64::from(fat_offset),
            size: u64::from(fat_size),
            rom_len: rom.len() as u64,
        })?;
    if end > rom.len() {
        return Err(RomError::SliceOutOfBounds {
            context: "fat",
            offset: u64::from(fat_offset),
            size: u64::from(fat_size),
            rom_len: rom.len() as u64,
        });
    }

    let slice = &rom[start..end];
    let mut entries = Vec::with_capacity(slice.len() / 8);
    for (idx, rec) in slice.as_chunks::<8>().0.iter().enumerate() {
        entries.push(FatEntry {
            file_id: idx as u16,
            start_offset: read_u32_le(rec, 0),
            end_offset: read_u32_le(rec, 4),
        });
    }
    Ok(entries)
}

fn walk_fnt_dir(
    fnt: &[u8],
    fat: &[FatEntry],
    dir_id: u16,
    prefix: &str,
    depth: usize,
    total_dirs: usize,
    out: &mut Vec<NitroFileEntry>,
) -> Result<()> {
    if depth > 32 {
        return Err(RomError::InvalidNitroFs(
            "FNT directory recursion depth exceeded 32".to_string(),
        ));
    }
    let dir_idx = usize::from(dir_id & 0x0FFF);
    if dir_idx >= total_dirs {
        return Err(RomError::InvalidNitroFs(format!(
            "FNT directory index {dir_idx} >= total_dirs {total_dirs}"
        )));
    }

    let main_off = dir_idx * 8;
    let sub_off = read_u32_le(fnt, main_off) as usize;
    let mut cur_file_id = read_u16_le(fnt, main_off + 4);

    if sub_off >= fnt.len() {
        return Err(RomError::InvalidNitroFs(format!(
            "FNT subtable offset 0x{sub_off:X} out of bounds"
        )));
    }

    let mut p = sub_off;
    while p < fnt.len() {
        let type_len = fnt[p];
        p += 1;
        if type_len == 0 {
            break;
        }
        let is_subdir = (type_len & 0x80) != 0;
        let name_len = usize::from(type_len & 0x7F);
        if p + name_len > fnt.len() {
            return Err(RomError::InvalidNitroFs(
                "FNT entry name truncated".to_string(),
            ));
        }
        let name = String::from_utf8_lossy(&fnt[p..p + name_len]).to_string();
        p += name_len;

        let child_path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };

        if is_subdir {
            if p + 2 > fnt.len() {
                return Err(RomError::InvalidNitroFs(
                    "FNT subdir ID truncated".to_string(),
                ));
            }
            let sub_dir_id = read_u16_le(fnt, p);
            p += 2;
            walk_fnt_dir(
                fnt,
                fat,
                sub_dir_id,
                &child_path,
                depth + 1,
                total_dirs,
                out,
            )?;
        } else {
            let fid = cur_file_id;
            cur_file_id = cur_file_id.wrapping_add(1);
            if let Some(fe) = fat.get(usize::from(fid)) {
                out.push(NitroFileEntry {
                    path: child_path,
                    file_id: fid,
                    offset: fe.start_offset,
                    size: fe.size(),
                });
            }
        }
    }

    Ok(())
}

fn sanitize_relative_path(raw: &str) -> Result<PathBuf> {
    let candidate = Path::new(raw);
    let mut clean = PathBuf::new();
    for comp in candidate.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if s.is_empty() || s.contains(':') {
                    return Err(RomError::UnsafePath(raw.to_string()));
                }
                clean.push(seg);
            }
            _ => return Err(RomError::UnsafePath(raw.to_string())),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(RomError::UnsafePath(raw.to_string()));
    }
    Ok(clean)
}
