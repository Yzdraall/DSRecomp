use std::path::{Path, PathBuf};

use nitro_arm::{ArmAsm, Cond, ThumbAsm};
use nitro_rom::{NdsRomBuilder, OverlaySpec};

#[derive(Debug, Clone)]
pub struct DemoSpec {
    pub slug: &'static str,
    pub filename: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
}

pub const ALL_DEMOS: [DemoSpec; 3] = [
    DemoSpec {
        slug: "plasma-dual",
        filename: "plasma_dual.nds",
        title: "Aurora Dual-Screen Demoscene",
        summary: "ARM9 + Thumb BLX interworking plasma on Top Screen + NitroFS DMA3 scrolling tilemap & 8 OAM sprites on Bottom Screen",
    },
    DemoSpec {
        slug: "mandelbrot-stylus",
        filename: "mandelbrot_stylus.nds",
        title: "Mandelbrot & Interactive Stylus Studio",
        summary: "Fixed-point Q12 ARM9 Mandelbrot fractal on Top Screen + interactive Mouse-to-Stylus 15-bit paint canvas on Bottom Screen",
    },
    DemoSpec {
        slug: "overlay-quest",
        filename: "overlay_quest.nds",
        title: "BLZ Overlay Multiplexer & Jump-Table Showcase",
        summary: "Two BLZ-compressed ARM9 Overlays sharing virtual address 0x02020000 + LDRLS PC switch jump-table running 100% AOT",
    },
];

pub fn write_all_demos(out_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let p1 = out_dir.join("plasma_dual.nds");
    let p2 = out_dir.join("mandelbrot_stylus.nds");
    let p3 = out_dir.join("overlay_quest.nds");

    std::fs::write(&p1, build_plasma_dual_rom())?;
    std::fs::write(&p2, build_mandelbrot_stylus_rom())?;
    std::fs::write(&p3, build_overlay_quest_rom())?;

    Ok(vec![p1, p2, p3])
}

