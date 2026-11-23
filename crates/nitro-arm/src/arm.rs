use crate::isa::{
    AddrOffset, AluOp, ArmOp, Cond, CpuMode, DecodedInst, MemSize, ShiftOp, ShiftedOp2,
};

/// Decodes a single 32-bit `ARMv5TE` instruction word at `addr`.
pub fn decode_arm(addr: u32, raw: u32) -> DecodedInst {
    let cond_bits = ((raw >> 28) & 0x0F) as u8;

    // ARMv5TE Unconditional extension space (`cond == 0xF`)
    if cond_bits == 0x0F {
        // `BLX <label>` (ARM -> Thumb immediate): `1111 101H imm24`
        if ((raw >> 25) & 0x07) == 0b101 {
            let h = ((raw >> 24) & 1) << 1;
            let imm24 = raw & 0x00FF_FFFF;
            let simm = (((imm24 << 8) as i32) >> 6) | (h as i32);
            let target = addr.wrapping_add(8).wrapping_add(simm as u32);
            return DecodedInst {
                addr,
                mode: CpuMode::Arm,
                size: 4,
                raw,
                cond: Cond::Al,
                op: ArmOp::Branch {
                    target,
                    link: true,
                    exchange_to: Some(CpuMode::Thumb),
                },
            };
        }
        return DecodedInst {
            addr,
            mode: CpuMode::Arm,
            size: 4,
            raw,
            cond: Cond::Nv,
            op: ArmOp::Unknown { raw },
        };
    }

    let cond = Cond::from_u4(cond_bits);
    let top3 = (raw >> 25) & 0x07;

    let op = match top3 {
        // Data processing / Multiply / Extra Load-Store / Misc
        0b000 | 0b001 => decode_arm_dp_or_misc(raw),
        // Single Data Transfer (`LDR` / `STR` / `LDRB` / `STRB`)
        0b010 | 0b011 => {
            if top3 == 0b011 && ((raw >> 4) & 1) == 1 {
                ArmOp::Unknown { raw }
            } else {
                decode_arm_single_transfer(raw)
            }
        }
        // Block Data Transfer (`LDM` / `STM`)
        0b100 => decode_arm_block_transfer(raw),
        // Branch / Branch with Link (`B` / `BL`)
        0b101 => {
            let link = ((raw >> 24) & 1) != 0;
            let imm24 = raw & 0x00FF_FFFF;
            let simm = ((imm24 << 8) as i32) >> 6;
            let target = addr.wrapping_add(8).wrapping_add(simm as u32);
            ArmOp::Branch {
                target,
                link,
                exchange_to: None,
            }
        }
        // Coprocessor / Software Interrupt
        0b111 => {
            if ((raw >> 24) & 1) != 0 {
                ArmOp::Swi {
                    comment: raw & 0x00FF_FFFF,
                }
            } else if ((raw >> 4) & 1) != 0 {
                // MCR / MRC
                let to_coproc = ((raw >> 20) & 1) == 0;
                let op1 = ((raw >> 21) & 0x07) as u8;
                let crn = ((raw >> 16) & 0x0F) as u8;
                let rd = ((raw >> 12) & 0x0F) as u8;
                let cp = ((raw >> 8) & 0x0F) as u8;
                let op2 = ((raw >> 5) & 0x07) as u8;
                let crm = (raw & 0x0F) as u8;
                ArmOp::CoprocMove {
                    to_coproc,
                    cp,
                    op1,
                    rd,
                    crn,
                    crm,
                    op2,
                }
            } else {
                ArmOp::Nop
            }
        }
        _ => ArmOp::Unknown { raw },
    };

    DecodedInst {
        addr,
        mode: CpuMode::Arm,
        size: 4,
        raw,
        cond,
        op,
    }
}

