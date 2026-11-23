use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use nitro_arm::{CfgConfig, CpuMode, ModuleCfg};
use nitro_rom::NdsRom;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::codegen::emit_module_rust;

#[derive(Debug, Error)]
pub enum LifterError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ROM error: {0}")]
    Rom(#[from] nitro_rom::RomError),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, LifterError>;

/// Seed entry recorded in `recomp_manifest.json` or `trace_targets.json` for trace-guided healing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SeedEntry {
    pub overlay_id: Option<u32>,
    pub addr: u32,
    pub mode: CpuMode,
}

/// Persistent manifest written into a transpiled Cargo project (`recomp_manifest.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecompManifest {
    pub game_title: String,
    pub game_code: String,
    pub rom_sha256: String,
    pub crate_name: String,
    pub arm9_entry_address: u32,
    pub arm9_ram_address: u32,
    pub static_functions: usize,
    pub static_blocks: usize,
    pub overlay_count: usize,
    pub overlay_functions: usize,
    pub jump_tables_resolved: usize,
    pub interworking_calls: usize,
    pub healed_seeds: Vec<SeedEntry>,
}

/// Options for transpiling an `.nds` ROM into a standalone safe-Rust Cargo project.
#[derive(Debug, Clone)]
pub struct TranspileOptions {
    pub out_dir: PathBuf,
    pub runtime_crate_path: PathBuf,
    pub rom_crate_path: PathBuf,
    pub scan_prologues: bool,
    pub extra_seeds: Vec<SeedEntry>,
}