/// Demo 1: `plasma_dual.nds`
/// - Top Screen: Direct VRAM Mode 2 (`256x192` RGB555) calling a Thumb pixel shader via `BLX`!
/// - Bottom Screen: Mode 0 4bpp Tilemap + 8 animated OAM Hardware Sprites + `NitroFS` palette loaded via `SWI 0xFE` & `DMA3`!
pub fn build_plasma_dual_rom() -> Vec<u8> {
    let base = 0x0200_0800u32;
    let mut arm = ArmAsm::new();

    // 1. Configure Engine A (`0x0400_0000`) = Display Mode 2 (VRAM Bank A `0x0002_0000`), enable VRAM A (`0x0400_0240 = 0x80`)
    arm.mov_const32(0, 0x0400_0000);
    arm.mov_const32(1, 0x0002_0000);
    arm.str_u32_imm(1, 0, 0);
    arm.mov_const32(2, 0x0080);
    arm.str_u16_imm(2, 0, 0xF0); // harmless warm-up

    // 2. Configure Engine B (`0x0400_1000`) = Mode 0 + BG0 + OBJ 1D (`0x0001_1110`), BG0CNT (`char_base=1`, `map_base=0`)
    arm.mov_const32(4, 0x0400_1000);
    arm.mov_const32(1, 0x0001_1110);
    arm.str_u32_imm(1, 4, 0);
    arm.mov_const32(1, 0x0004); // BG0CNT: 4bpp, tile_base = 1 (0x4000), screen_base = 0 (0x0000)
    arm.str_u16_imm(1, 4, 8);

    // 3. Load 64-byte palette file (`file_id = 0`) from NitroFS into Main RAM `0x0210_0000` via SWI `0xFE`,
    //    then copy via Hardware DMA3 (`0x0400_00D4`) to Engine B BG Palette (`0x0500_0400`) & OBJ Palette (`0x0500_0600`)!
    arm.mov_const32(0, 0);
    arm.mov_const32(1, 0x0210_0000);
    arm.mov_const32(2, 64);
    arm.swi(0xFE);

    arm.mov_const32(5, 0x0400_00D4); // DMA3SAD
    arm.mov_const32(1, 0x0210_0000);
    arm.str_u32_imm(1, 5, 0);
    arm.mov_const32(1, 0x0500_0400);
    arm.str_u32_imm(1, 5, 4);
    arm.mov_const32(1, 0x8000_0020); // Enable 16-bit DMA, 32 halfwords (64 bytes)
    arm.str_u32_imm(1, 5, 8);

    arm.mov_const32(1, 0x0210_0000);
    arm.str_u32_imm(1, 5, 0);
    arm.mov_const32(1, 0x0500_0600);
    arm.str_u32_imm(1, 5, 4);
    arm.mov_const32(1, 0x8000_0020);
    arm.str_u32_imm(1, 5, 8);

    // 4. Populate Engine B Tile 1 (neon grid cell) & Tile 2 (star cell) at `0x0620_4020`, plus 32x32 tilemap at `0x0620_0000`
    arm.mov_const32(6, 0x0620_4020);
    arm.mov_const32(1, 0x2222_2223); // top border row
    arm.str_u32_imm(1, 6, 0);
    arm.mov_const32(1, 0x1111_1113); // interior rows with left neon line
    for row in 1..8u16 {
        arm.str_u32_imm(1, 6, row * 4);
    }
    // Tile 2: glowing center star
    arm.mov_const32(6, 0x0620_4040);
    for row in 0..8u16 {
        let pat = if row == 3 || row == 4 {
            0x0045_5400
        } else if row == 2 || row == 5 {
            0x0004_4000
        } else {
            0x1111_1111
        };
        arm.mov_const32(1, pat);
        arm.str_u32_imm(1, 6, row * 4);
    }

    // Fill 32x32 tilemap (`1024` entries) alternating grid & star tiles
    arm.mov_const32(6, 0x0620_0000);
    arm.mov_const32(7, 0);
    let map_loop = arm.offset();
    arm.and_imm(1, 7, 7);
    arm.cmp_imm(1, 0);
    arm.mov_const32(2, 1);
    let skip_star = arm.offset();
    arm.emit_u32(0); // patched BNE
    arm.mov_const32(2, 2);
    let after_star = arm.offset();
    patch_cond_b(&mut arm, skip_star, Cond::Ne, after_star);
    arm.str_u16_post(2, 6, 2);
    arm.add_imm(7, 7, 1);
    arm.mov_const32(3, 1024);
    arm.cmp_reg(7, 3);
    arm.branch_to_offset(Cond::Lt, false, map_loop);

    // 5. Populate Engine B Sprite Tile 0 (`8x8` glowing diamond at `0x0660_0000`)
    arm.mov_const32(6, 0x0660_0000);
    let diamond_rows: [u32; 8] = [
        0x0006_6000,
        0x0067_7600,
        0x0673_3760,
        0x6733_3376,
        0x6733_3376,
        0x0673_3760,
        0x0067_7600,
        0x0006_6000,
    ];
    for (r, &pat) in diamond_rows.iter().enumerate() {
        arm.mov_const32(1, pat);
        arm.str_u32_imm(1, 6, (r * 4) as u16);
    }

    // ================= MAIN 60 HZ FRAME LOOP =================
    let frame_loop = arm.offset();
    arm.mov_const32(0, 0x04FF_0008);
    arm.ldr_u32_imm(8, 0, 0); // r8 = frame_counter

    // Scroll Engine B BG0 (`BG0HOFS = frame * 2`, `BG0VOFS = frame`)
    arm.mov_const32(4, 0x0400_1000);
    arm.lsl_imm(1, 8, 1);
    arm.str_u16_imm(1, 4, 0x10);
    arm.str_u16_imm(8, 4, 0x12);

    // Update 8 OAM sprites on Engine B (`0x0700_0400`)
    arm.mov_const32(6, 0x0700_0400);
    arm.mov_const32(7, 0);
    let spr_loop = arm.offset();
    // x = (idx * 30 + frame * 3) & 0xFF
    arm.mov_const32(2, 30);
    arm.mul(1, 7, 2);
    arm.add_reg(1, 1, 8);
    arm.add_reg(1, 1, 8);
    arm.and_imm(1, 1, 0xFF);
    // y = 40 + ((idx * 19 + frame * 2) & 0x7F)
    arm.mov_const32(2, 19);
    arm.mul(3, 7, 2);
    arm.add_reg(3, 3, 8);
    arm.and_imm(3, 3, 0x7F);
    arm.add_imm(3, 3, 32);

    arm.str_u16_imm(3, 6, 0); // Attr0 = Y (8x8 4bpp normal)
    arm.str_u16_imm(1, 6, 2); // Attr1 = X
    arm.mov_const32(2, 0);
    arm.str_u16_imm(2, 6, 4); // Attr2 = tile 0, prio 0
    arm.add_imm(6, 6, 8);
    arm.add_imm(7, 7, 1);
    arm.cmp_imm(7, 8);
    arm.branch_to_offset(Cond::Lt, false, spr_loop);

    // Render Top Screen (`0x0680_0000`, `256 x 192`) calling Thumb shader via `BLX`!
    arm.mov_const32(9, 0x0680_0000);
    arm.mov_const32(10, 0); // y = 0
    let y_loop = arm.offset();
    arm.mov_const32(11, 0); // x = 0
    let x_loop = arm.offset();
    arm.dp_reg(Cond::Al, 0xD, false, 0, 0, 11, 0, 0); // r0 = x
    arm.dp_reg(Cond::Al, 0xD, false, 0, 1, 10, 0, 0); // r1 = y
    arm.dp_reg(Cond::Al, 0xD, false, 0, 2, 8, 0, 0); // r2 = frame
    let blx_slot = arm.offset();
    arm.emit_u32(0); // patched BLX to Thumb shader -> returns RGB555 in r0
    arm.str_u16_post(0, 9, 2);

    arm.add_imm(11, 11, 1);
    arm.mov_const32(3, 256);
    arm.cmp_reg(11, 3);
    arm.branch_to_offset(Cond::Lt, false, x_loop);

    arm.add_imm(10, 10, 1);
    arm.cmp_imm(10, 192);
    arm.branch_to_offset(Cond::Lt, false, y_loop);

    // Wait for VBlank (`SWI 0x05`) and repeat!
    arm.swi(0x05);
    arm.branch_to_offset(Cond::Al, false, frame_loop);

    // Thumb subroutine (`thumb_plasma_pixel(x=r0, y=r1, t=r2) -> r0`):
    // Renders a synthwave sun disc + glowing horizon & shimmering water reflections!
    let thumb_off = arm.offset();
    let mut th = ThumbAsm::new();
    th.push((1 << 4) | (1 << 5) | (1 << 6), false);
    // Compute dx = x - 128 (`r4`), dy = y - 64 (`r5`), dist2 = dx*dx + dy*dy (`r6`)
    th.mov_imm(4, 128);
    th.emit_u16(0x1B04); // subs r4, r0, r4
    th.emit_u16(0x1826); // adds r6, r4, #0
    th.mul_reg(6, 4); // r6 = dx * dx
    th.mov_imm(5, 64);
    th.emit_u16(0x1B4D); // subs r5, r1, r5
    th.mul_reg(5, 5); // r5 = dy * dy
    th.add_reg3(6, 6, 5); // r6 = dx^2 + dy^2

    // Base sky/water waves: r3 = ((x + t*2) >> 3), r4 = ((y + t) ^ (x >> 2)) >> 3, r5 = (31 - (y >> 3))
    th.lsl_imm(3, 2, 1);
    th.add_reg3(3, 0, 3);
    th.lsr_imm(3, 3, 2);

    th.add_reg3(4, 1, 2);
    th.lsr_imm(5, 0, 2);
    th.eor_reg(4, 5);
    th.lsr_imm(4, 4, 2);

    th.lsr_imm(5, 1, 2);
    // Boost warm gold inside sun (`dist2 < 1024`, i.e. `r6 >> 10 == 0`)!
    th.lsr_imm(6, 6, 10);
    th.emit_u16(0x2E00); // cmp r6, #0
    th.emit_u16(0xD102); // bne +2 insts
    th.mov_imm(3, 31); // sun R = 31
    th.mov_imm(4, 24); // sun G = 24

    th.and_low_mask_31(&mut [3, 4, 5]);
    // Pack `(b << 10) | (g << 5) | r | 0x8000`
    th.lsl_imm(5, 5, 10);
    th.lsl_imm(4, 4, 5);
    th.orr_reg(3, 4);
    th.orr_reg(3, 5);
    th.mov_imm(0, 1);
    th.lsl_imm(0, 0, 15);
    th.orr_reg(0, 3);
    th.pop((1 << 4) | (1 << 5) | (1 << 6), false);
    th.bx(14); // BX LR back to ARM mode!

    let mut code = arm.into_bytes();
    code.extend_from_slice(&th.into_bytes());
    patch_blx_thumb(&mut code, blx_slot, thumb_off);

    // Build 16-color synthwave RGB555 palette for NitroFS `palette.bin`
    let pal_colors: [u16; 32] = [
        0x1842, 0x2084, 0x7C14, 0x7F1B, 0x03FF, 0x7FFF, 0x03E8, 0x7FE0, 0x7C1F, 0x02BF, 0x501F,
        0x7FFF, 0x001F, 0x03E0, 0x7C00, 0x7FFF, 0x0000, 0x1084, 0x7C1F, 0x7FFF, 0x03FF, 0x7FE0,
        0x43FF, 0x7FFF, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
    ];
    let mut pal_bytes = Vec::with_capacity(64);
    for c in pal_colors {
        pal_bytes.extend_from_slice(&c.to_le_bytes());
    }

    let _ = base;
    NdsRomBuilder::new("AURORA_DS", *b"AURA")
        .with_arm9(0x0200_0800, 0x0200_0800, code)
        .add_file("palette.bin", pal_bytes)
        .build()
}

