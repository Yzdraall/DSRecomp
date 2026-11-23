use std::fmt;

use serde::{Deserialize, Serialize};

/// Execution ISA state of an ARM946E-S / ARM7TDMI core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CpuMode {
    Arm,
    Thumb,
}

impl CpuMode {
    pub fn from_interworking_addr(raw: u32) -> (u32, Self) {
        if (raw & 1) != 0 {
            (raw & !1, Self::Thumb)
        } else {
            (raw & !3, Self::Arm)
        }
    }
}

/// 4-bit ARM condition field (`bits[31:28]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cond {
    Eq = 0,
    Ne = 1,
    Cs = 2,
    Cc = 3,
    Mi = 4,
    Pl = 5,
    Vs = 6,
    Vc = 7,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
    Al = 14,
    Nv = 15,
}

impl Cond {
    pub fn from_u4(v: u8) -> Self {
        match v & 0x0F {
            0 => Self::Eq,
            1 => Self::Ne,
            2 => Self::Cs,
            3 => Self::Cc,
            4 => Self::Mi,
            5 => Self::Pl,
            6 => Self::Vs,
            7 => Self::Vc,
            8 => Self::Hi,
            9 => Self::Ls,
            10 => Self::Ge,
            11 => Self::Lt,
            12 => Self::Gt,
            13 => Self::Le,
            14 => Self::Al,
            _ => Self::Nv,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Cs => "cs",
            Self::Cc => "cc",
            Self::Mi => "mi",
            Self::Pl => "pl",
            Self::Vs => "vs",
            Self::Vc => "vc",
            Self::Hi => "hi",
            Self::Ls => "ls",
            Self::Ge => "ge",
            Self::Lt => "lt",
            Self::Gt => "gt",
            Self::Le => "le",
            Self::Al | Self::Nv => "",
        }
    }
}

/// Barrel-shifter operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShiftOp {
    Lsl,
    Lsr,
    Asr,
    Ror,
    Rrx,
}

impl ShiftOp {
    pub fn from_u2(code: u8, imm_amount: Option<u8>) -> (Self, u8) {
        match code & 3 {
            0 => (Self::Lsl, imm_amount.unwrap_or(0)),
            1 => (
                Self::Lsr,
                match imm_amount {
                    Some(0) => 32,
                    Some(n) => n,
                    None => 0,
                },
            ),
            2 => (
                Self::Asr,
                match imm_amount {
                    Some(0) => 32,
                    Some(n) => n,
                    None => 0,
                },
            ),
            _ => match imm_amount {
                Some(0) => (Self::Rrx, 1),
                Some(n) => (Self::Ror, n),
                None => (Self::Ror, 0),
            },
        }
    }
}

/// Second operand (`Operand2`) for ARM/Thumb data-processing instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShiftedOp2 {
    Imm { value: u32, rot_carry: Option<bool> },
    RegImm { rm: u8, shift: ShiftOp, amount: u8 },
    RegReg { rm: u8, shift: ShiftOp, rs: u8 },
}

/// Standard 16 ARM data-processing opcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AluOp {
    And = 0,
    Eor = 1,
    Sub = 2,
    Rsb = 3,
    Add = 4,
    Adc = 5,
    Sbc = 6,
    Rsc = 7,
    Tst = 8,
    Teq = 9,
    Cmp = 10,
    Cmn = 11,
    Orr = 12,
    Mov = 13,
    Bic = 14,
    Mvn = 15,
}

impl AluOp {
    pub fn from_u4(v: u8) -> Self {
        match v & 0x0F {
            0 => Self::And,
            1 => Self::Eor,
            2 => Self::Sub,
            3 => Self::Rsb,
            4 => Self::Add,
            5 => Self::Adc,
            6 => Self::Sbc,
            7 => Self::Rsc,
            8 => Self::Tst,
            9 => Self::Teq,
            10 => Self::Cmp,
            11 => Self::Cmn,
            12 => Self::Orr,
            13 => Self::Mov,
            14 => Self::Bic,
            _ => Self::Mvn,
        }
    }

    pub fn is_test_only(self) -> bool {
        matches!(self, Self::Tst | Self::Teq | Self::Cmp | Self::Cmn)
    }

    pub fn mnemonic(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Eor => "eor",
            Self::Sub => "sub",
            Self::Rsb => "rsb",
            Self::Add => "add",
            Self::Adc => "adc",
            Self::Sbc => "sbc",
            Self::Rsc => "rsc",
            Self::Tst => "tst",
            Self::Teq => "teq",
            Self::Cmp => "cmp",
            Self::Cmn => "cmn",
            Self::Orr => "orr",
            Self::Mov => "mov",
            Self::Bic => "bic",
            Self::Mvn => "mvn",
        }
    }
}

/// Memory access width & sign extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemSize {
    Word,
    Halfword,
    SignedHalfword,
    Byte,
    SignedByte,
    Doubleword,
}

