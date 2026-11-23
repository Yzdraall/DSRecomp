//! Dual-ISA `ARMv5TE` (32-bit) + `Thumb` (16/32-bit) decoder, disassembler,
//! interworking Control-Flow Graph (`CFG`) + switch jump-table analyzer, and clean-room assembler.

pub mod arm;
pub mod asm;
pub mod cfg;
pub mod isa;
pub mod thumb;

pub use arm::decode_arm;
pub use asm::{ArmAsm, ThumbAsm};
pub use cfg::{
    decode_at, BasicBlock, CallTarget, CfgConfig, CfgEdge, EdgeKind, FunctionUnit, JumpTableInfo,
    ModuleCfg,
};
pub use isa::{
    reg_name, AddrOffset, AluOp, ArmOp, Cond, CpuMode, DecodedInst, MemSize, ShiftOp, ShiftedOp2,
};
pub use thumb::decode_thumb;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_to_thumb_interworking_and_jump_table_cfg_resolve_cleanly() {
        let base = 0x0200_0800u32;
        let mut arm = ArmAsm::new();

        // fn_02000800_arm:
        //   push {r4, lr}
        //   cmp r0, #1
        //   ldrls pc, [pc, r0, lsl #2]
        //   b default_exit
        //   .word case0
        //   .word case1
        arm.push((1 << 4) | (1 << 14));
        arm.cmp_imm(0, 1);
        arm.ldrls_pc_jump_table(0);
        let default_b_off = arm.offset();
        arm.emit_u32(0); // patched below
        let jt_off = arm.offset();
        arm.emit_u32(0); // case 0
        arm.emit_u32(0); // case 1

        let case0_off = arm.offset();
        // Call Thumb subroutine via BLX!
        let blx_slot = arm.offset();
        arm.emit_u32(0);
        let b_end0 = arm.offset();
        arm.emit_u32(0);

        let case1_off = arm.offset();
        arm.add_imm(0, 0, 42);

        let exit_off = arm.offset();
        arm.pop((1 << 4) | (1 << 15));

        // Append a Thumb subroutine right after ARM code!
        let thumb_off = arm.offset();
        let mut th = ThumbAsm::new();
        th.push(1 << 4, true);
        th.lsl_imm(0, 0, 1);
        th.add_imm(0, 7);
        th.pop(1 << 4, true);

        let mut code = arm.into_bytes();
        code.extend_from_slice(&th.into_bytes());

        // Patch branches & jump-table pointers
        let patch_u32 = |buf: &mut [u8], off: u32, val: u32| {
            let o = off as usize;
            buf[o..o + 4].copy_from_slice(&val.to_le_bytes());
        };
        let mk_b = |from: u32, to: u32| -> u32 {
            let rel = (((to as i32) - ((from as i32) + 8)) >> 2) as u32;
            0xEA00_0000 | (rel & 0x00FF_FFFF)
        };
        let mk_blx_thumb = |from: u32, to: u32| -> u32 {
            let diff = (to as i32) - ((from as i32) + 8);
            let h = ((diff >> 1) & 1) as u32;
            0xFA00_0000 | (h << 24) | (((diff >> 2) as u32) & 0x00FF_FFFF)
        };

        patch_u32(&mut code, default_b_off, mk_b(default_b_off, exit_off));
        patch_u32(&mut code, jt_off, base + case0_off);
        patch_u32(&mut code, jt_off + 4, base + case1_off);
        patch_u32(&mut code, blx_slot, mk_blx_thumb(blx_slot, thumb_off));
        patch_u32(&mut code, b_end0, mk_b(b_end0, exit_off));

        let cfg = ModuleCfg::analyze(
            "unit_arm9",
            &code,
            base,
            Some((base, CpuMode::Arm)),
            &CfgConfig::default(),
        );

        assert_eq!(cfg.jump_tables.len(), 1);
        assert_eq!(cfg.jump_tables[0].entries.len(), 2);
        assert_eq!(cfg.interworking_transitions, 1);
        assert!(cfg.functions.contains_key(&(base, CpuMode::Arm)));
        assert!(cfg
            .functions
            .contains_key(&(base + thumb_off, CpuMode::Thumb)));
    }
}