/// Demo 2: `mandelbrot_stylus.nds`
/// - Top Screen: Real fixed-point `Q12` Mandelbrot Set computed in ARM9 assembly!
/// - Bottom Screen: Mode 5 Direct 15-bit Stylus Paint Canvas + OAM Stylus Cursor!
pub fn build_mandelbrot_stylus_rom() -> Vec<u8> {
    let mut arm = ArmAsm::new();

    // 1. Engine A = Direct VRAM Mode 2 (`0x0002_0000`)
    arm.mov_const32(0, 0x0400_0000);
    arm.mov_const32(1, 0x0002_0000);
    arm.str_u32_imm(1, 0, 0);

    // 2. Engine B = Mode 5 + BG3 Extended 15-bit Bitmap + OBJ (`0x0001_1815`)
    arm.mov_const32(4, 0x0400_1000);
    arm.mov_const32(1, 0x0001_1815);
    arm.str_u32_imm(1, 4, 0);
    // BG3CNT (`offset 0x0E`) = `0x0084` (16-bit Direct Bitmap at `0x0620_0000`)
    arm.mov_const32(1, 0x0084);
    arm.str_u16_imm(1, 4, 0x0E);

    // Set OBJ palette + crosshair sprite on Engine B
    arm.mov_const32(5, 0x0500_0602);
    arm.mov_const32(1, 0x7FFF);
    arm.str_u16_imm(1, 5, 0);
    arm.mov_const32(5, 0x0660_0000);
    let cross_rows: [u32; 8] = [
        0x0001_1000,
        0x0001_1000,
        0x0001_1000,
        0x1110_0111,
        0x1110_0111,
        0x0001_1000,
        0x0001_1000,
        0x0001_1000,
    ];
    for (i, &r) in cross_rows.iter().enumerate() {
        arm.mov_const32(1, r);
        arm.str_u32_imm(1, 5, (i * 4) as u16);
    }
    // Disable unused OAM 1..127 (`attr0 = 0x0200`)
    arm.mov_const32(5, 0x0700_0408);
    arm.mov_const32(6, 127);
    arm.mov_const32(1, 0x0200);
    let hide_loop = arm.offset();
    arm.str_u16_imm(1, 5, 0);
    arm.add_imm(5, 5, 8);
    arm.subs_imm(6, 6, 1);
    arm.branch_to_offset(Cond::Ne, false, hide_loop);

    // 3. Render Fixed-Point `Q12` (`4096 = 1.0`) Mandelbrot Set onto Top Screen (`0x0680_0000`)!
    arm.mov_const32(9, 0x0680_0000);
    arm.mov_const32(10, 0); // py = 0..192
    let mb_y = arm.offset();
    arm.mov_const32(11, 0); // px = 0..256
    let mb_x = arm.offset();

    // cr = px * 40 - 6800  (spans ~[-1.66, +0.84] in Q12)
    // ci = (py - 96) * 40  (spans ~[-0.94, +0.94] in Q12)
    arm.mov_const32(2, 40);
    arm.mul(0, 11, 2);
    arm.mov_const32(3, 6800);
    arm.sub_reg(0, 0, 3); // r0 = cr

    arm.sub_imm(1, 10, 96);
    arm.mul(1, 1, 2); // r1 = ci

    arm.mov_const32(2, 0); // zr = 0
    arm.mov_const32(3, 0); // zi = 0
    arm.mov_const32(4, 0); // iter = 0

    let iter_loop = arm.offset();
    // zr2 = (zr * zr) >> 12 (ASR #12 via signed SMULL)
    emit_q12_mul(&mut arm, 5, 2, 2); // r5 = zr^2
    emit_q12_mul(&mut arm, 6, 3, 3); // r6 = zi^2
    arm.add_reg(7, 5, 6);
    arm.mov_const32(8, 16384); // 4.0 in Q12
    arm.cmp_reg(7, 8);
    let escaped_br = arm.offset();
    arm.emit_u32(0); // BGT escaped

    // zi = ((zr * zi) >> 11) + ci
    emit_q12_mul(&mut arm, 7, 2, 3);
    arm.lsl_imm(7, 7, 1);
    arm.add_reg(3, 7, 1);
    // zr = zr2 - zi2 + cr
    arm.sub_reg(2, 5, 6);
    arm.add_reg(2, 2, 0);

    arm.add_imm(4, 4, 1);
    arm.cmp_imm(4, 24);
    arm.branch_to_offset(Cond::Lt, false, iter_loop);

    let escaped_lbl = arm.offset();
    patch_cond_b(&mut arm, escaped_br, Cond::Gt, escaped_lbl);

    // Map `iter` (`0..24`) to vibrant RGB555 color (`0x8000` inside set = deep midnight, smooth neon gradient outside)
    arm.cmp_imm(4, 24);
    arm.mov_const32(0, 0x8821);
    let write_px_br = arm.offset();
    arm.emit_u32(0); // BEQ write_px

    arm.lsl_imm(5, 4, 1);
    arm.and_imm(5, 5, 31); // R
    arm.mul(6, 4, 4);
    arm.and_imm(6, 6, 31); // G
    arm.mov_const32(7, 28);
    arm.sub_reg(7, 7, 4);
    arm.and_imm(7, 7, 31); // B
    arm.lsl_imm(6, 6, 5);
    arm.lsl_imm(7, 7, 10);
    arm.orr_reg(0, 5, 6);
    arm.orr_reg(0, 0, 7);
    arm.mov_const32(8, 0x8000);
    arm.orr_reg(0, 0, 8);

    let write_px_lbl = arm.offset();
    patch_cond_b(&mut arm, write_px_br, Cond::Eq, write_px_lbl);
    arm.str_u16_post(0, 9, 2);

    arm.add_imm(11, 11, 1);
    arm.mov_const32(8, 256);
    arm.cmp_reg(11, 8);
    arm.branch_to_offset(Cond::Lt, false, mb_x);

    arm.add_imm(10, 10, 1);
    arm.cmp_imm(10, 192);
    arm.branch_to_offset(Cond::Lt, false, mb_y);

    // 4. Initialize Bottom Touch Canvas (`0x0620_0000`) with dark blueprint grid + neon welcome wave!
    arm.mov_const32(9, 0x0620_0000);
    arm.mov_const32(10, 0);
    let grid_y = arm.offset();
    arm.mov_const32(11, 0);
    let grid_x = arm.offset();
    arm.mov_const32(0, 0x9084); // dark slate
    arm.and_imm(1, 11, 15);
    arm.cmp_imm(1, 0);
    let set_grid1 = arm.offset();
    arm.emit_u32(0);
    arm.and_imm(1, 10, 15);
    arm.cmp_imm(1, 0);
    let keep_bg = arm.offset();
    arm.emit_u32(0);
    let grid_line_lbl = arm.offset();
    patch_cond_b(&mut arm, set_grid1, Cond::Eq, grid_line_lbl);
    arm.mov_const32(0, 0x9CE7); // blueprint grid line
    let after_grid = arm.offset();
    patch_cond_b(&mut arm, keep_bg, Cond::Ne, after_grid);
    arm.str_u16_post(0, 9, 2);
    arm.add_imm(11, 11, 1);
    arm.mov_const32(8, 256);
    arm.cmp_reg(11, 8);
    arm.branch_to_offset(Cond::Lt, false, grid_x);
    arm.add_imm(10, 10, 1);
    arm.cmp_imm(10, 192);
    arm.branch_to_offset(Cond::Lt, false, grid_y);

    // Pre-draw a colorful diagonal ribbon across the touch canvas so frame 1+ looks great too
    arm.mov_const32(7, 24);
    let rib_loop = arm.offset();
    arm.lsr_imm(6, 7, 1);
    arm.add_imm(6, 6, 32); // y = 32 + x/2
    arm.lsl_imm(5, 6, 8);
    arm.add_reg(5, 5, 7);
    arm.lsl_imm(5, 5, 1);
    arm.mov_const32(9, 0x0620_0000);
    arm.add_reg(9, 9, 5);
    arm.mov_const32(0, 0xFFE6); // bright cyan-lime stroke
    arm.str_u16_imm(0, 9, 0);
    arm.str_u16_imm(0, 9, 2);
    arm.mov_const32(0, 0xFC1F); // neon magenta shadow
    arm.mov_const32(8, 512);
    arm.add_reg(9, 9, 8);
    arm.str_u16_imm(0, 9, 0);
    arm.str_u16_imm(0, 9, 2);
    arm.add_imm(7, 7, 1);
    arm.cmp_imm(7, 232);
    arm.branch_to_offset(Cond::Lt, false, rib_loop);

    // ================= INTERACTIVE STYLUS LOOP =================
    let main_loop = arm.offset();
    arm.mov_const32(4, 0x04FF_0000);
    arm.ldr_u16_imm(0, 4, 0); // r0 = stylus_x
    arm.ldr_u16_imm(1, 4, 2); // r1 = stylus_y
    arm.ldr_u16_imm(2, 4, 4); // r2 = stylus_down
    arm.ldr_u32_imm(8, 4, 8); // r8 = frame_counter

    // Update OAM 0 crosshair at `(stylus_x - 4, stylus_y - 4)`
    arm.mov_const32(5, 0x0700_0400);
    arm.sub_imm(6, 1, 4);
    arm.and_imm(6, 6, 0xFF);
    arm.str_u16_imm(6, 5, 0);
    arm.sub_imm(6, 0, 4);
    arm.mov_const32(7, 0x01FF);
    arm.dp_reg(Cond::Al, 0x0, false, 6, 6, 7, 0, 0);
    arm.str_u16_imm(6, 5, 2);

    // If stylus_down != 0, paint a glowing 2x2 brush dab at `(stylus_x, stylus_y)`!
    arm.cmp_imm(2, 0);
    let skip_paint = arm.offset();
    arm.emit_u32(0); // BEQ vblank

    arm.lsl_imm(5, 1, 8);
    arm.add_reg(5, 5, 0);
    arm.lsl_imm(5, 5, 1);
    arm.mov_const32(9, 0x0620_0000);
    arm.add_reg(9, 9, 5);
    arm.mov_const32(3, 0x83FF); // bright gold brush
    arm.eor_reg(3, 3, 8);
    arm.mov_const32(7, 0x8000);
    arm.orr_reg(3, 3, 7);
    arm.str_u16_imm(3, 9, 0);
    arm.str_u16_imm(3, 9, 2);

    let vblank_lbl = arm.offset();
    patch_cond_b(&mut arm, skip_paint, Cond::Eq, vblank_lbl);
    arm.swi(0x05);
    arm.branch_to_offset(Cond::Al, false, main_loop);

    NdsRomBuilder::new("STYLUS_LAB", *b"STYL")
        .with_arm9(0x0200_0800, 0x0200_0800, arm.into_bytes())
        .build()
}