fn decode_arm_dp_or_misc(raw: u32) -> ArmOp {
    let is_imm = ((raw >> 25) & 1) != 0;
    let opcode = ((raw >> 21) & 0x0F) as u8;
    let set_flags = ((raw >> 20) & 1) != 0;

    if !is_imm {
        let bit7 = ((raw >> 7) & 1) != 0;
        let bit4 = ((raw >> 4) & 1) != 0;

        // Misc instruction space (`opcode` in `0b10xx` with `S == 0`)
        if (opcode & 0b1100) == 0b1000 && !set_flags {
            // BX / BLX Rm / CLZ / MRS / MSR reg
            let low4 = raw & 0x0F;
            if ((raw >> 4) & 0xFF) == 0b0001_0010 {
                return ArmOp::BranchExchangeReg {
                    rm: low4 as u8,
                    link: false,
                };
            }
            if ((raw >> 4) & 0xFF) == 0b0001_0011 {
                return ArmOp::BranchExchangeReg {
                    rm: low4 as u8,
                    link: true,
                };
            }
            if ((raw >> 4) & 0xFF) == 0b0001_0110 {
                let rd = ((raw >> 12) & 0x0F) as u8;
                return ArmOp::Clz { rd, rm: low4 as u8 };
            }
            if ((raw >> 4) & 0xFF) == 0 {
                if (opcode & 1) == 0 {
                    // MRS
                    let spsr = ((raw >> 22) & 1) != 0;
                    let rd = ((raw >> 12) & 0x0F) as u8;
                    return ArmOp::Mrs { rd, spsr };
                } else {
                    // MSR register
                    let spsr = ((raw >> 22) & 1) != 0;
                    let mask = ((raw >> 16) & 0x0F) as u8;
                    return ArmOp::Msr {
                        spsr,
                        mask,
                        rhs: ShiftedOp2::RegImm {
                            rm: low4 as u8,
                            shift: ShiftOp::Lsl,
                            amount: 0,
                        },
                    };
                }
            }
        }

        // Multiply / Extra Load-Store (`bit7 && bit4`)
        if bit7 && bit4 {
            let sub_op = ((raw >> 5) & 0x03) as u8;
            if sub_op == 0 {
                let top_mul = (raw >> 22) & 0x0F;
                if top_mul == 0b0000 {
                    // MUL / MLA
                    let accumulate = ((raw >> 21) & 1) != 0;
                    let rd = ((raw >> 16) & 0x0F) as u8;
                    let rn = ((raw >> 12) & 0x0F) as u8;
                    let rs = ((raw >> 8) & 0x0F) as u8;
                    let rm = (raw & 0x0F) as u8;
                    return ArmOp::Mul {
                        accumulate,
                        rd,
                        rm,
                        rs,
                        rn,
                        set_flags,
                    };
                }
                if (top_mul & 0b1110) == 0b0010 {
                    // UMULL / UMLAL / SMULL / SMLAL
                    let signed = ((raw >> 22) & 1) != 0;
                    let accumulate = ((raw >> 21) & 1) != 0;
                    let rd_hi = ((raw >> 16) & 0x0F) as u8;
                    let rd_lo = ((raw >> 12) & 0x0F) as u8;
                    let rs = ((raw >> 8) & 0x0F) as u8;
                    let rm = (raw & 0x0F) as u8;
                    return ArmOp::Mull {
                        signed,
                        accumulate,
                        rd_hi,
                        rd_lo,
                        rm,
                        rs,
                        set_flags,
                    };
                }
                if (top_mul & 0b1101) == 0b0100 && !set_flags && ((raw >> 21) & 1) == 0 {
                    // SWP / SWPB
                    let byte = ((raw >> 22) & 1) != 0;
                    let rn = ((raw >> 16) & 0x0F) as u8;
                    let rd = ((raw >> 12) & 0x0F) as u8;
                    let rm = (raw & 0x0F) as u8;
                    return ArmOp::Swp { byte, rd, rm, rn };
                }
                return ArmOp::Unknown { raw };
            }

            // Halfword / Signed Byte / Doubleword Transfer (`LDRH`, `STRH`, `LDRSB`, `LDRSH`, `LDRD`, `STRD`)
            let pre_index = ((raw >> 24) & 1) != 0;
            let add = ((raw >> 23) & 1) != 0;
            let imm_off = ((raw >> 22) & 1) != 0;
            let writeback = !pre_index || (((raw >> 21) & 1) != 0);
            let l_bit = ((raw >> 20) & 1) != 0;
            let rn = ((raw >> 16) & 0x0F) as u8;
            let rd = ((raw >> 12) & 0x0F) as u8;

            let offset = if imm_off {
                let imm8 = (((raw >> 8) & 0x0F) << 4) | (raw & 0x0F);
                let signed_imm = if add { imm8 as i32 } else { -(imm8 as i32) };
                AddrOffset::Imm(signed_imm)
            } else {
                AddrOffset::Reg {
                    rm: (raw & 0x0F) as u8,
                    shift: ShiftOp::Lsl,
                    amount: 0,
                    add,
                }
            };

            let (load, size) = match (l_bit, sub_op) {
                (false, 0b01) => (false, MemSize::Halfword),
                (true, 0b01) => (true, MemSize::Halfword),
                (true, 0b10) => (true, MemSize::SignedByte),
                (true, 0b11) => (true, MemSize::SignedHalfword),
                (false, 0b10) => (true, MemSize::Doubleword), // ARMv5TE LDRD
                (false, 0b11) => (false, MemSize::Doubleword), // ARMv5TE STRD
                _ => return ArmOp::Unknown { raw },
            };

            return ArmOp::MemSingle {
                load,
                size,
                rd,
                rn,
                offset,
                pre_index,
                writeback,
                thumb_pc_align: false,
            };
        }
    } else if (opcode & 0b1101) == 0b1001 && !set_flags {
        // MSR immediate
        let spsr = ((raw >> 22) & 1) != 0;
        let mask = ((raw >> 16) & 0x0F) as u8;
        let rhs = decode_arm_imm_op2(raw);
        return ArmOp::Msr { spsr, mask, rhs };
    }

    let rn = ((raw >> 16) & 0x0F) as u8;
    let rd = ((raw >> 12) & 0x0F) as u8;
    let rhs = if is_imm {
        decode_arm_imm_op2(raw)
    } else {
        decode_arm_reg_op2(raw)
    };

    // Recognize canonical `MOV R0, R0` (`0xE1A00000`) as NOP
    if opcode == (AluOp::Mov as u8)
        && !set_flags
        && rd == 0
        && matches!(
            rhs,
            ShiftedOp2::RegImm {
                rm: 0,
                shift: ShiftOp::Lsl,
                amount: 0
            }
        )
    {
        return ArmOp::Nop;
    }

    ArmOp::Alu {
        op: AluOp::from_u4(opcode),
        rd,
        rn,
        rhs,
        set_flags,
    }
}

