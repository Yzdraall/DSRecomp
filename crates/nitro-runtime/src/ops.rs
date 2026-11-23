use nitro_arm::{
    decode_arm, decode_thumb, AddrOffset, ArmOp, CpuMode, MemSize, ShiftOp, ShiftedOp2,
};

use crate::bus::{NitroBus, TracedTarget};
use crate::cpu::{CpuContext, MAX_CALL_NESTING};
use crate::AotDispatchFn;

/// Executes a direct `BL` / `BLX` subroutine call, running AOT compiled blocks (or dynamic fallback)
/// until the callee returns to `(return_pc, caller_thumb)` or yields the current frame.
pub fn call_direct(
    cpu: &mut CpuContext,
    bus: &mut NitroBus,
    root_dispatch: AotDispatchFn,
    target: u32,
    callee_thumb: bool,
    return_pc: u32,
    caller_thumb: bool,
) {
    cpu.regs[15] = target;
    cpu.thumb = callee_thumb;
    if cpu.call_depth >= MAX_CALL_NESTING {
        return;
    }
    cpu.call_depth += 1;
    while !cpu.should_yield_frame() {
        if cpu.regs[15] == return_pc && cpu.thumb == caller_thumb {
            break;
        }
        if !root_dispatch(cpu, bus) {
            record_unmapped_target(cpu, bus);
            step_dynamic_instruction(cpu, bus);
        }
    }
    cpu.call_depth -= 1;
}

/// Executes a register-indirect `BLX Rm` call, recording the dynamic target if unmapped.
pub fn call_indirect(
    cpu: &mut CpuContext,
    bus: &mut NitroBus,
    root_dispatch: AotDispatchFn,
    raw_target: u32,
    return_pc: u32,
    caller_thumb: bool,
) {
    cpu.write_pc_interworking(raw_target);
    let target = cpu.regs[15];
    let callee_thumb = cpu.thumb;
    call_direct(
        cpu,
        bus,
        root_dispatch,
        target,
        callee_thumb,
        return_pc,
        caller_thumb,
    );
}

#[inline]
pub fn record_unmapped_target(cpu: &mut CpuContext, bus: &mut NitroBus) {
    let pc = cpu.regs[15];
    // Only record Cartridge / Main RAM code addresses (`0x0200_0000..0x0240_0000`) for static healing
    if (0x0200_0000..0x0240_0000).contains(&pc) {
        bus.traced_jumps.insert(TracedTarget {
            overlay_id: bus.active_arm9_overlay(),
            addr: pc,
            mode: if cpu.thumb {
                CpuMode::Thumb
            } else {
                CpuMode::Arm
            },
        });
    }
}