/// Demo 3: `overlay_quest.nds`
/// - Two BLZ-compressed ARM9 Overlays (`ov_0` & `ov_1`) BOTH mapped at `0x0202_0000`!
/// - Uses an ARM `LDRLS PC, [PC, R0, LSL #2]` Jump-Table inside each overlay to render distinct dual-screen scenes!
pub fn build_overlay_quest_rom() -> Vec<u8> {
    let ov_addr = 0x0202_0000u32;
    let ov0_code = build_biome_overlay(ov_addr, false);
    let ov1_code = build_biome_overlay(ov_addr, true);

    let mut arm = ArmAsm::new();
    // Engine A = Direct VRAM Mode 2 (`0x0680_0000`), Engine B = Mode 5 Direct BG3 (`0x0620_0000`)
    arm.mov_const32(0, 0x0400_0000);
    arm.mov_const32(1, 0x0002_0000);
    arm.str_u32_imm(1, 0, 0);

    arm.mov_const32(4, 0x0400_1000);
    arm.mov_const32(1, 0x0001_0805);
    arm.str_u32_imm(1, 4, 0);
    arm.mov_const32(1, 0x0084);
    arm.str_u16_imm(1, 4, 0x0E);

    let main_loop = arm.offset();
    arm.mov_const32(4, 0x04FF_0008);
    arm.ldr_u32_imm(8, 4, 0); // r8 = frame_counter

    // Switch active overlay at `0x0202_0000` every 16 frames (`(frame >> 4) & 1`)!
    arm.lsr_imm(0, 8, 4);
    arm.and_imm(0, 0, 1);
    arm.swi(0xFF); // Load Overlay `r0` (`0` or `1`) into `0x0202_0000`!

    // Call `0x0202_0000(phase = frame & 3, frame = r8)` for all 4 jump-table bands (`0..=3`)!
    arm.mov_const32(7, 0);
    let band_loop = arm.offset();
    arm.dp_reg(Cond::Al, 0xD, false, 0, 0, 7, 0, 0); // r0 = band case (0..3)
    arm.dp_reg(Cond::Al, 0xD, false, 0, 1, 8, 0, 0); // r1 = frame
                                                     // Direct `BL 0x0202_0000` (`offset = 0x0202_0000 - 0x0200_0800 = 0x1F800`)
    arm.branch_to_offset(Cond::Al, true, 0x0001_F800);
    arm.add_imm(7, 7, 1);
    arm.cmp_imm(7, 4);
    arm.branch_to_offset(Cond::Lt, false, band_loop);

    arm.swi(0x05);
    arm.branch_to_offset(Cond::Al, false, main_loop);

    NdsRomBuilder::new("OVL_QUEST", *b"OVLQ")
        .with_arm9(0x0200_0800, 0x0200_0800, arm.into_bytes())
        .add_overlay(OverlaySpec {
            overlay_id: 0,
            ram_address: ov_addr,
            bss_size: 64,
            code: ov0_code,
            compress_blz: true,
        })
        .add_overlay(OverlaySpec {
            overlay_id: 1,
            ram_address: ov_addr,
            bss_size: 64,
            code: ov1_code,
            compress_blz: true,
        })
        .build()
}

