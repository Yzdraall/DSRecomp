#![forbid(unsafe_code)]

mod demos;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use image::{ImageBuffer, Rgba};
use nitro_arm::{CfgConfig, CpuMode, ModuleCfg};
use nitro_lifter::{transpile_rom_to_project, RecompManifest, SeedEntry, TranspileOptions};
use nitro_rom::NdsRom;
use nitro_runtime::TracedTarget;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "dsrecomp",
    version,
    about = "Ahead-of-Time Nintendo DS (ARM9/Thumb + Overlays + NitroFS) to Safe Rust Static Recompiler"
)]
struct Cli {
    /// Emit structured JSON output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect an `.nds` cartridge header, CRC16, BLZ compression, Overlays, and NitroFS summary
    Inspect {
        rom: PathBuf,
        /// Optional path to export the 32x32 4bpp cartridge banner icon as a PNG
        #[arg(long)]
        export_icon: Option<PathBuf>,
    },

    /// List or extract files from a cartridge's embedded `NitroFS` (`FNT` + `FAT`)
    Fs {
        rom: PathBuf,
        /// Extract files into this output directory (if omitted, lists files only)
        #[arg(long)]
        extract_to: Option<PathBuf>,
        /// Optional path prefix filter
        #[arg(long)]
        prefix: Option<String>,
    },

    /// Statically analyze `ARM9` + all dynamic `Overlays` (BLZ decompression, interworking CFG, jump-tables)
    Analyze {
        rom: PathBuf,
        /// Scan binary sections for standard ARM/Thumb function prologues (`STMFD` / `PUSH`)
        #[arg(long, default_value_t = true)]
        scan_prologues: bool,
    },

    /// Transpile an `.nds` ROM into a standalone `#![forbid(unsafe_code)]` Rust Cargo project
    Transpile {
        rom: PathBuf,
        /// Destination directory for the generated Rust Cargo crate
        #[arg(long)]
        out: PathBuf,
        /// Compile the generated Cargo crate in `--release` mode immediately
        #[arg(long, default_value_t = false)]
        build: bool,
        /// Launch the compiled native binary (interactive window, or headless if `--headless-frames` is set)
        #[arg(long, default_value_t = false)]
        run: bool,
        /// Run N frames deterministically without opening a GUI window
        #[arg(long)]
        headless_frames: Option<u32>,
        /// Export a 2x upscaled Dual-Screen PNG showcase screenshot
        #[arg(long)]
        dump_png: Option<PathBuf>,
        /// Write runtime-discovered indirect jump targets to JSON for `dsrecomp heal`
        #[arg(long)]
        trace_out: Option<PathBuf>,
    },

    /// Promote runtime-traced indirect jump targets (`trace.json`) into 100% static AOT Rust
    Heal {
        /// Original `.nds` ROM path
        #[arg(long)]
        rom: PathBuf,
        /// Generated Cargo project directory containing `recomp_manifest.json`
        #[arg(long)]
        project: PathBuf,
        /// Runtime trace JSON produced via `--trace-out`
        #[arg(long)]
        trace: PathBuf,
    },

    /// List or assemble clean-room playable Nintendo DS `.nds` demo cartridges from scratch
    Demo {
        #[arg(long, default_value = "demos")]
        out_dir: PathBuf,
        /// Assemble all 3 clean-room `.nds` demo cartridges
        #[arg(long, default_value_t = false)]
        build_all: bool,
    },
}