/// Memory offset calculation for `MemSingle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AddrOffset {
    Imm(i32),
    Reg {
        rm: u8,
        shift: ShiftOp,
        amount: u8,
        add: bool,
    },
}

/// Unified decoded `ARMv5TE` / `Thumb` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArmOp {
    Nop,
    Alu {
        op: AluOp,
        rd: u8,
        rn: u8,
        rhs: ShiftedOp2,
        set_flags: bool,
    },
    Mul {
        accumulate: bool,
        rd: u8,
        rm: u8,
        rs: u8,
        rn: u8,
        set_flags: bool,
    },
    Mull {
        signed: bool,
        accumulate: bool,
        rd_hi: u8,
        rd_lo: u8,
        rm: u8,
        rs: u8,
        set_flags: bool,
    },
    Clz {
        rd: u8,
        rm: u8,
    },
    Branch {
        target: u32,
        link: bool,
        exchange_to: Option<CpuMode>,
    },
    BranchExchangeReg {
        rm: u8,
        link: bool,
    },
    MemSingle {
        load: bool,
        size: MemSize,
        rd: u8,
        rn: u8,
        offset: AddrOffset,
        pre_index: bool,
        writeback: bool,
        thumb_pc_align: bool,
    },
    MemMultiple {
        load: bool,
        rn: u8,
        reg_mask: u16,
        increment: bool,
        before: bool,
        writeback: bool,
        user_bank: bool,
    },
    Mrs {
        rd: u8,
        spsr: bool,
    },
    Msr {
        spsr: bool,
        mask: u8,
        rhs: ShiftedOp2,
    },
    Swp {
        byte: bool,
        rd: u8,
        rm: u8,
        rn: u8,
    },
    Swi {
        comment: u32,
    },
    CoprocMove {
        to_coproc: bool,
        cp: u8,
        op1: u8,
        rd: u8,
        crn: u8,
        crm: u8,
        op2: u8,
    },
    Unknown {
        raw: u32,
    },
}

/// Decoded instruction with address, ISA state, byte width, and condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedInst {
    pub addr: u32,
    pub mode: CpuMode,
    pub size: u8,
    pub raw: u32,
    pub cond: Cond,
    pub op: ArmOp,
}

impl DecodedInst {
    /// Architectural PC value seen when reading `R15` during execution of this instruction.
    pub fn pc_read_value(&self) -> u32 {
        match self.mode {
            CpuMode::Arm => self.addr.wrapping_add(8),
            CpuMode::Thumb => self.addr.wrapping_add(4),
        }
    }

    /// Returns `true` if this instruction unconditionally alters `R15` (`PC`).
    pub fn is_unconditional_terminator(&self) -> bool {
        if self.cond != Cond::Al {
            return false;
        }
        match self.op {
            ArmOp::Branch { link: false, .. } => true,
            ArmOp::BranchExchangeReg { link: false, .. } => true,
            ArmOp::Alu { op, rd: 15, .. } if !op.is_test_only() => true,
            ArmOp::MemSingle {
                load: true, rd: 15, ..
            } => true,
            ArmOp::MemMultiple {
                load: true,
                reg_mask,
                ..
            } if (reg_mask & (1 << 15)) != 0 => true,
            _ => false,
        }
    }
}

pub fn reg_name(r: u8) -> &'static str {
    match r {
        0 => "r0",
        1 => "r1",
        2 => "r2",
        3 => "r3",
        4 => "r4",
        5 => "r5",
        6 => "r6",
        7 => "r7",
        8 => "r8",
        9 => "r9",
        10 => "r10",
        11 => "r11",
        12 => "r12",
        13 => "sp",
        14 => "lr",
        15 => "pc",
        _ => "r?",
    }
}