/// Executes a single memory load/store (`LDR`, `STR`, `LDRB`, `STRB`, `LDRH`, `STRH`, `LDRSB`, `LDRSH`, `LDRD`, `STRD`).
/// Returns `true` if `R15` (`PC`) was loaded (caller must return to dispatcher).
#[allow(clippy::too_many_arguments)]
pub fn exec_mem_single(
    cpu: &mut CpuContext,
    bus: &mut NitroBus,
    load: bool,
    size_code: u8,
    rd: usize,
    rn: usize,
    base: u32,
    offset: i32,
    pre_index: bool,
    writeback: bool,
) -> bool {
    let modified_addr = base.wrapping_add(offset as u32);
    let eff_addr = if pre_index { modified_addr } else { base };

    if load {
        match size_code {
            // Word (ARM unaligned LDR rotates right by `(addr & 3) * 8`)
            0 => {
                let aligned = bus.read32(eff_addr & !3);
                let rot = (eff_addr & 3) * 8;
                let val = if rot == 0 {
                    aligned
                } else {
                    aligned.rotate_right(rot)
                };
                if writeback && rd != rn {
                    cpu.regs[rn] = modified_addr;
                }
                if rd == 15 {
                    cpu.write_pc_interworking(val);
                    return true;
                }
                cpu.regs[rd] = val;
            }
            // Halfword (`u16`)
            1 => {
                let val = u32::from(bus.read16(eff_addr));
                if writeback && rd != rn {
                    cpu.regs[rn] = modified_addr;
                }
                cpu.regs[rd] = val;
            }
            // Signed Halfword (`i16 as i32`)
            2 => {
                let val = i32::from(bus.read16(eff_addr) as i16) as u32;
                if writeback && rd != rn {
                    cpu.regs[rn] = modified_addr;
                }
                cpu.regs[rd] = val;
            }
            // Byte (`u8`)
            3 => {
                let val = u32::from(bus.read8(eff_addr));
                if writeback && rd != rn {
                    cpu.regs[rn] = modified_addr;
                }
                cpu.regs[rd] = val;
            }
            // Signed Byte (`i8 as i32`)
            4 => {
                let val = i32::from(bus.read8(eff_addr) as i8) as u32;
                if writeback && rd != rn {
                    cpu.regs[rn] = modified_addr;
                }
                cpu.regs[rd] = val;
            }
            // Doubleword (`LDRD`)
            _ => {
                let lo = bus.read32(eff_addr);
                let hi = bus.read32(eff_addr.wrapping_add(4));
                if writeback && rd != rn && (rd + 1) != rn {
                    cpu.regs[rn] = modified_addr;
                }
                cpu.regs[rd] = lo;
                if rd + 1 < 15 {
                    cpu.regs[rd + 1] = hi;
                }
            }
        }
        false
    } else {
        let store_val = if rd == 15 {
            cpu.regs[15].wrapping_add(if cpu.thumb { 4 } else { 8 })
        } else {
            cpu.regs[rd]
        };
        match size_code {
            0 => bus.write32(eff_addr, store_val),
            1 | 2 => bus.write16(eff_addr, store_val as u16),
            3 | 4 => bus.write8(eff_addr, store_val as u8),
            _ => {
                bus.write32(eff_addr, store_val);
                if rd + 1 < 16 {
                    bus.write32(eff_addr.wrapping_add(4), cpu.regs[rd + 1]);
                }
            }
        }
        if writeback {
            cpu.regs[rn] = modified_addr;
        }
        false
    }
}

/// Executes block transfers (`LDM` / `STM` / `PUSH` / `POP`).
/// Returns `true` if `R15` (`PC`) was loaded.
#[allow(clippy::too_many_arguments)]
pub fn exec_mem_multiple(
    cpu: &mut CpuContext,
    bus: &mut NitroBus,
    load: bool,
    rn: usize,
    reg_mask: u16,
    increment: bool,
    before: bool,
    writeback: bool,
) -> bool {
    let count = reg_mask.count_ones();
    if count == 0 {
        return false;
    }

    let base = cpu.regs[rn];
    let total_bytes = count * 4;
    let (mut cur_addr, final_wb) = match (increment, before) {
        (true, false) => (base, base.wrapping_add(total_bytes)), // IA
        (true, true) => (base.wrapping_add(4), base.wrapping_add(total_bytes)), // IB
        (false, false) => (
            base.wrapping_sub(total_bytes).wrapping_add(4),
            base.wrapping_sub(total_bytes),
        ), // DA
        (false, true) => (
            base.wrapping_sub(total_bytes),
            base.wrapping_sub(total_bytes),
        ), // DB
    };

    let mut wrote_pc = false;
    if load {
        for r in 0..16usize {
            if ((reg_mask >> r) & 1) != 0 {
                let val = bus.read32(cur_addr);
                cur_addr = cur_addr.wrapping_add(4);
                if r == 15 {
                    cpu.write_pc_interworking(val);
                    wrote_pc = true;
                } else {
                    cpu.regs[r] = val;
                }
            }
        }
        if writeback && ((reg_mask >> rn) & 1) == 0 {
            cpu.regs[rn] = final_wb;
        }
    } else {
        for r in 0..16usize {
            if ((reg_mask >> r) & 1) != 0 {
                let val = if r == 15 {
                    cpu.regs[15].wrapping_add(if cpu.thumb { 4 } else { 8 })
                } else {
                    cpu.regs[r]
                };
                bus.write32(cur_addr, val);
                cur_addr = cur_addr.wrapping_add(4);
            }
        }
        if writeback {
            cpu.regs[rn] = final_wb;
        }
    }
    wrote_pc
}