#[derive(Debug, Serialize)]
struct AnalysisReport {
    game_title: String,
    game_code: String,
    arm9_blz_decompressed: bool,
    arm9_code_bytes: usize,
    static_functions: usize,
    static_arm_blocks: usize,
    static_thumb_blocks: usize,
    static_instructions: usize,
    overlays_analyzed: usize,
    overlay_functions: usize,
    overlay_instructions: usize,
    interworking_transitions: usize,
    jump_tables_resolved: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Inspect { rom, export_icon } => {
            let parsed = NdsRom::from_file(&rom)?;
            let summary = parsed.summary();
            if let (Some(icon_path), Some(banner)) = (&export_icon, &parsed.banner) {
                let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
                    ImageBuffer::from_raw(32, 32, banner.icon_rgba8.to_vec())
                        .ok_or("invalid banner icon buffer")?;
                if let Some(p) = icon_path.parent() {
                    if !p.as_os_str().is_empty() {
                        std::fs::create_dir_all(p)?;
                    }
                }
                img.save(icon_path)?;
            }

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("=== DSRecomp Cartridge Inspection ===");
                println!(
                    "  Title / Code     : {} ({}) [Maker: {}]",
                    summary.header.game_title, summary.header.game_code, summary.header.maker_code
                );
                if let Some(ref bt) = summary.banner_title_english {
                    println!("  Banner Title     : {}", bt.replace('\n', " | "));
                }
                println!(
                    "  Header CRC16     : 0x{:04X} (valid: {})",
                    summary.header.header_crc16_expected, summary.header.header_crc16_valid
                );
                println!(
                    "  ARM9 Binary      : entry=0x{:08X} ram=0x{:08X} raw={} B -> decompressed={} B (BLZ: {})",
                    summary.header.arm9_entry_address,
                    summary.header.arm9_ram_address,
                    summary.arm9_raw_size,
                    summary.arm9_decompressed_size,
                    summary.arm9_was_blz_compressed
                );
                if let Some(mp) = summary.module_params {
                    let (maj, min) = mp.sdk_major_minor();
                    println!(
                        "  NitroSDK Version : {maj}.{min} (0x{:08X})",
                        mp.sdk_version
                    );
                }
                println!(
                    "  Dynamic Overlays : {} ARM9 | {} ARM7",
                    summary.arm9_overlay_count, summary.arm7_overlay_count
                );
                println!("  NitroFS Files    : {}", summary.nitrofs_file_count);
                if let Some(icon_path) = export_icon {
                    println!("  Exported Icon    : {}", icon_path.display());
                }
            }
        }

        Commands::Fs {
            rom,
            extract_to,
            prefix,
        } => {
            let parsed = NdsRom::from_file(&rom)?;
            if let Some(dest) = extract_to {
                let count = parsed
                    .nitrofs
                    .extract_to_dir(&parsed.raw, &dest, prefix.as_deref())?;
                println!(
                    "[DSRecomp NitroFS] Extracted {count} file(s) -> {}",
                    dest.display()
                );
            } else if cli.json {
                println!("{}", serde_json::to_string_pretty(&parsed.nitrofs.files)?);
            } else {
                for f in &parsed.nitrofs.files {
                    if let Some(ref p) = prefix {
                        if !f.path.starts_with(p.trim_start_matches('/')) {
                            continue;
                        }
                    }
                    println!("  [{:04}] {:>9} B  {}", f.file_id, f.size, f.path);
                }
                println!("Total NitroFS files: {}", parsed.nitrofs.files.len());
            }
        }

        Commands::Analyze {
            rom,
            scan_prologues,
        } => {
            let parsed = NdsRom::from_file(&rom)?;
            let (entry_addr, entry_mode) =
                CpuMode::from_interworking_addr(parsed.header.arm9_entry_address);
            let static_cfg = ModuleCfg::analyze(
                "static_arm9",
                &parsed.arm9_code,
                parsed.header.arm9_ram_address,
                Some((entry_addr, entry_mode)),
                &CfgConfig {
                    extra_seeds: Vec::new(),
                    scan_prologues,
                },
            );

            let mut ov_fns = 0usize;
            let mut ov_insts = 0usize;
            let mut interworking = static_cfg.interworking_transitions;
            let mut jts = static_cfg.jump_tables.len();

            for ov in &parsed.arm9_overlays {
                let code = ov.load_code(&parsed.raw, &parsed.nitrofs.fat)?;
                let ov_cfg = ModuleCfg::analyze(
                    format!("ov_{}", ov.overlay_id),
                    &code,
                    ov.ram_address,
                    Some((ov.ram_address, CpuMode::Arm)),
                    &CfgConfig {
                        extra_seeds: Vec::new(),
                        scan_prologues,
                    },
                );
                ov_fns += ov_cfg.functions.len();
                ov_insts += ov_cfg.total_instructions;
                interworking += ov_cfg.interworking_transitions;
                jts += ov_cfg.jump_tables.len();
            }

            let report = AnalysisReport {
                game_title: parsed.header.game_title,
                game_code: parsed.header.game_code,
                arm9_blz_decompressed: parsed.arm9_was_blz_compressed,
                arm9_code_bytes: parsed.arm9_code.len(),
                static_functions: static_cfg.functions.len(),
                static_arm_blocks: static_cfg.arm_block_count,
                static_thumb_blocks: static_cfg.thumb_block_count,
                static_instructions: static_cfg.total_instructions,
                overlays_analyzed: parsed.arm9_overlays.len(),
                overlay_functions: ov_fns,
                overlay_instructions: ov_insts,
                interworking_transitions: interworking,
                jump_tables_resolved: jts,
            };

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "=== DSRecomp Static CFG Report: {} ({}) ===",
                    report.game_title, report.game_code
                );
                println!(
                    "  Static ARM9      : {} functions ({} ARM blocks, {} Thumb blocks, {} instructions)",
                    report.static_functions,
                    report.static_arm_blocks,
                    report.static_thumb_blocks,
                    report.static_instructions
                );
                println!(
                    "  Dynamic Overlays : {} overlays -> {} functions ({} instructions)",
                    report.overlays_analyzed, report.overlay_functions, report.overlay_instructions
                );
                println!(
                    "  Interworking     : {} ARM <-> Thumb transitions | {} switch jump-tables resolved",
                    report.interworking_transitions, report.jump_tables_resolved
                );
            }
        }

        Commands::Transpile {
            rom,
            out,
            build,
            run,
            headless_frames,
            dump_png,
            trace_out,
        } => {
            let parsed = NdsRom::from_file(&rom)?;
            let workspace_root = locate_dsrecomp_workspace_root()?;
            let opts = TranspileOptions {
                out_dir: out.clone(),
                runtime_crate_path: workspace_root.join("crates").join("nitro-runtime"),
                rom_crate_path: workspace_root.join("crates").join("nitro-rom"),
                scan_prologues: true,
                extra_seeds: Vec::new(),
            };
            let manifest = transpile_rom_to_project(&parsed, &rom, &opts)?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!(
                    "[DSRecomp] Transpiled `{}` -> {} (`#![forbid(unsafe_code)]`)",
                    manifest.game_title,
                    out.display()
                );
                println!(
                    "           {} static fns + {} overlay fns across {} overlays ({} jump-tables)",
                    manifest.static_functions,
                    manifest.overlay_functions,
                    manifest.overlay_count,
                    manifest.jump_tables_resolved
                );
            }

            if build || run || headless_frames.is_some() || dump_png.is_some() {
                let status = Command::new("cargo")
                    .arg("build")
                    .arg("--release")
                    .arg("--offline")
                    .arg("--manifest-path")
                    .arg(out.join("Cargo.toml"))
                    .status()?;
                if !status.success() {
                    let retry = Command::new("cargo")
                        .arg("build")
                        .arg("--release")
                        .arg("--manifest-path")
                        .arg(out.join("Cargo.toml"))
                        .status()?;
                    if !retry.success() {
                        return Err("generated Cargo project failed to compile".into());
                    }
                }
            }

            if run || headless_frames.is_some() || dump_png.is_some() {
                let exe_name = if cfg!(windows) {
                    format!("{}.exe", manifest.crate_name)
                } else {
                    manifest.crate_name.clone()
                };
                let bin_path = out.join("target").join("release").join(exe_name);
                let mut cmd = Command::new(&bin_path);
                cmd.arg("--rom").arg(&rom);
                if let Some(frames) = headless_frames {
                    cmd.arg("--headless-frames").arg(frames.to_string());
                }
                if let Some(png) = dump_png {
                    cmd.arg("--dump-png").arg(png);
                }
                if let Some(tr) = trace_out {
                    cmd.arg("--trace-out").arg(tr);
                }
                let status = cmd.status()?;
                if !status.success() {
                    return Err("recompiled native binary exited with error".into());
                }
            }
        }

        Commands::Heal {
            rom,
            project,
            trace,
        } => {
            let manifest_path = project.join("recomp_manifest.json");
            let prev: RecompManifest =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
            let traced: Vec<TracedTarget> =
                serde_json::from_str(&std::fs::read_to_string(&trace)?)?;

            let mut merged = prev.healed_seeds;
            for t in traced {
                let entry = SeedEntry {
                    overlay_id: t.overlay_id,
                    addr: t.addr,
                    mode: t.mode,
                };
                if !merged.contains(&entry) {
                    merged.push(entry);
                }
            }

            let parsed = NdsRom::from_file(&rom)?;
            let workspace_root = locate_dsrecomp_workspace_root()?;
            let opts = TranspileOptions {
                out_dir: project.clone(),
                runtime_crate_path: workspace_root.join("crates").join("nitro-runtime"),
                rom_crate_path: workspace_root.join("crates").join("nitro-rom"),
                scan_prologues: true,
                extra_seeds: merged,
            };
            let updated = transpile_rom_to_project(&parsed, &rom, &opts)?;
            println!(
                "[DSRecomp Heal] Healed `{}` -> {} static fns, {} overlay fns ({} total seeds)",
                updated.game_title,
                updated.static_functions,
                updated.overlay_functions,
                updated.healed_seeds.len()
            );
        }

        Commands::Demo { out_dir, build_all } => {
            if build_all {
                let paths = demos::write_all_demos(&out_dir)?;
                for (spec, path) in demos::ALL_DEMOS.iter().zip(paths.iter()) {
                    println!(
                        "[DSRecomp Demo] Assembled {} -> {} ({})",
                        spec.slug,
                        path.display(),
                        spec.title
                    );
                }
            } else {
                for spec in &demos::ALL_DEMOS {
                    println!(
                        "  - {:<20} ({}) : {}",
                        spec.slug, spec.filename, spec.summary
                    );
                }
            }
        }
    }

    Ok(())
}