impl fmt::Display for DecodedInst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.cond.suffix();
        match self.op {
            ArmOp::Nop => write!(f, "nop{c}"),
            ArmOp::Alu {
                op,
                rd,
                rn,
                rhs,
                set_flags,
            } => {
                let s = if set_flags && !op.is_test_only() {
                    "s"
                } else {
                    ""
                };
                match op {
                    AluOp::Mov | AluOp::Mvn => {
                        write!(f, "{}{c}{s} {}, ", op.mnemonic(), reg_name(rd))?;
                    }
                    op if op.is_test_only() => {
                        write!(f, "{}{c} {}, ", op.mnemonic(), reg_name(rn))?;
                    }
                    _ => {
                        write!(
                            f,
                            "{}{c}{s} {}, {}, ",
                            op.mnemonic(),
                            reg_name(rd),
                            reg_name(rn)
                        )?;
                    }
                }
                fmt_op2(f, rhs)
            }
            ArmOp::Mul {
                accumulate: false,
                rd,
                rm,
                rs,
                set_flags,
                ..
            } => {
                let s = if set_flags { "s" } else { "" };
                write!(
                    f,
                    "mul{c}{s} {}, {}, {}",
                    reg_name(rd),
                    reg_name(rm),
                    reg_name(rs)
                )
            }
            ArmOp::Mul {
                accumulate: true,
                rd,
                rm,
                rs,
                rn,
                set_flags,
            } => {
                let s = if set_flags { "s" } else { "" };
                write!(
                    f,
                    "mla{c}{s} {}, {}, {}, {}",
                    reg_name(rd),
                    reg_name(rm),
                    reg_name(rs),
                    reg_name(rn)
                )
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
                let mnem = match (signed, accumulate) {
                    (false, false) => "umull",
                    (false, true) => "umlal",
                    (true, false) => "smull",
                    (true, true) => "smlal",
                };
                let s = if set_flags { "s" } else { "" };
                write!(
                    f,
                    "{mnem}{c}{s} {}, {}, {}, {}",
                    reg_name(rd_lo),
                    reg_name(rd_hi),
                    reg_name(rm),
                    reg_name(rs)
                )
            }
            ArmOp::Clz { rd, rm } => write!(f, "clz{c} {}, {}", reg_name(rd), reg_name(rm)),
            ArmOp::Branch {
                target,
                link,
                exchange_to,
            } => {
                let mnem = match (link, exchange_to) {
                    (_, Some(_)) => "blx",
                    (true, None) => "bl",
                    (false, None) => "b",
                };
                write!(f, "{mnem}{c} 0x{target:08X}")
            }
            ArmOp::BranchExchangeReg { rm, link } => {
                let mnem = if link { "blx" } else { "bx" };
                write!(f, "{mnem}{c} {}", reg_name(rm))
            }
            ArmOp::MemSingle {
                load,
                size,
                rd,
                rn,
                offset,
                ..
            } => {
                let base_m = if load { "ldr" } else { "str" };
                let sz = match size {
                    MemSize::Word => "",
                    MemSize::Halfword => "h",
                    MemSize::SignedHalfword => "sh",
                    MemSize::Byte => "b",
                    MemSize::SignedByte => "sb",
                    MemSize::Doubleword => "d",
                };
                match offset {
                    AddrOffset::Imm(0) => {
                        write!(f, "{base_m}{c}{sz} {}, [{}]", reg_name(rd), reg_name(rn))
                    }
                    AddrOffset::Imm(imm) => {
                        write!(
                            f,
                            "{base_m}{c}{sz} {}, [{}, #{imm}]",
                            reg_name(rd),
                            reg_name(rn)
                        )
                    }
                    AddrOffset::Reg { rm, add, .. } => {
                        let sign = if add { "" } else { "-" };
                        write!(
                            f,
                            "{base_m}{c}{sz} {}, [{}, {sign}{}]",
                            reg_name(rd),
                            reg_name(rn),
                            reg_name(rm)
                        )
                    }
                }
            }
            ArmOp::MemMultiple {
                load,
                rn,
                reg_mask,
                writeback,
                ..
            } => {
                let mnem = if load { "ldm" } else { "stm" };
                let wb = if writeback { "!" } else { "" };
                write!(f, "{mnem}{c} {}{wb}, {{0x{reg_mask:04X}}}", reg_name(rn))
            }
            ArmOp::Mrs { rd, spsr } => {
                let psr = if spsr { "spsr" } else { "cpsr" };
                write!(f, "mrs{c} {}, {psr}", reg_name(rd))
            }
            ArmOp::Msr { spsr, rhs, .. } => {
                let psr = if spsr { "spsr" } else { "cpsr" };
                write!(f, "msr{c} {psr}, ")?;
                fmt_op2(f, rhs)
            }
            ArmOp::Swp { byte, rd, rm, rn } => {
                let b = if byte { "b" } else { "" };
                write!(
                    f,
                    "swp{c}{b} {}, {}, [{}]",
                    reg_name(rd),
                    reg_name(rm),
                    reg_name(rn)
                )
            }
            ArmOp::Swi { comment } => write!(f, "swi{c} 0x{comment:X}"),
            ArmOp::CoprocMove {
                to_coproc,
                cp,
                rd,
                crn,
                crm,
                op1,
                op2,
            } => {
                let mnem = if to_coproc { "mcr" } else { "mrc" };
                write!(
                    f,
                    "{mnem}{c} p{cp}, {op1}, {}, c{crn}, c{crm}, {op2}",
                    reg_name(rd)
                )
            }
            ArmOp::Unknown { raw } => write!(f, ".word 0x{raw:08X}"),
        }
    }
}

fn fmt_op2(f: &mut fmt::Formatter<'_>, op2: ShiftedOp2) -> fmt::Result {
    match op2 {
        ShiftedOp2::Imm { value, .. } => write!(f, "#0x{value:X}"),
        ShiftedOp2::RegImm {
            rm,
            shift: ShiftOp::Lsl,
            amount: 0,
        } => write!(f, "{}", reg_name(rm)),
        ShiftedOp2::RegImm { rm, shift, amount } => {
            write!(f, "{}, {:?} #{amount}", reg_name(rm), shift)
        }
        ShiftedOp2::RegReg { rm, shift, rs } => {
            write!(f, "{}, {:?} {}", reg_name(rm), shift, reg_name(rs))
        }
    }
}
