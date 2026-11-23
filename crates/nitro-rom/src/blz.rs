use serde::Serialize;

use crate::error::{Result, RomError};
use crate::header::read_u32_le;

/// Magic signature following `_start_ModuleParams` inside NitroSDK ARM9 binaries:
/// `0xDEC00621` LE followed by `0x2106C0DE` LE.
pub const MODULE_PARAMS_MAGIC: [u8; 8] = [0x21, 0x06, 0xC0, 0xDE, 0xDE, 0xC0, 0x06, 0x21];

const MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

/// NitroSDK `_start_ModuleParams` metadata embedded inside `arm9.bin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModuleParams {
    pub offset_in_arm9: usize,
    pub autoload_list_start: u32,
    pub autoload_list_end: u32,
    pub autoload_start: u32,
    pub static_bss_start: u32,
    pub static_bss_end: u32,
    pub compressed_static_end: u32,
    pub sdk_version: u32,
}

impl ModuleParams {
    /// Scans an `arm9.bin` image for the NitroSDK `_start_ModuleParams` structure.
    pub fn find_in_arm9(arm9: &[u8]) -> Option<Self> {
        let search_limit = arm9.len().min(0x8000);
        if search_limit < 36 {
            return None;
        }
        let mut pos = 28usize;
        while pos + 8 <= search_limit {
            if arm9[pos..pos + 8] == MODULE_PARAMS_MAGIC {
                let base = pos - 28;
                return Some(Self {
                    offset_in_arm9: base,
                    autoload_list_start: read_u32_le(arm9, base),
                    autoload_list_end: read_u32_le(arm9, base + 4),
                    autoload_start: read_u32_le(arm9, base + 8),
                    static_bss_start: read_u32_le(arm9, base + 12),
                    static_bss_end: read_u32_le(arm9, base + 16),
                    compressed_static_end: read_u32_le(arm9, base + 20),
                    sdk_version: read_u32_le(arm9, base + 24),
                });
            }
            pos += 4;
        }
        None
    }

    pub fn sdk_major_minor(&self) -> (u8, u8) {
        let major = ((self.sdk_version >> 24) & 0xFF) as u8;
        let minor = ((self.sdk_version >> 16) & 0xFF) as u8;
        (major, minor)
    }
}

/// Decompresses a Nintendo DS Backward LZ77 (`BLZ` / `MIi_UncompressBackward`) buffer.
///
/// Preserves any uncompressed prefix (`0 .. buf.len() - comp_len`), such as the NitroSDK
/// CRT0 bootstrap region at the start of `arm9.bin`.
pub fn blz_decompress(buf: &[u8]) -> Result<Vec<u8>> {
    let end = buf.len();
    if end < 8 {
        return Err(RomError::InvalidBlz(
            "buffer shorter than 8-byte BLZ footer",
        ));
    }

    let extra_len = read_u32_le(buf, end - 4) as usize;
    if extra_len == 0 {
        return Ok(buf.to_vec());
    }

    let len_and_hdr = read_u32_le(buf, end - 8);
    let hdr_len = ((len_and_hdr >> 24) & 0xFF) as usize;
    let comp_len = (len_and_hdr & 0x00FF_FFFF) as usize;

    if !(8..=0x20).contains(&hdr_len) || comp_len < hdr_len || comp_len > end {
        return Err(RomError::InvalidBlz(
            "invalid BLZ header_len or compressed_len",
        ));
    }

    let out_len = end
        .checked_add(extra_len)
        .ok_or(RomError::InvalidBlz("BLZ output length overflow"))?;
    if out_len > MAX_DECOMPRESSED_BYTES {
        return Err(RomError::InvalidBlz("BLZ output exceeds 16 MiB safety cap"));
    }

    let prefix_len = end - comp_len;
    let mut out = vec![0u8; out_len];
    out[..prefix_len].copy_from_slice(&buf[..prefix_len]);

    let mut src = end - hdr_len;
    let mut dst = out_len;

    while dst > prefix_len {
        if src <= prefix_len {
            return Err(RomError::InvalidBlz(
                "unexpected end of BLZ compressed input",
            ));
        }
        src -= 1;
        let mut flags = buf[src];

        for _ in 0..8 {
            if dst <= prefix_len {
                break;
            }
            if (flags & 0x80) == 0 {
                if src <= prefix_len {
                    return Err(RomError::InvalidBlz("truncated BLZ literal byte"));
                }
                src -= 1;
                dst -= 1;
                out[dst] = buf[src];
            } else {
                if src < prefix_len + 2 {
                    return Err(RomError::InvalidBlz("truncated BLZ back-reference pair"));
                }
                src -= 1;
                let a = usize::from(buf[src]);
                src -= 1;
                let b = usize::from(buf[src]);

                let count = (a >> 4) + 3;
                let disp_plus_2 = (((a & 0x0F) << 8) | b) + 2;

                for _ in 0..count {
                    let ref_idx = dst
                        .checked_add(disp_plus_2)
                        .ok_or(RomError::InvalidBlz("BLZ reference index overflow"))?;
                    if ref_idx >= out_len || dst <= prefix_len {
                        return Err(RomError::InvalidBlz(
                            "BLZ back-reference out of decompressed bounds",
                        ));
                    }
                    let byte = out[ref_idx];
                    dst -= 1;
                    out[dst] = byte;
                }
            }
            flags <<= 1;
        }
    }

    Ok(out)
}