pub fn exec_swp(
    cpu: &mut CpuContext,
    bus: &mut NitroBus,
    byte: bool,
    rd: usize,
    rm: usize,
    rn: usize,
) {
    let addr = cpu.regs[rn];
    let src_val = cpu.regs[rm];
    if byte {
        let old = u32::from(bus.read8(addr));
        bus.write8(addr, src_val as u8);
        cpu.regs[rd] = old;
    } else {
        let aligned = bus.read32(addr & !3);
        let rot = (addr & 3) * 8;
        let old = if rot == 0 {
            aligned
        } else {
            aligned.rotate_right(rot)
        };
        bus.write32(addr & !3, src_val);
        cpu.regs[rd] = old;
    }
}

/// Safe-Rust single-instruction fallback executor for dynamic/ITCM/unmapped code paths.
/// Seamlessly hands control back to AOT Rust as soon as `PC` enters any compiled block.
pub fn step_dynamic_instruction(cpu: &mut CpuContext, bus: &mut NitroBus) {
    let pc = cpu.regs[15];
    let inst = if cpu.thumb {
        let hw0 = bus.read16(pc);
        let hw1 = Some(bus.read16(pc.wrapping_add(2)));
        decode_thumb(pc, hw0, hw1)
    } else {
        decode_arm(pc, bus.read32(pc))
    };

    let next_pc = pc.wrapping_add(u32::from(inst.size));
    cpu.regs[15] = next_pc;
    cpu.step_tick(1);
    cpu.fallback_steps_executed += 1;

    if !cpu.check_cond(inst.cond as u8) {
        return;
    }

    match inst.op {
        ArmOp::Nop => {}
        ArmOp::Alu {
            op,
            rd,
            rn,
            rhs,
            set_flags,
        } => {
            let rn_val = if rn == 15 {
                inst.pc_read_value()
            } else {
                cpu.regs[usize::from(rn)]
            };
            let (rhs_val, carry) = eval_op2_runtime(cpu, &inst, rhs);
            if let Some(res) = cpu.exec_alu(op as u8, rn_val, rhs_val, carry, set_flags) {
                if rd == 15 {
                    cpu.write_pc_alu(res);
                } else {
                    cpu.regs[usize::from(rd)] = res;
                }
            }
        }
        ArmOp::Mul {
            accumulate,
            rd,
            rm,
            rs,
            rn,
            set_flags,
        } => {
            cpu.exec_mul(
                accumulate,
                usize::from(rd),
                usize::from(rm),
                usize::from(rs),
                usize::from(rn),
                set_flags,
            );
        }
        ArmOp::Mull {
            signed,
            accumulate,
            rd_hi,
            rd_lo,
            rm,
            rs,
            set_flags,
        } => {
            cpu.exec_mull(
                signed,
                accumulate,
                usize::from(rd_hi),
                usize::from(rd_lo),
                usize::from(rm),
                usize::from(rs),
                set_flags,
            );
        }
        ArmOp::Clz { rd, rm } => {
            cpu.regs[usize::from(rd)] = cpu.regs[usize::from(rm)].leading_zeros();
        }
        ArmOp::Branch {
            target,
            link,
            exchange_to,
        } => {
            if link {
                cpu.regs[14] = if cpu.thumb { next_pc | 1 } else { next_pc };
            }
            if let Some(dst_mode) = exchange_to {
                cpu.thumb = dst_mode == CpuMode::Thumb;
            }
            cpu.regs[15] = if cpu.thumb { target & !1 } else { target & !3 };
        }
        ArmOp::BranchExchangeReg { rm, link } => {
            let target = cpu.regs[usize::from(rm)];
            if link {
                cpu.regs[14] = if cpu.thumb { next_pc | 1 } else { next_pc };
            }
            cpu.write_pc_interworking(target);
        }
        ArmOp::MemSingle {
            load,
            size,
            rd,
            rn,
            offset,
            pre_index,
            writeback,
            thumb_pc_align,
        } => {
            let base = if rn == 15 {
                let p = inst.pc_read_value();
                if thumb_pc_align {
                    p & !3
                } else {
                    p
                }
            } else {
                cpu.regs[usize::from(rn)]
            };
            let off = match offset {
                AddrOffset::Imm(i) => i,
                AddrOffset::Reg {
                    rm,
                    shift,
                    amount,
                    add,
                } => {
                    let v = cpu.shift_imm_val(
                        cpu.regs[usize::from(rm)],
                        shift_op_to_code(shift),
                        amount,
                    ) as i32;
                    if add {
                        v
                    } else {
                        -v
                    }
                }
            };
            let sc = match size {
                MemSize::Word => 0,
                MemSize::Halfword => 1,
                MemSize::SignedHalfword => 2,
                MemSize::Byte => 3,
                MemSize::SignedByte => 4,
                MemSize::Doubleword => 5,
            };
            exec_mem_single(
                cpu,
                bus,
                load,
                sc,
                usize::from(rd),
                usize::from(rn),
                base,
                off,
                pre_index,
                writeback,
            );
        }
        ArmOp::MemMultiple {
            load,
            rn,
            reg_mask,
            increment,
            before,
            writeback,
            ..
        } => {
            exec_mem_multiple(
                cpu,
                bus,
                load,
                usize::from(rn),
                reg_mask,
                increment,
                before,
                writeback,
            );
        }
        ArmOp::Mrs { rd, spsr } => {
            cpu.regs[usize::from(rd)] = cpu.read_psr(spsr);
        }
        ArmOp::Msr { spsr, mask, rhs } => {
            let (val, _) = eval_op2_runtime(cpu, &inst, rhs);
            cpu.write_psr(spsr, mask, val);
        }
        ArmOp::Swp { byte, rd, rm, rn } => {
            exec_swp(
                cpu,
                bus,
                byte,
                usize::from(rd),
                usize::from(rm),
                usize::from(rn),
            );
        }
        ArmOp::Swi { comment } => {
            bus.swi_hle(cpu, comment);
        }
        ArmOp::CoprocMove {
            to_coproc,
            cp,
            op1,
            rd,
            crn,
            crm,
            op2,
        } => {
            bus.coproc_move(cpu, to_coproc, cp, op1, usize::from(rd), crn, crm, op2);
        }
        ArmOp::Unknown { .. } => {
            cpu.vblank_requested = true;
        }
    }
}

fn shift_op_to_code(s: ShiftOp) -> u8 {
    match s {
        ShiftOp::Lsl => 0,
        ShiftOp::Lsr => 1,
        ShiftOp::Asr => 2,
        ShiftOp::Ror => 3,
        ShiftOp::Rrx => 4,
    }
}

fn eval_op2_runtime(
    cpu: &CpuContext,
    inst: &nitro_arm::DecodedInst,
    op2: ShiftedOp2,
) -> (u32, bool) {
    match op2 {
        ShiftedOp2::Imm { value, rot_carry } => (value, rot_carry.unwrap_or(cpu.c)),
        ShiftedOp2::RegImm { rm, shift, amount } => {
            let v = if rm == 15 {
                inst.pc_read_value()
            } else {
                cpu.regs[usize::from(rm)]
            };
            cpu.shift_imm_pair(v, shift_op_to_code(shift), amount)
        }
        ShiftedOp2::RegReg { rm, shift, rs } => {
            let v = if rm == 15 {
                inst.pc_read_value()
            } else {
                cpu.regs[usize::from(rm)]
            };
            let s = cpu.regs[usize::from(rs)];
            cpu.shift_reg_pair(v, shift_op_to_code(shift), s)
        }
    }
}