/// Transpiles `rom` (static `arm9.bin` + all `ARM9 Overlays`) into a compilable `#![forbid(unsafe_code)]` Rust project.
pub fn transpile_rom_to_project(
    rom: &NdsRom,
    rom_source_path: &Path,
    opts: &TranspileOptions,
) -> Result<RecompManifest> {
    let crate_name = sanitize_crate_name(&rom.header.game_title, &rom.header.game_code);
    let src_dir = opts.out_dir.join("src");
    let gen_dir = src_dir.join("generated");
    if gen_dir.exists() {
        std::fs::remove_dir_all(&gen_dir)?;
    }
    std::fs::create_dir_all(&gen_dir)?;

    // Group extra seeds by `overlay_id`
    let mut seeds_by_ov: BTreeMap<Option<u32>, Vec<(u32, CpuMode)>> = BTreeMap::new();
    for s in &opts.extra_seeds {
        seeds_by_ov
            .entry(s.overlay_id)
            .or_default()
            .push((s.addr, s.mode));
    }

    // 1. Analyze & emit static `arm9.bin`
    let (entry_addr, entry_mode) = CpuMode::from_interworking_addr(rom.header.arm9_entry_address);
    let arm9_cfg = ModuleCfg::analyze(
        "static_arm9",
        &rom.arm9_code,
        rom.header.arm9_ram_address,
        Some((entry_addr, entry_mode)),
        &CfgConfig {
            extra_seeds: seeds_by_ov.remove(&None).unwrap_or_default(),
            scan_prologues: opts.scan_prologues,
        },
    );
    let static_emitted = emit_module_rust("static_arm9", &arm9_cfg);
    for (fname, content) in &static_emitted.files {
        std::fs::write(gen_dir.join(fname), content)?;
    }

    // 2. Analyze & emit all ARM9 dynamic Overlays
    let mut overlay_metas = Vec::new();
    let mut total_ov_fns = 0usize;
    let mut total_jts = arm9_cfg.jump_tables.len();
    let mut total_interworking = arm9_cfg.interworking_transitions;

    for ov in &rom.arm9_overlays {
        let code = ov.load_code(&rom.raw, &rom.nitrofs.fat)?;
        let mut ov_seeds = seeds_by_ov.remove(&Some(ov.overlay_id)).unwrap_or_default();
        // Seed overlay base entry + C++ static initializer array if present
        if !code.is_empty() {
            ov_seeds.push((ov.ram_address, CpuMode::Arm));
        }
        if ov.static_init_start >= ov.ram_address && ov.static_init_end > ov.static_init_start {
            let mut p = ov.static_init_start;
            while p + 4 <= ov.static_init_end {
                let off = (p - ov.ram_address) as usize;
                if off + 4 <= code.len() {
                    let fn_ptr = u32::from_le_bytes([
                        code[off],
                        code[off + 1],
                        code[off + 2],
                        code[off + 3],
                    ]);
                    if fn_ptr != 0 {
                        ov_seeds.push(CpuMode::from_interworking_addr(fn_ptr));
                    }
                }
                p += 4;
            }
        }

        let ov_stem = format!("ov_{}", ov.overlay_id);
        let ov_cfg = ModuleCfg::analyze(
            &ov_stem,
            &code,
            ov.ram_address,
            ov_seeds.first().copied(),
            &CfgConfig {
                extra_seeds: ov_seeds,
                scan_prologues: opts.scan_prologues,
            },
        );
        total_jts += ov_cfg.jump_tables.len();
        total_interworking += ov_cfg.interworking_transitions;

        let ov_emitted = emit_module_rust(&ov_stem, &ov_cfg);
        total_ov_fns += ov_emitted.function_count;
        for (fname, content) in &ov_emitted.files {
            std::fs::write(gen_dir.join(fname), content)?;
        }
        overlay_metas.push((
            ov.overlay_id,
            ov.ram_address,
            ov.ram_address.saturating_add(ov.ram_size),
            ov_stem,
        ));
    }

    // 3. Emit `src/generated/mod.rs` with Overlay Virtual Address Multiplexer
    let mut mod_rs = String::new();
    let _ = writeln!(mod_rs, "pub mod static_arm9;");
    for (_, _, _, stem) in &overlay_metas {
        let _ = writeln!(mod_rs, "pub mod {stem};");
    }
    let _ = writeln!(mod_rs, "\nuse nitro_runtime::{{CpuContext, NitroBus}};\n");
    let _ = writeln!(
        mod_rs,
        "pub fn dispatch_aot(cpu: &mut CpuContext, bus: &mut NitroBus) -> bool {{"
    );
    let _ = writeln!(mod_rs, "    let pc = cpu.regs[15];");
    let _ = writeln!(mod_rs, "    let thumb = cpu.thumb;");
    if !overlay_metas.is_empty() {
        let _ = writeln!(
            mod_rs,
            "    if let Some(ov_id) = bus.active_arm9_overlay() {{"
        );
        let _ = writeln!(mod_rs, "        match ov_id {{");
        for (ov_id, start_va, end_va, stem) in &overlay_metas {
            let _ = writeln!(
                mod_rs,
                "            {ov_id} if (0x{start_va:08X}..0x{end_va:08X}).contains(&pc) => {{"
            );
            let _ = writeln!(
                mod_rs,
                "                if {stem}::dispatch(pc, thumb, cpu, bus, dispatch_aot) {{ return true; }}"
            );
            let _ = writeln!(mod_rs, "            }}");
        }
        let _ = writeln!(mod_rs, "            _ => {{}}");
        let _ = writeln!(mod_rs, "        }}");
        let _ = writeln!(mod_rs, "    }}");
    }
    let _ = writeln!(
        mod_rs,
        "    static_arm9::dispatch(pc, thumb, cpu, bus, dispatch_aot)"
    );
    let _ = writeln!(mod_rs, "}}");
    std::fs::write(gen_dir.join("mod.rs"), mod_rs)?;

    // 4. Emit `src/main.rs` (BYO-ROM loader + CLI flags `--headless-frames`, `--dump-png`, `--trace-out`)
    let default_rom_escaped = rom_source_path.to_string_lossy().replace('\\', "/");
    let main_rs = build_generated_main_rs(&rom.header.game_title, &default_rom_escaped);
    std::fs::write(src_dir.join("main.rs"), main_rs)?;

    // 5. Emit `Cargo.toml` with `#![forbid(unsafe_code)]`
    let rt_path = opts
        .runtime_crate_path
        .canonicalize()
        .unwrap_or_else(|_| opts.runtime_crate_path.clone())
        .to_string_lossy()
        .replace('\\', "/");
    let rom_path = opts
        .rom_crate_path
        .canonicalize()
        .unwrap_or_else(|_| opts.rom_crate_path.clone())
        .to_string_lossy()
        .replace('\\', "/");
    let rt_clean = rt_path.strip_prefix("//?/").unwrap_or(&rt_path);
    let rom_clean = rom_path.strip_prefix("//?/").unwrap_or(&rom_path);

    let cargo_toml = format!(
        r#"[workspace]

[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2021"

[dependencies]
nitro-rom = {{ path = "{rom_clean}" }}
nitro-runtime = {{ path = "{rt_clean}" }}

[lints.rust]
unsafe_code = "forbid"

[profile.release]
opt-level = 3
lto = "thin"
"#
    );
    std::fs::write(opts.out_dir.join("Cargo.toml"), cargo_toml)?;

    let manifest = RecompManifest {
        game_title: rom.header.game_title.clone(),
        game_code: rom.header.game_code.clone(),
        rom_sha256: rom.sha256_hex.clone(),
        crate_name,
        arm9_entry_address: rom.header.arm9_entry_address,
        arm9_ram_address: rom.header.arm9_ram_address,
        static_functions: static_emitted.function_count,
        static_blocks: static_emitted.block_count,
        overlay_count: rom.arm9_overlays.len(),
        overlay_functions: total_ov_fns,
        jump_tables_resolved: total_jts,
        interworking_calls: total_interworking,
        healed_seeds: opts.extra_seeds.clone(),
    };

    std::fs::write(
        opts.out_dir.join("recomp_manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    Ok(manifest)
}

fn sanitize_crate_name(title: &str, code: &str) -> String {
    let mut s = String::from("nds_recomp_");
    let raw = if title.is_empty() { code } else { title };
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_lowercase());
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    while s.ends_with('_') {
        s.pop();
    }
    if s == "nds_recomp" {
        s.push_str("_port");
    }
    s
}

fn build_generated_main_rs(title: &str, default_rom_hint: &str) -> String {
    format!(
        r#"#![forbid(unsafe_code)]

mod generated;

use std::path::PathBuf;
use nitro_rom::NdsRom;
use nitro_runtime::{{LaunchConfig, NitroMachine}};

fn main() -> Result<(), Box<dyn std::error::Error>> {{
    let mut rom_path: Option<PathBuf> = None;
    let mut headless_frames: Option<u32> = None;
    let mut dump_png: Option<PathBuf> = None;
    let mut trace_out: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {{
        match arg.as_str() {{
            "--rom" => rom_path = args.next().map(PathBuf::from),
            "--headless-frames" => {{
                headless_frames = args.next().and_then(|v| v.parse::<u32>().ok());
            }}
            "--dump-png" => dump_png = args.next().map(PathBuf::from),
            "--trace-out" => trace_out = args.next().map(PathBuf::from),
            other if !other.starts_with('-') && rom_path.is_none() => {{
                rom_path = Some(PathBuf::from(other));
            }}
            _ => {{}}
        }}
    }}

    let resolved_rom = rom_path.unwrap_or_else(|| PathBuf::from("{default_rom_hint}"));
    let rom = NdsRom::from_file(&resolved_rom)?;
    let machine = NitroMachine::from_rom(rom, generated::dispatch_aot);

    let cfg = LaunchConfig {{
        window_title: format!("DSRecomp Native — {title}"),
        headless_frames,
        dump_png,
        trace_out,
    }};
    machine.run(cfg)?;
    Ok(())
}}
"#
    )
}
