//! Ahead-of-Time Nintendo DS (`ARM9`/`Thumb` + `Overlays`) to `#![forbid(unsafe_code)]` Rust transpiler.

pub mod codegen;
pub mod project;

pub use codegen::{emit_module_rust, GeneratedModuleFiles};
pub use project::{
    transpile_rom_to_project, LifterError, RecompManifest, Result, SeedEntry, TranspileOptions,
};