fn decode_arm_imm_op2(raw: u32) -> ShiftedOp2 {
    let imm8 = raw & 0xFF;
    let rot = ((raw >> 8) & 0x0F) * 2;
    let value = imm8.rotate_right(rot);
    let rot_carry = if rot == 0 {
        None
    } else {
        Some(((value >> 31) & 1) != 0)
    };
    ShiftedOp2::Imm { value, rot_carry }
}

fn decode_arm_reg_op2(raw: u32) -> ShiftedOp2 {
    let rm = (raw & 0x0F) as u8;
    let shift_code = ((raw >> 5) & 0x03) as u8;
    if ((raw >> 4) & 1) == 0 {
        let imm5 = ((raw >> 7) & 0x1F) as u8;
        let (shift, amount) = ShiftOp::from_u2(shift_code, Some(imm5));
        ShiftedOp2::RegImm { rm, shift, amount }
    } else {
        let rs = ((raw >> 8) & 0x0F) as u8;
        let (shift, _) = ShiftOp::from_u2(shift_code, None);
        ShiftedOp2::RegReg { rm, shift, rs }
    }
}

fn decode_arm_single_transfer(raw: u32) -> ArmOp {
    let reg_offset = ((raw >> 25) & 1) != 0;
    let pre_index = ((raw >> 24) & 1) != 0;
    let add = ((raw >> 23) & 1) != 0;
    let byte = ((raw >> 22) & 1) != 0;
    let writeback = !pre_index || (((raw >> 21) & 1) != 0);
    let load = ((raw >> 20) & 1) != 0;
    let rn = ((raw >> 16) & 0x0F) as u8;
    let rd = ((raw >> 12) & 0x0F) as u8;

    let offset = if !reg_offset {
        let imm12 = (raw & 0x0FFF) as i32;
        AddrOffset::Imm(if add { imm12 } else { -imm12 })
    } else {
        let rm = (raw & 0x0F) as u8;
        let shift_code = ((raw >> 5) & 0x03) as u8;
        let imm5 = ((raw >> 7) & 0x1F) as u8;
        let (shift, amount) = ShiftOp::from_u2(shift_code, Some(imm5));
        AddrOffset::Reg {
            rm,
            shift,
            amount,
            add,
        }
    };

    ArmOp::MemSingle {
        load,
        size: if byte { MemSize::Byte } else { MemSize::Word },
        rd,
        rn,
        offset,
        pre_index,
        writeback,
        thumb_pc_align: false,
    }
}

fn decode_arm_block_transfer(raw: u32) -> ArmOp {
    let before = ((raw >> 24) & 1) != 0;
    let increment = ((raw >> 23) & 1) != 0;
    let user_bank = ((raw >> 22) & 1) != 0;
    let writeback = ((raw >> 21) & 1) != 0;
    let load = ((raw >> 20) & 1) != 0;
    let rn = ((raw >> 16) & 0x0F) as u8;
    let reg_mask = (raw & 0xFFFF) as u16;

    ArmOp::MemMultiple {
        load,
        rn,
        reg_mask,
        increment,
        before,
        writeback,
        user_bank,
    }
}