/// Decompresses `arm9.bin` if `_start_ModuleParams.compressed_static_end` indicates BLZ compression.
/// Returns `(decompressed_bytes, was_compressed, Option<ModuleParams>)`.
pub fn decode_arm9_binary(
    arm9_raw: &[u8],
    arm9_ram_address: u32,
) -> Result<(Vec<u8>, bool, Option<ModuleParams>)> {
    let params = ModuleParams::find_in_arm9(arm9_raw);
    if let Some(mp) = params {
        if mp.compressed_static_end > arm9_ram_address {
            let comp_end = (mp.compressed_static_end - arm9_ram_address) as usize;
            if comp_end <= arm9_raw.len() && comp_end >= 8 {
                let mut decoded = blz_decompress(&arm9_raw[..comp_end])?;
                // Append any trailing uncompressed autoload tail if present beyond comp_end
                if comp_end < arm9_raw.len() {
                    decoded.extend_from_slice(&arm9_raw[comp_end..]);
                }
                // Match NitroSDK crt0 behavior: clear compressed_static_end in RAM after decompression
                let cse_off = mp.offset_in_arm9 + 20;
                if cse_off + 4 <= decoded.len() {
                    decoded[cse_off..cse_off + 4].copy_from_slice(&0u32.to_le_bytes());
                }
                return Ok((decoded, true, Some(mp)));
            }
        }
    }
    Ok((arm9_raw.to_vec(), false, params))
}

/// Compresses a buffer using valid Nintendo DS Backward LZ77 (`BLZ`) framing (`prefix_len = 0`).
/// Useful for unit testing and assembling authentic compressed overlays in clean-room demos.
pub fn blz_compress(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 16 {
        return raw.to_vec();
    }

    // Build tokens walking backwards from the end of `raw` to index 0.
    enum Token {
        Lit(u8),
        Ref { count: usize, raw_disp: usize },
    }

    let mut tokens = Vec::new();
    let mut pos = raw.len();
    while pos > 0 {
        let mut best_len = 0usize;
        let mut best_raw_disp = 0usize;

        let max_len = 18.min(pos);
        if max_len >= 3 {
            // At start of match, current dst is `pos`. For the first matched byte at `pos - 1`,
            // `ref_idx = pos + raw_disp + 2`, so `raw_disp = ref_idx - pos - 2`.
            // Since `ref_idx >= pos + 2`, `raw_disp` ranges over `0..=0xFFF`.
            let max_ref = raw.len().min(pos + 2 + 0xFFF + 1);
            for ref_idx in (pos + 2)..max_ref {
                let mut m = 0usize;
                while m < max_len && raw[pos - 1 - m] == raw[ref_idx - m] {
                    m += 1;
                }
                if m >= 3 && m > best_len {
                    best_len = m;
                    best_raw_disp = ref_idx - pos - 2;
                    if best_len == 18 {
                        break;
                    }
                }
            }
        }

        if best_len >= 3 {
            tokens.push(Token::Ref {
                count: best_len,
                raw_disp: best_raw_disp,
            });
            pos -= best_len;
        } else {
            pos -= 1;
            tokens.push(Token::Lit(raw[pos]));
        }
    }

    // Emit reversed stream (will be reversed back so the decoder reads it from the end).
    let mut rev_payload = Vec::new();
    for group in tokens.chunks(8) {
        let mut flags = 0u8;
        let flag_idx = rev_payload.len();
        rev_payload.push(0);
        for (i, tok) in group.iter().enumerate() {
            let bit = 7 - i;
            match *tok {
                Token::Lit(b) => {
                    rev_payload.push(b);
                }
                Token::Ref { count, raw_disp } => {
                    flags |= 1 << bit;
                    let a = (((count - 3) << 4) | ((raw_disp >> 8) & 0x0F)) as u8;
                    let b = (raw_disp & 0xFF) as u8;
                    rev_payload.push(a);
                    rev_payload.push(b);
                }
            }
        }
        rev_payload[flag_idx] = flags;
    }

    rev_payload.reverse();
    let pad = (4 - (rev_payload.len() % 4)) % 4;
    let hdr_len = 8 + pad;
    let total_comp_len = rev_payload.len() + hdr_len;

    // Only emit BLZ if strictly smaller than raw input (guarantees extra_len > 0).
    if total_comp_len >= raw.len() {
        return raw.to_vec();
    }

    let extra_len = (raw.len() - total_comp_len) as u32;
    let len_and_hdr = ((hdr_len as u32) << 24) | (total_comp_len as u32);

    let mut out = rev_payload;
    out.resize(out.len() + pad, 0xFF);
    out.extend_from_slice(&len_and_hdr.to_le_bytes());
    out.extend_from_slice(&extra_len.to_le_bytes());
    out
}