fn build_biome_overlay(base: u32, crimson_boss: bool) -> Vec<u8> {
    let mut arm = ArmAsm::new();
    arm.push((1 << 4) | (1 << 5) | (1 << 6) | (1 << 14));
    arm.cmp_imm(0, 3);
    arm.ldrls_pc_jump_table(0);
    let default_b = arm.offset();
    arm.emit_u32(0);
    let jt_off = arm.offset();
    for _ in 0..4 {
        arm.emit_u32(0);
    }

    let palette_top: [u16; 4] = if crimson_boss {
        [0x9014, 0xA81C, 0x809F, 0x83FF]
    } else {
        [0x9882, 0xB9E4, 0x83E8, 0xFFE6]
    };
    let palette_bot: [u16; 4] = if crimson_boss {
        [0x8810, 0x9418, 0xFC12, 0xFE7F]
    } else {
        [0x9062, 0x9945, 0x835F, 0xDFFF]
    };

    let mut case_offs = [0u32; 4];
    let mut end_jumps = [0u32; 4];

    for c in 0..4usize {
        case_offs[c] = arm.offset();
        let band_bytes = (c as u32) * 48 * 256 * 2;
        // Fill 48 scanlines on Top Screen (`0x0680_0000 + band_bytes`) & Bottom Screen (`0x0620_0000 + band_bytes`)
        arm.mov_const32(2, 0x0680_0000 + band_bytes);
        arm.mov_const32(3, 0x0620_0000 + band_bytes);
        arm.mov_const32(4, 48 * 256);
        arm.mov_const32(5, u32::from(palette_top[c]));
        arm.mov_const32(6, u32::from(palette_bot[c]));

        let fill = arm.offset();
        // Patterned pixel shading `(x ^ y ^ frame)` across both screens!
        arm.and_imm(12, 4, 63);
        arm.add_reg(12, 12, 1);
        arm.and_imm(12, 12, 31);
        arm.eor_reg(12, 5, 12);
        arm.str_u16_post(12, 2, 2);
        arm.eor_reg(12, 6, 12);
        arm.mov_const32(0, 0x8000);
        arm.orr_reg(12, 12, 0);
        arm.str_u16_post(12, 3, 2);
        arm.subs_imm(4, 4, 1);
        arm.branch_to_offset(Cond::Ne, false, fill);

        end_jumps[c] = arm.offset();
        arm.emit_u32(0);
    }

    let exit_off = arm.offset();
    arm.pop((1 << 4) | (1 << 5) | (1 << 6) | (1 << 15));

    let mut bytes = arm.into_bytes();
    patch_raw_b(&mut bytes, default_b, Cond::Al, exit_off);
    for c in 0..4usize {
        let ptr = base + case_offs[c];
        let o = (jt_off as usize) + c * 4;
        bytes[o..o + 4].copy_from_slice(&ptr.to_le_bytes());
        patch_raw_b(&mut bytes, end_jumps[c], Cond::Al, exit_off);
    }
    bytes
}

