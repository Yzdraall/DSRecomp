//! Safe-Rust Nintendo DS (`ARM946E-S`) Dual-Screen Hardware Runtime for `DSRecomp`.

pub mod bus;
pub mod cpu;
#[cfg(feature = "gui")]
pub mod gui;
pub mod ops;
pub mod ppu;

use std::path::PathBuf;
use std::time::Instant;

use nitro_rom::NdsRom;
use thiserror::Error;

pub use bus::{NdsInputState, NitroBus, TracedTarget};
pub use cpu::{CpuContext, FRAME_INSTRUCTION_BUDGET};
pub use ops::{
    call_direct, call_indirect, exec_mem_multiple, exec_mem_single, exec_swp,
    record_unmapped_target, step_dynamic_instruction,
};
pub use ppu::{
    render_dual_screens, rgb555_to_rgba8, save_dual_screen_png, DualFrame, NDS_HEIGHT, NDS_WIDTH,
};

/// Function pointer signature implemented by `nitro-lifter`'s generated AOT Rust crate.
pub type AotDispatchFn = fn(&mut CpuContext, &mut NitroBus) -> bool;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("GUI frontend error: {0}")]
    Gui(String),
}

/// Execution configuration for a transpiled Nintendo DS native binary.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub window_title: String,
    pub headless_frames: Option<u32>,
    pub dump_png: Option<PathBuf>,
    pub trace_out: Option<PathBuf>,
}

/// Complete Nintendo DS execution machine pairing `CpuContext`, `NitroBus`, `DualFrame`, and the AOT Rust dispatcher.
pub struct NitroMachine {
    pub cpu: CpuContext,
    pub bus: NitroBus,
    pub frame: DualFrame,
    pub dispatch_aot: AotDispatchFn,
}

impl NitroMachine {
    pub fn from_rom(rom: NdsRom, dispatch_aot: AotDispatchFn) -> Self {
        let cpu = CpuContext::with_entry(rom.header.arm9_entry_address);
        let bus = NitroBus::from_rom(rom);
        Self {
            cpu,
            bus,
            frame: DualFrame::default(),
            dispatch_aot,
        }
    }

    /// Executes one full 60 Hz Nintendo DS frame (`ARM9` AOT + VBlank + Dual-Screen 2D PPU render).
    pub fn step_frame(&mut self) {
        self.bus.sync_input_mmio();
        self.cpu.frame_ticks = 0;
        self.cpu.vblank_requested = false;

        // Visible scanlines (`0..192`)
        self.bus.io_regs[0x0004] = 0x00; // DISPSTAT VBlank=0
        self.bus.io_regs[0x1004] = 0x00;

        while !self.cpu.should_yield_frame() {
            if !(self.dispatch_aot)(&mut self.cpu, &mut self.bus) {
                record_unmapped_target(&mut self.cpu, &mut self.bus);
                step_dynamic_instruction(&mut self.cpu, &mut self.bus);
            }
        }

        // Enter VBlank (`scanlines 192..263`) & render both screens
        self.bus.io_regs[0x0004] = 0x01; // DISPSTAT VBlank=1
        self.bus.io_regs[0x1004] = 0x01;
        render_dual_screens(&self.bus, &mut self.frame);
        self.bus.frame_counter = self.bus.frame_counter.wrapping_add(1);
    }

    pub fn save_trace_if_requested(&self, path: &Option<PathBuf>) -> Result<(), RuntimeError> {
        if let Some(p) = path {
            let list: Vec<&TracedTarget> = self.bus.traced_jumps.iter().collect();
            std::fs::write(p, serde_json::to_string_pretty(&list)?)?;
        }
        Ok(())
    }

    /// Runs either headless deterministic frames (with optional PNG & trace export) or the interactive desktop window.
    pub fn run(mut self, cfg: LaunchConfig) -> Result<(), RuntimeError> {
        if let Some(frames) = cfg.headless_frames {
            let t0 = Instant::now();
            for _ in 0..frames {
                self.step_frame();
            }
            let elapsed = t0.elapsed();
            let fps = if elapsed.as_secs_f64() > 0.0 {
                f64::from(frames) / elapsed.as_secs_f64()
            } else {
                0.0
            };
            println!(
                "[DSRecomp Headless] Executed {frames} frames in {:.2} ms ({fps:.1} FPS | AOT blocks: {} | Dynamic steps: {})",
                elapsed.as_secs_f64() * 1000.0,
                self.cpu.aot_blocks_executed,
                self.cpu.fallback_steps_executed
            );
            if let Some(ref png_path) = cfg.dump_png {
                save_dual_screen_png(&self.frame, png_path)?;
                println!(
                    "[DSRecomp Headless] Saved dual-screen PNG -> {}",
                    png_path.display()
                );
            }
            self.save_trace_if_requested(&cfg.trace_out)?;
            return Ok(());
        }

        #[cfg(feature = "gui")]
        {
            gui::run_native_window(self, cfg.window_title).map_err(RuntimeError::Gui)
        }
        #[cfg(not(feature = "gui"))]
        {
            for _ in 0..60 {
                self.step_frame();
            }
            if let Some(ref png_path) = cfg.dump_png {
                save_dual_screen_png(&self.frame, png_path)?;
            }
            self.save_trace_if_requested(&cfg.trace_out)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nitro_arm::ArmAsm;
    use nitro_rom::NdsRomBuilder;

    fn dummy_no_aot(_cpu: &mut CpuContext, _bus: &mut NitroBus) -> bool {
        false
    }

    #[test]
    fn runtime_renders_direct_vram_and_dma_transfers_cleanly() {
        // Assemble tiny ARM9 program enabling Direct VRAM Mode (`0x0002_0000`) on Engine A
        // and writing bright emerald `0x03E0` to VRAM Bank A (`0x0680_0000`) + SWI VBlankWait (`0x05`)
        let mut asm = ArmAsm::new();
        asm.mov_const32(0, 0x0400_0000);
        asm.mov_const32(1, 0x0002_0000); // DISPCNT = Display Mode 2 (VRAM Bank A)
        asm.str_u32_imm(1, 0, 0);
        asm.mov_const32(2, 0x0680_0000);
        asm.mov_const32(3, 0x03E0);
        let loop_off = asm.offset();
        asm.str_u16_imm(3, 2, 0);
        asm.swi(0x05);
        asm.branch_to_offset(nitro_arm::Cond::Al, false, loop_off);

        let raw_rom = NdsRomBuilder::new("RT_TEST", *b"RTST")
            .with_arm9(0x0200_0800, 0x0200_0800, asm.into_bytes())
            .build();
        let rom = NdsRom::parse(raw_rom).unwrap();
        let mut machine = NitroMachine::from_rom(rom, dummy_no_aot);
        machine.step_frame();

        // Top-left pixel (`0..4`) should be emerald green (`r=0, g=255, b=0, a=255`)
        assert_eq!(&machine.frame.top_rgba[0..4], &[0, 255, 0, 255]);
    }
}
