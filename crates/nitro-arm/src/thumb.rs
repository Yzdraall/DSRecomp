use crate::isa::{
    AddrOffset, AluOp, ArmOp, Cond, CpuMode, DecodedInst, MemSize, ShiftOp, ShiftedOp2,
};

/// Decodes a 16-bit (or 32-bit `BL`/`BLX` pair) Thumb instruction at `addr`.
pub fn decode_thumb(addr: u32, hw0: u16, hw1: Option<u16>) -> DecodedInst {
    // Format 19: 32-bit Long Branch with Link (`BL` / `BLX`)
    if (hw0 >> 11) == 0b11110 {
        if let Some(second) = hw1 {
            let s_top = second >> 11;
            if s_top == 0b11111 {
                let hi = (((i32::from(hw0 & 0x07FF)) << 21) >> 9) as u32;
                let lo = u32::from(second & 0x07FF) << 1;
                let target = addr.wrapping_add(4).wrapping_add(hi).wrapping_add(lo);
                return DecodedInst {
                    addr,
                    mode: CpuMode::Thumb,
                    size: 4,
                    raw: (u32::from(hw0) << 16) | u32::from(second),
                    cond: Cond::Al,
                    op: ArmOp::Branch {
                        target,
                        link: true,
                        exchange_to: None,
                    },
                };
            }
            if s_top == 0b11101 && (second & 1) == 0 {
                let hi = (((i32::from(hw0 & 0x07FF)) << 21) >> 9) as u32;
                let lo = u32::from(second & 0x07FE) << 1;
                let base = addr.wrapping_add(4) & !3;
                let target = base.wrapping_add(hi).wrapping_add(lo);
                return DecodedInst {
                    addr,
                    mode: CpuMode::Thumb,
                    size: 4,
                    raw: (u32::from(hw0) << 16) | u32::from(second),
                    cond: Cond::Al,
                    op: ArmOp::Branch {
                        target,
                        link: true,
                        exchange_to: Some(CpuMode::Arm),
                    },
                };
            }
        }
    }

    let raw = u32::from(hw0);
    let mut cond = Cond::Al;

    let op = match hw0 >> 13 {
        // Formats 1 & 2: Shift by immediate / Add-Subtract 3-op
        0b000 => {
            let sub = (hw0 >> 11) & 0x03;
            if sub == 0b11 {
                // Format 2: ADD / SUB Rd, Rn, Rm/#imm3
                let is_imm = ((hw0 >> 10) & 1) != 0;
                let is_sub = ((hw0 >> 9) & 1) != 0;
                let rm_or_imm = ((hw0 >> 6) & 0x07) as u8;
                let rn = ((hw0 >> 3) & 0x07) as u8;
                let rd = (hw0 & 0x07) as u8;
                let rhs = if is_imm {
                    ShiftedOp2::Imm {
                        value: u32::from(rm_or_imm),
                        rot_carry: None,
                    }
                } else {
                    ShiftedOp2::RegImm {
                        rm: rm_or_imm,
                        shift: ShiftOp::Lsl,
                        amount: 0,
                    }
                };
                ArmOp::Alu {
                    op: if is_sub { AluOp::Sub } else { AluOp::Add },
                    rd,
                    rn,
                    rhs,
                    set_flags: true,
                }
            } else {
                // Format 1: LSL / LSR / ASR Rd, Rm, #imm5
                let imm5 = ((hw0 >> 6) & 0x1F) as u8;
                let rm = ((hw0 >> 3) & 0x07) as u8;
                let rd = (hw0 & 0x07) as u8;
                let (shift, amount) = ShiftOp::from_u2(sub as u8, Some(imm5));
                ArmOp::Alu {
                    op: AluOp::Mov,
                    rd,
                    rn: 0,
                    rhs: ShiftedOp2::RegImm { rm, shift, amount },
                    set_flags: true,
                }
            }
        }

        // Format 3: MOV / CMP / ADD / SUB Rd, #imm8
        0b001 => {
            let sub = (hw0 >> 11) & 0x03;
            let rd = ((hw0 >> 8) & 0x07) as u8;
            let imm8 = u32::from(hw0 & 0xFF);
            let alu_op = match sub {
                0 => AluOp::Mov,
                1 => AluOp::Cmp,
                2 => AluOp::Add,
                _ => AluOp::Sub,
            };
            ArmOp::Alu {
                op: alu_op,
                rd,
                rn: rd,
                rhs: ShiftedOp2::Imm {
                    value: imm8,
                    rot_carry: None,
                },
                set_flags: true,
            }
        }

        // Formats 4, 5, 6, 7, 8
        0b010 => {
            if ((hw0 >> 10) & 0b111) == 0b000 {
                // Format 4: 16 Thumb ALU ops
                let alu_sub = (hw0 >> 6) & 0x0F;
                let rm = ((hw0 >> 3) & 0x07) as u8;
                let rd = (hw0 & 0x07) as u8;
                decode_thumb_alu_fmt4(alu_sub as u8, rd, rm)
            } else if ((hw0 >> 10) & 0b111) == 0b001 {
                // Format 5: Hi-register ops / BX / BLX
                let sub = (hw0 >> 8) & 0x03;
                let h1 = ((hw0 >> 7) & 1) as u8;
                let h2 = ((hw0 >> 6) & 1) as u8;
                let rm = (((hw0 >> 3) & 0x07) as u8) | (h2 << 3);
                let rd = ((hw0 & 0x07) as u8) | (h1 << 3);
                match sub {
                    0b00 => ArmOp::Alu {
                        op: AluOp::Add,
                        rd,
                        rn: rd,
                        rhs: ShiftedOp2::RegImm {
                            rm,
                            shift: ShiftOp::Lsl,
                            amount: 0,
                        },
                        set_flags: false,
                    },
                    0b01 => ArmOp::Alu {
                        op: AluOp::Cmp,
                        rd: 0,
                        rn: rd,
                        rhs: ShiftedOp2::RegImm {
                            rm,
                            shift: ShiftOp::Lsl,
                            amount: 0,
                        },
                        set_flags: true,
                    },
                    0b10 => {
                        if rd == 8 && rm == 8 {
                            ArmOp::Nop // `mov r8, r8` canonical Thumb NOP
                        } else {
                            ArmOp::Alu {
                                op: AluOp::Mov,
                                rd,
                                rn: 0,
                                rhs: ShiftedOp2::RegImm {
                                    rm,
                                    shift: ShiftOp::Lsl,
                                    amount: 0,
                                },
                                set_flags: false,
                            }
                        }
                    }
                    _ => ArmOp::BranchExchangeReg { rm, link: h1 != 0 },
                }
            } else if ((hw0 >> 11) & 0b11) == 0b01 {
                // Format 6: PC-relative load `LDR Rd, [PC, #imm8*4]`
                let rd = ((hw0 >> 8) & 0x07) as u8;
                let imm = i32::from(hw0 & 0xFF) * 4;
                ArmOp::MemSingle {
                    load: true,
                    size: MemSize::Word,
                    rd,
                    rn: 15,
                    offset: AddrOffset::Imm(imm),
                    pre_index: true,
                    writeback: false,
                    thumb_pc_align: true,
                }
            } else {
                // Formats 7 & 8: Load/Store register offset
                let ro = ((hw0 >> 6) & 0x07) as u8;
                let rb = ((hw0 >> 3) & 0x07) as u8;
                let rd = (hw0 & 0x07) as u8;
                let (load, size) = match (hw0 >> 9) & 0x07 {
                    0b000 => (false, MemSize::Word),
                    0b001 => (false, MemSize::Halfword),
                    0b010 => (false, MemSize::Byte),
                    0b011 => (true, MemSize::SignedByte),
                    0b100 => (true, MemSize::Word),
                    0b101 => (true, MemSize::Halfword),
                    0b110 => (true, MemSize::Byte),
                    _ => (true, MemSize::SignedHalfword),
                };
                ArmOp::MemSingle {
                    load,
                    size,
                    rd,
                    rn: rb,
                    offset: AddrOffset::Reg {
                        rm: ro,
                        shift: ShiftOp::Lsl,
                        amount: 0,
                        add: true,
                    },
                    pre_index: true,
                    writeback: false,
                    thumb_pc_align: false,
                }
            }
        }

        // Format 9: Load/store word/byte with immediate5
        0b011 => {
            let byte = ((hw0 >> 12) & 1) != 0;
            let load = ((hw0 >> 11) & 1) != 0;
            let imm5 = i32::from((hw0 >> 6) & 0x1F);
            let rb = ((hw0 >> 3) & 0x07) as u8;
            let rd = (hw0 & 0x07) as u8;
            let scaled = if byte { imm5 } else { imm5 * 4 };
            ArmOp::MemSingle {
                load,
                size: if byte { MemSize::Byte } else { MemSize::Word },
                rd,
                rn: rb,
                offset: AddrOffset::Imm(scaled),
                pre_index: true,
                writeback: false,
                thumb_pc_align: false,
            }
        }

        // Formats 10 & 11: Halfword immediate & SP-relative load/store
        0b100 => {
            if ((hw0 >> 12) & 1) == 0 {
                let load = ((hw0 >> 11) & 1) != 0;
                let imm5 = i32::from((hw0 >> 6) & 0x1F) * 2;
                let rb = ((hw0 >> 3) & 0x07) as u8;
                let rd = (hw0 & 0x07) as u8;
                ArmOp::MemSingle {
                    load,
                    size: MemSize::Halfword,
                    rd,
                    rn: rb,
                    offset: AddrOffset::Imm(imm5),
                    pre_index: true,
                    writeback: false,
                    thumb_pc_align: false,
                }
            } else {
                let load = ((hw0 >> 11) & 1) != 0;
                let rd = ((hw0 >> 8) & 0x07) as u8;
                let imm8 = i32::from(hw0 & 0xFF) * 4;
                ArmOp::MemSingle {
                    load,
                    size: MemSize::Word,
                    rd,
                    rn: 13,
                    offset: AddrOffset::Imm(imm8),
                    pre_index: true,
                    writeback: false,
                    thumb_pc_align: false,
                }
            }
        }

        // Formats 12, 13, 14
        0b101 => {
            if ((hw0 >> 12) & 1) == 0 {
                // Format 12: `ADD Rd, PC/SP, #imm8*4`
                let is_sp = ((hw0 >> 11) & 1) != 0;
                let rd = ((hw0 >> 8) & 0x07) as u8;
                let imm = u32::from(hw0 & 0xFF) * 4;
                if is_sp {
                    ArmOp::Alu {
                        op: AluOp::Add,
                        rd,
                        rn: 13,
                        rhs: ShiftedOp2::Imm {
                            value: imm,
                            rot_carry: None,
                        },
                        set_flags: false,
                    }
                } else {
                    // `ADR Rd, label` resolves `(addr + 4) & !3 + imm` statically!
                    let target_val = (addr.wrapping_add(4) & !3).wrapping_add(imm);
                    ArmOp::Alu {
                        op: AluOp::Mov,
                        rd,
                        rn: 0,
                        rhs: ShiftedOp2::Imm {
                            value: target_val,
                            rot_carry: None,
                        },
                        set_flags: false,
                    }
                }
            } else if ((hw0 >> 8) & 0x0F) == 0b0000 {
                // Format 13: `ADD/SUB SP, #imm7*4`
                let is_sub = ((hw0 >> 7) & 1) != 0;
                let imm = u32::from(hw0 & 0x7F) * 4;
                ArmOp::Alu {
                    op: if is_sub { AluOp::Sub } else { AluOp::Add },
                    rd: 13,
                    rn: 13,
                    rhs: ShiftedOp2::Imm {
                        value: imm,
                        rot_carry: None,
                    },
                    set_flags: false,
                }
            } else if ((hw0 >> 9) & 0b011) == 0b010 {
                // Format 14: PUSH / POP
                let load = ((hw0 >> 11) & 1) != 0;
                let extra = ((hw0 >> 8) & 1) != 0;
                let mut mask = hw0 & 0x00FF;
                if extra {
                    mask |= if load { 1 << 15 } else { 1 << 14 };
                }
                ArmOp::MemMultiple {
                    load,
                    rn: 13,
                    reg_mask: mask,
                    increment: load,
                    before: !load,
                    writeback: true,
                    user_bank: false,
                }
            } else {
                ArmOp::Unknown { raw }
            }
        }

        // Formats 15, 16, 17
        0b110 => {
            if ((hw0 >> 12) & 1) == 0 {
                // Format 15: LDMIA / STMIA Rn!, {rlist}
                let load = ((hw0 >> 11) & 1) != 0;
                let rn = ((hw0 >> 8) & 0x07) as u8;
                let mask = hw0 & 0x00FF;
                let writeback = !load || ((mask & (1 << rn)) == 0);
                ArmOp::MemMultiple {
                    load,
                    rn,
                    reg_mask: mask,
                    increment: true,
                    before: false,
                    writeback,
                    user_bank: false,
                }
            } else {
                let c_bits = ((hw0 >> 8) & 0x0F) as u8;
                if c_bits == 0x0F {
                    ArmOp::Swi {
                        comment: u32::from(hw0 & 0xFF),
                    }
                } else if c_bits == 0x0E {
                    ArmOp::Unknown { raw }
                } else {
                    cond = Cond::from_u4(c_bits);
                    let simm8 = i32::from((hw0 & 0xFF) as i8) * 2;
                    let target = addr.wrapping_add(4).wrapping_add(simm8 as u32);
                    ArmOp::Branch {
                        target,
                        link: false,
                        exchange_to: None,
                    }
                }
            }
        }

        // Format 18: Unconditional branch `B label`
        0b111 => {
            if ((hw0 >> 11) & 0b11) == 0b00 {
                let simm11 = (((i32::from(hw0 & 0x07FF)) << 21) >> 20) as u32;
                let target = addr.wrapping_add(4).wrapping_add(simm11);
                ArmOp::Branch {
                    target,
                    link: false,
                    exchange_to: None,
                }
            } else {
                ArmOp::Unknown { raw }
            }
        }

        _ => ArmOp::Unknown { raw },
    };

    DecodedInst {
        addr,
        mode: CpuMode::Thumb,
        size: 2,
        raw,
        cond,
        op,
    }
}