/// Emits signed `Q12` fixed-point multiply: `dst = ((a * b) >> 12)` using `SMULL` + `LSR`/`ORR`.
fn emit_q12_mul(arm: &mut ArmAsm, dst: u8, a: u8, b: u8) {
    // SMULL r12 (lo), dst (hi), a, b  (`0xE0C0_0090`)
    let smull = 0xE0C0_0090
        | (u32::from(dst & 0x0F) << 16)
        | (12u32 << 12)
        | (u32::from(b & 0x0F) << 8)
        | u32::from(a & 0x0F);
    arm.emit_u32(smull);
    // dst = (r12 >> 12) | (dst << 20)
    arm.lsr_imm(12, 12, 12);
    arm.lsl_imm(dst, dst, 20);
    arm.orr_reg(dst, 12, dst);
}

trait ThumbMaskExt {
    fn and_low_mask_31(&mut self, regs: &mut [u8]);
}

impl ThumbMaskExt for ThumbAsm {
    fn and_low_mask_31(&mut self, regs: &mut [u8]) {
        self.mov_imm(2, 31);
        for &mut r in regs {
            // Thumb Format 4 `AND Rd, Rm` (`0x4000 | rm<<3 | rd`)
            self.emit_u16(0x4000 | (2u16 << 3) | u16::from(r & 7));
        }
    }
}

