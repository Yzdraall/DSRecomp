# Contributing to DSRecomp

Thank you for your interest in contributing to **DSRecomp**!

## Clean-Room & Legal Policy (Strict BYO-ROM)

To keep `DSRecomp` 100% legal and safe for everyone:

1. **Zero Copyrighted Assets**: Never commit commercial Nintendo DS ROMs (`.nds`, `.srl`), extracted proprietary `arm9.bin` / `NitroFS` dumps, or proprietary Nintendo BIOS/firmware binaries in issues, tests, or pull requests.
2. **Clean-Room Test Cartridges Only**: All automated tests and built-in demos must assemble their own machine code from scratch using `nitro_arm::{ArmAsm, ThumbAsm}` and `nitro_rom::NdsRomBuilder`.
3. **Zero `unsafe`**: Workspace crates and all generated AOT Rust projects enforce `#![forbid(unsafe_code)]`.

## Pre-PR Checklist

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