fn locate_dsrecomp_workspace_root() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            candidates.push(p.to_path_buf());
        }
    }

    for start in candidates {
        let mut cur: Option<&Path> = Some(start.as_path());
        while let Some(dir) = cur {
            if dir
                .join("crates")
                .join("nitro-runtime")
                .join("Cargo.toml")
                .is_file()
            {
                return Ok(dir.to_path_buf());
            }
            cur = dir.parent();
        }
    }

    Err("unable to locate DSRecomp `crates/nitro-runtime` directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_clean_room_demos_assemble_parse_and_transpile_cleanly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let demo_dir = tmp.path().join("demos");
        let paths = demos::write_all_demos(&demo_dir).expect("write demos");
        assert_eq!(paths.len(), 3);

        for (idx, rom_path) in paths.iter().enumerate() {
            let parsed = NdsRom::from_file(rom_path).expect("parse demo ROM");
            assert!(parsed.header.header_crc16_valid);
            let out_proj = tmp.path().join(format!("port_{idx}"));
            let opts = TranspileOptions {
                out_dir: out_proj.clone(),
                runtime_crate_path: PathBuf::from("../nitro-runtime"),
                rom_crate_path: PathBuf::from("../nitro-rom"),
                scan_prologues: true,
                extra_seeds: Vec::new(),
            };
            let manifest =
                transpile_rom_to_project(&parsed, rom_path, &opts).expect("transpile demo");
            assert!(manifest.static_functions >= 1);
            assert!(out_proj
                .join("src")
                .join("generated")
                .join("mod.rs")
                .is_file());
        }
    }
}