fn patch_cond_b(arm: &mut ArmAsm, slot_off: u32, cond: Cond, target_off: u32) {
    let mut bytes = arm.clone().into_bytes();
    patch_raw_b(&mut bytes, slot_off, cond, target_off);
    *arm = ArmAsm::default();
    for chunk in bytes.as_chunks::<4>().0 {
        arm.emit_u32(u32::from_le_bytes(*chunk));
    }
}

fn patch_raw_b(buf: &mut [u8], slot_off: u32, cond: Cond, target_off: u32) {
    let rel = (((target_off as i32) - ((slot_off as i32) + 8)) >> 2) as u32;
    let w = (u32::from(cond as u8) << 28) | (0b101 << 25) | (rel & 0x00FF_FFFF);
    let o = slot_off as usize;
    buf[o..o + 4].copy_from_slice(&w.to_le_bytes());
}

fn patch_blx_thumb(buf: &mut [u8], slot_off: u32, thumb_off: u32) {
    let diff = (thumb_off as i32) - ((slot_off as i32) + 8);
    let h = ((diff >> 1) & 1) as u32;
    let w = 0xFA00_0000 | (h << 24) | (((diff >> 2) as u32) & 0x00FF_FFFF);
    let o = slot_off as usize;
    buf[o..o + 4].copy_from_slice(&w.to_le_bytes());
}