fn decode_thumb_alu_fmt4(sub: u8, rd: u8, rm: u8) -> ArmOp {
    match sub & 0x0F {
        0x0 => reg_alu(AluOp::And, rd, rd, rm),
        0x1 => reg_alu(AluOp::Eor, rd, rd, rm),
        0x2 => shift_reg_alu(ShiftOp::Lsl, rd, rm),
        0x3 => shift_reg_alu(ShiftOp::Lsr, rd, rm),
        0x4 => shift_reg_alu(ShiftOp::Asr, rd, rm),
        0x5 => reg_alu(AluOp::Adc, rd, rd, rm),
        0x6 => reg_alu(AluOp::Sbc, rd, rd, rm),
        0x7 => shift_reg_alu(ShiftOp::Ror, rd, rm),
        0x8 => reg_alu(AluOp::Tst, 0, rd, rm),
        0x9 => ArmOp::Alu {
            // `NEG Rd, Rm` == `RSBS Rd, Rm, #0`
            op: AluOp::Rsb,
            rd,
            rn: rm,
            rhs: ShiftedOp2::Imm {
                value: 0,
                rot_carry: None,
            },
            set_flags: true,
        },
        0xA => reg_alu(AluOp::Cmp, 0, rd, rm),
        0xB => reg_alu(AluOp::Cmn, 0, rd, rm),
        0xC => reg_alu(AluOp::Orr, rd, rd, rm),
        0xD => ArmOp::Mul {
            accumulate: false,
            rd,
            rm,
            rs: rd,
            rn: 0,
            set_flags: true,
        },
        0xE => reg_alu(AluOp::Bic, rd, rd, rm),
        _ => reg_alu(AluOp::Mvn, rd, 0, rm),
    }
}

fn reg_alu(op: AluOp, rd: u8, rn: u8, rm: u8) -> ArmOp {
    ArmOp::Alu {
        op,
        rd,
        rn,
        rhs: ShiftedOp2::RegImm {
            rm,
            shift: ShiftOp::Lsl,
            amount: 0,
        },
        set_flags: true,
    }
}

fn shift_reg_alu(shift: ShiftOp, rd: u8, rs: u8) -> ArmOp {
    ArmOp::Alu {
        op: AluOp::Mov,
        rd,
        rn: 0,
        rhs: ShiftedOp2::RegReg { rm: rd, shift, rs },
        set_flags: true,
    }
}
