use std::path::Path;

use image::{ImageBuffer, Rgba};

use crate::bus::NitroBus;

pub const NDS_WIDTH: usize = 256;
pub const NDS_HEIGHT: usize = 192;
pub const SCREEN_RGBA_BYTES: usize = NDS_WIDTH * NDS_HEIGHT * 4;

/// Dual-Screen rendered RGBA8 framebuffers (`256×192` Top + `256×192` Bottom).
#[derive(Clone)]
pub struct DualFrame {
    pub top_rgba: Vec<u8>,
    pub bottom_rgba: Vec<u8>,
}

impl Default for DualFrame {
    fn default() -> Self {
        Self {
            top_rgba: vec![0u8; SCREEN_RGBA_BYTES],
            bottom_rgba: vec![0u8; SCREEN_RGBA_BYTES],
        }
    }
}

/// Renders both Nintendo DS 2D Engines (`Engine A` & `Engine B`) into `DualFrame`.
pub fn render_dual_screens(bus: &NitroBus, out: &mut DualFrame) {
    let mut eng_a = vec![0u8; SCREEN_RGBA_BYTES];
    let mut eng_b = vec![0u8; SCREEN_RGBA_BYTES];

    render_engine_2d(bus, false, &mut eng_a);
    render_engine_2d(bus, true, &mut eng_b);

    // POWCNT1 bit 15 (`Display Swap`): `1` = Engine A on Top, `0` = Engine B on Top
    let powcnt1 = u16::from_le_bytes([bus.io_regs[0x0304], bus.io_regs[0x0305]]);
    if (powcnt1 & 0x8000) != 0 {
        out.top_rgba.copy_from_slice(&eng_a);
        out.bottom_rgba.copy_from_slice(&eng_b);
    } else {
        out.top_rgba.copy_from_slice(&eng_b);
        out.bottom_rgba.copy_from_slice(&eng_a);
    }
}

fn render_engine_2d(bus: &NitroBus, is_engine_b: bool, rgba: &mut [u8]) {
    let io_base = if is_engine_b {
        0x1000usize
    } else {
        0x0000usize
    };
    let pal_base = if is_engine_b {
        0x0400usize
    } else {
        0x0000usize
    };
    let bg_vram_base = if is_engine_b {
        0x4_0000usize
    } else {
        0x0_0000usize
    };
    let obj_vram_base = if is_engine_b {
        0x9_0000usize
    } else {
        0x8_0000usize
    };
    let oam_base = if is_engine_b {
        0x0400usize
    } else {
        0x0000usize
    };

    let dispcnt = u32::from_le_bytes([
        bus.io_regs[io_base],
        bus.io_regs[io_base + 1],
        bus.io_regs[io_base + 2],
        bus.io_regs[io_base + 3],
    ]);

    let disp_mode = (dispcnt >> 16) & 0x03;

    // Engine A Direct VRAM Bitmap Mode (`disp_mode == 2`): `256×192` 15-bit RGB555 from VRAM Bank A..D
    if !is_engine_b && disp_mode == 2 {
        let bank = ((dispcnt >> 18) & 0x03) as usize;
        let bank_off = bank * 0x2_0000;
        for px in 0..(NDS_WIDTH * NDS_HEIGHT) {
            let v_off = bank_off + px * 2;
            let raw = u16::from_le_bytes([bus.vram[v_off], bus.vram[v_off + 1]]);
            rgba[px * 4..px * 4 + 4].copy_from_slice(&rgb555_to_rgba8(raw));
        }
        return;
    }

    // Fill backdrop with palette entry 0
    let backdrop = rgb555_to_rgba8(u16::from_le_bytes([
        bus.palette[pal_base],
        bus.palette[pal_base + 1],
    ]));
    for px in 0..(NDS_WIDTH * NDS_HEIGHT) {
        rgba[px * 4..px * 4 + 4].copy_from_slice(&backdrop);
    }

    if disp_mode == 0 {
        return;
    }

    let bg_mode = dispcnt & 0x07;

    // Render enabled BG0..BG3 back-to-front by priority (`3` down to `0`)
    for prio in (0..=3u16).rev() {
        for bg in (0..4usize).rev() {
            if ((dispcnt >> (8 + bg)) & 1) == 0 {
                continue;
            }
            let bgcnt = u16::from_le_bytes([
                bus.io_regs[io_base + 0x08 + bg * 2],
                bus.io_regs[io_base + 0x09 + bg * 2],
            ]);
            if (bgcnt & 0x03) != prio {
                continue;
            }

            // Extended Direct 15-bit Bitmap BG3 (`bg == 3` in Mode 3/5 with `bgcnt bit 7` && `bit 2` set)
            if bg == 3 && (bg_mode == 3 || bg_mode == 5) && (bgcnt & 0x0084) == 0x0084 {
                let bmp_base = bg_vram_base + usize::from((bgcnt >> 8) & 0x1F) * 0x4000;
                for px in 0..(NDS_WIDTH * NDS_HEIGHT) {
                    let off = bmp_base + px * 2;
                    if off + 2 <= bus.vram.len() {
                        let c = u16::from_le_bytes([bus.vram[off], bus.vram[off + 1]]);
                        if (c & 0x8000) != 0 {
                            rgba[px * 4..px * 4 + 4].copy_from_slice(&rgb555_to_rgba8(c));
                        }
                    }
                }
            } else {
                render_text_bg(
                    bus,
                    io_base,
                    pal_base,
                    bg_vram_base,
                    dispcnt,
                    bg,
                    bgcnt,
                    rgba,
                );
            }
        }

        // Render hardware OAM sprites (`OBJ` enabled = `dispcnt bit 12`) at this priority!
        if ((dispcnt >> 12) & 1) != 0 {
            render_oam_sprites(bus, pal_base, obj_vram_base, oam_base, dispcnt, prio, rgba);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text_bg(
    bus: &NitroBus,
    io_base: usize,
    pal_base: usize,
    bg_vram_base: usize,
    dispcnt: u32,
    bg: usize,
    bgcnt: u16,
    rgba: &mut [u8],
) {
    let is_8bpp = ((bgcnt >> 7) & 1) != 0;
    let char_base_step = if is_engine_a_base(bg_vram_base) {
        (((dispcnt >> 24) & 0x07) as usize) * 0x1_0000
    } else {
        0
    };
    let screen_base_step = if is_engine_a_base(bg_vram_base) {
        (((dispcnt >> 27) & 0x07) as usize) * 0x1_0000
    } else {
        0
    };

    let tile_base = bg_vram_base + char_base_step + usize::from((bgcnt >> 2) & 0x0F) * 0x4000;
    let map_base = bg_vram_base + screen_base_step + usize::from((bgcnt >> 8) & 0x1F) * 0x0800;

    let hofs = usize::from(
        u16::from_le_bytes([
            bus.io_regs[io_base + 0x10 + bg * 4],
            bus.io_regs[io_base + 0x11 + bg * 4],
        ]) & 0x01FF,
    );
    let vofs = usize::from(
        u16::from_le_bytes([
            bus.io_regs[io_base + 0x12 + bg * 4],
            bus.io_regs[io_base + 0x13 + bg * 4],
        ]) & 0x01FF,
    );

    for sy in 0..NDS_HEIGHT {
        let wy = (sy + vofs) & 255;
        let ty = wy / 8;
        let py_raw = wy & 7;

        for sx in 0..NDS_WIDTH {
            let wx = (sx + hofs) & 255;
            let tx = wx / 8;
            let px_raw = wx & 7;

            let map_off = map_base + (ty * 32 + tx) * 2;
            if map_off + 2 > bus.vram.len() {
                continue;
            }
            let entry = u16::from_le_bytes([bus.vram[map_off], bus.vram[map_off + 1]]);
            let tile_num = usize::from(entry & 0x03FF);
            let hflip = ((entry >> 10) & 1) != 0;
            let vflip = ((entry >> 11) & 1) != 0;
            let pal_bank = usize::from((entry >> 12) & 0x0F);

            let px = if hflip { 7 - px_raw } else { px_raw };
            let py = if vflip { 7 - py_raw } else { py_raw };

            let color_idx = if is_8bpp {
                let off = tile_base + tile_num * 64 + py * 8 + px;
                if off >= bus.vram.len() {
                    0
                } else {
                    usize::from(bus.vram[off])
                }
            } else {
                let off = tile_base + tile_num * 32 + py * 4 + (px / 2);
                if off >= bus.vram.len() {
                    0
                } else {
                    let b = bus.vram[off];
                    let nib = if (px & 1) == 0 { b & 0x0F } else { b >> 4 };
                    if nib == 0 {
                        0
                    } else {
                        pal_bank * 16 + usize::from(nib)
                    }
                }
            };

            if color_idx != 0 {
                let p_off = pal_base + color_idx * 2;
                let c = u16::from_le_bytes([bus.palette[p_off], bus.palette[p_off + 1]]);
                let out_idx = (sy * NDS_WIDTH + sx) * 4;
                rgba[out_idx..out_idx + 4].copy_from_slice(&rgb555_to_rgba8(c));
            }
        }
    }
}

fn render_oam_sprites(
    bus: &NitroBus,
    pal_base: usize,
    obj_vram_base: usize,
    oam_base: usize,
    dispcnt: u32,
    target_prio: u16,
    rgba: &mut [u8],
) {
    // Render lower OAM indices on top by iterating 127 down to 0
    let obj_pal_base = pal_base + 0x0200; // OBJ palette sits at `+0x200` (`0x0500_0200` / `0x0500_0600`)
    let one_dim = ((dispcnt >> 4) & 1) != 0;
    let boundary_shift = if one_dim {
        ((dispcnt >> 20) & 0x03) as usize
    } else {
        0
    };
    let tile_stride = 32usize << boundary_shift;

    for idx in (0..128usize).rev() {
        let off = oam_base + idx * 8;
        let attr0 = u16::from_le_bytes([bus.oam[off], bus.oam[off + 1]]);
        let obj_mode = (attr0 >> 8) & 0x03;
        if obj_mode == 0b10 {
            continue; // Sprite disabled!
        }

        let attr1 = u16::from_le_bytes([bus.oam[off + 2], bus.oam[off + 3]]);
        let attr2 = u16::from_le_bytes([bus.oam[off + 4], bus.oam[off + 5]]);
        let prio = (attr2 >> 10) & 0x03;
        if prio != target_prio {
            continue;
        }

        let shape = (attr0 >> 14) & 0x03;
        let size_code = (attr1 >> 14) & 0x03;
        let (w, h) = sprite_dimensions(shape, size_code);
        let is_8bpp = ((attr0 >> 13) & 1) != 0;

        let mut y = i32::from(attr0 & 0x00FF);
        if y >= 192 {
            y -= 256;
        }
        let mut x = i32::from(attr1 & 0x01FF);
        if x >= 256 {
            x -= 512;
        }

        let hflip = obj_mode == 0 && ((attr1 >> 12) & 1) != 0;
        let vflip = obj_mode == 0 && ((attr1 >> 13) & 1) != 0;
        let base_tile = usize::from(attr2 & 0x03FF);
        let pal_bank = usize::from((attr2 >> 12) & 0x0F);

        let tiles_x = w / 8;
        for py in 0..h {
            let sy = y + (py as i32);
            if !(0..192).contains(&sy) {
                continue;
            }
            let src_y = if vflip { h - 1 - py } else { py };
            let ty = src_y / 8;
            let in_ty = src_y & 7;

            for px in 0..w {
                let sx = x + (px as i32);
                if !(0..256).contains(&sx) {
                    continue;
                }
                let src_x = if hflip { w - 1 - px } else { px };
                let tx = src_x / 8;
                let in_tx = src_x & 7;

                let color_idx = if is_8bpp {
                    let tile_off = if one_dim {
                        base_tile * tile_stride + (ty * tiles_x + tx) * 64
                    } else {
                        (base_tile + ty * 16 + tx * 2) * 32
                    };
                    let v_off = obj_vram_base + tile_off + in_ty * 8 + in_tx;
                    if v_off >= bus.vram.len() {
                        0
                    } else {
                        usize::from(bus.vram[v_off])
                    }
                } else {
                    let tile_off = if one_dim {
                        base_tile * tile_stride + (ty * tiles_x + tx) * 32
                    } else {
                        (base_tile + ty * 32 + tx) * 32
                    };
                    let v_off = obj_vram_base + tile_off + in_ty * 4 + (in_tx / 2);
                    if v_off >= bus.vram.len() {
                        0
                    } else {
                        let b = bus.vram[v_off];
                        let nib = if (in_tx & 1) == 0 { b & 0x0F } else { b >> 4 };
                        if nib == 0 {
                            0
                        } else {
                            pal_bank * 16 + usize::from(nib)
                        }
                    }
                };

                if color_idx != 0 {
                    let p_off = (obj_pal_base + color_idx * 2) & (bus.palette.len() - 1);
                    let c = u16::from_le_bytes([bus.palette[p_off], bus.palette[p_off + 1]]);
                    let out_idx = ((sy as usize) * NDS_WIDTH + (sx as usize)) * 4;
                    rgba[out_idx..out_idx + 4].copy_from_slice(&rgb555_to_rgba8(c));
                }
            }
        }
    }
}

fn sprite_dimensions(shape: u16, size: u16) -> (usize, usize) {
    match (shape & 3, size & 3) {
        (0, 0) => (8, 8),
        (0, 1) => (16, 16),
        (0, 2) => (32, 32),
        (0, 3) => (64, 64),
        (1, 0) => (16, 8),
        (1, 1) => (32, 8),
        (1, 2) => (32, 16),
        (1, 3) => (64, 32),
        (2, 0) => (8, 16),
        (2, 1) => (8, 32),
        (2, 2) => (16, 32),
        (2, 3) => (32, 64),
        _ => (8, 8),
    }
}

#[inline]
fn is_engine_a_base(bg_vram_base: usize) -> bool {
    bg_vram_base == 0
}

#[inline]
pub fn rgb555_to_rgba8(c: u16) -> [u8; 4] {
    let r5 = (c & 0x1F) as u8;
    let g5 = ((c >> 5) & 0x1F) as u8;
    let b5 = ((c >> 10) & 0x1F) as u8;
    let r = (r5 << 3) | (r5 >> 2);
    let g = (g5 << 3) | (g5 >> 2);
    let b = (b5 << 3) | (b5 >> 2);
    [r, g, b, 255]
}

/// Exports a crisp 2× upscaled (`544 × 816`) Nintendo DS Dual-Screen Showcase PNG image.
pub fn save_dual_screen_png(frame: &DualFrame, path: &Path) -> std::io::Result<()> {
    const SCALE: u32 = 2;
    const PAD: u32 = 16;
    const HINGE: u32 = 24;
    let w = (NDS_WIDTH as u32) * SCALE + PAD * 2;
    let h = (NDS_HEIGHT as u32) * SCALE * 2 + PAD * 2 + HINGE;

    let mut img = ImageBuffer::from_pixel(w, h, Rgba([18u8, 20, 28, 255]));

    // Subtle console bezel border & hinge groove
    let hinge_y = PAD + (NDS_HEIGHT as u32) * SCALE + (HINGE / 2);
    for x in PAD..(w - PAD) {
        img.put_pixel(x, hinge_y - 1, Rgba([38, 43, 58, 255]));
        img.put_pixel(x, hinge_y, Rgba([10, 12, 18, 255]));
        img.put_pixel(x, hinge_y + 1, Rgba([32, 36, 50, 255]));
    }
    // Power LED accent on right hinge
    for dy in 0..4u32 {
        for dx in 0..8u32 {
            img.put_pixel(
                w - PAD - 12 + dx,
                hinge_y - 2 + dy,
                Rgba([46, 213, 115, 255]),
            );
        }
    }

    blit_scaled(&mut img, &frame.top_rgba, PAD, PAD, SCALE);
    blit_scaled(
        &mut img,
        &frame.bottom_rgba,
        PAD,
        PAD + (NDS_HEIGHT as u32) * SCALE + HINGE,
        SCALE,
    );

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    img.save(path)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

fn blit_scaled(
    dst: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    src_rgba: &[u8],
    ox: u32,
    oy: u32,
    scale: u32,
) {
    // 1px inner screen border
    let sw = (NDS_WIDTH as u32) * scale;
    let sh = (NDS_HEIGHT as u32) * scale;
    for x in (ox - 1)..=(ox + sw) {
        dst.put_pixel(x, oy - 1, Rgba([52, 58, 76, 255]));
        dst.put_pixel(x, oy + sh, Rgba([52, 58, 76, 255]));
    }
    for y in (oy - 1)..=(oy + sh) {
        dst.put_pixel(ox - 1, y, Rgba([52, 58, 76, 255]));
        dst.put_pixel(ox + sw, y, Rgba([52, 58, 76, 255]));
    }

    for y in 0..(NDS_HEIGHT as u32) {
        for x in 0..(NDS_WIDTH as u32) {
            let i = ((y as usize) * NDS_WIDTH + (x as usize)) * 4;
            let px = Rgba([src_rgba[i], src_rgba[i + 1], src_rgba[i + 2], 255]);
            for dy in 0..scale {
                for dx in 0..scale {
                    dst.put_pixel(ox + x * scale + dx, oy + y * scale + dy, px);
                }
            }
        }
    }
}
