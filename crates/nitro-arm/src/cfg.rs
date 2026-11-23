use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::arm::decode_arm;
use crate::isa::{
    AddrOffset, AluOp, ArmOp, Cond, CpuMode, DecodedInst, MemSize, ShiftOp, ShiftedOp2,
};
use crate::thumb::decode_thumb;

/// Outgoing control-flow edge from a basic block.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeKind {
    Fallthrough,
    Branch { conditional: bool },
    JumpTableCase { index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CfgEdge {
    pub target: u32,
    pub mode: CpuMode,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CallTarget {
    pub caller_inst_addr: u32,
    pub target: u32,
    pub mode: CpuMode,
}

/// Discovered basic block in an `ARM9` or `Overlay` executable region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicBlock {
    pub start_addr: u32,
    pub end_addr: u32,
    pub mode: CpuMode,
    pub instructions: Vec<DecodedInst>,
    pub outgoing: Vec<CfgEdge>,
    pub calls: Vec<CallTarget>,
}

/// Resolved switch jump-table (`LDRLS PC, [PC, Rn, LSL #2]` or `ADDLS PC, PC, Rn, LSL #2`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JumpTableInfo {
    pub branch_inst_addr: u32,
    pub table_addr: u32,
    pub entries: Vec<(u32, CpuMode)>,
}

/// Discovered function grouping connected basic blocks reachable via intra-procedural edges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionUnit {
    pub entry_addr: u32,
    pub entry_mode: CpuMode,
    pub name: String,
    pub blocks: BTreeMap<u32, BasicBlock>,
    pub calls: Vec<CallTarget>,
    pub is_leaf: bool,
}

/// Complete Control-Flow Graph & static discovery report for one binary module (`arm9` or `overlay_N`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleCfg {
    pub module_name: String,
    pub base_addr: u32,
    pub code_size: u32,
    pub arm_block_count: usize,
    pub thumb_block_count: usize,
    pub total_instructions: usize,
    pub interworking_transitions: usize,
    pub jump_tables: Vec<JumpTableInfo>,
    pub functions: BTreeMap<(u32, CpuMode), FunctionUnit>,
}

/// Configuration seeds for static CFG discovery.
#[derive(Debug, Clone, Default)]
pub struct CfgConfig {
    pub extra_seeds: Vec<(u32, CpuMode)>,
    pub scan_prologues: bool,
}

impl ModuleCfg {
    /// Builds an interworking-aware ARMv5TE + Thumb Control-Flow Graph over `code` loaded at `base_addr`.
    pub fn analyze(
        module_name: impl Into<String>,
        code: &[u8],
        base_addr: u32,
        primary_entry: Option<(u32, CpuMode)>,
        cfg_opts: &CfgConfig,
    ) -> Self {
        let module_name = module_name.into();
        let end_addr = base_addr.saturating_add(code.len() as u32);

        let in_bounds = |addr: u32, min_bytes: u32| -> bool {
            addr >= base_addr && addr.saturating_add(min_bytes) <= end_addr
        };

        let mut func_seeds: BTreeSet<(u32, CpuMode)> = BTreeSet::new();
        if let Some((e_addr, e_mode)) = primary_entry {
            if in_bounds(e_addr, 2) {
                func_seeds.insert((align_for_mode(e_addr, e_mode), e_mode));
            }
        }
        for &(s_addr, s_mode) in &cfg_opts.extra_seeds {
            if in_bounds(s_addr, 2) {
                func_seeds.insert((align_for_mode(s_addr, s_mode), s_mode));
            }
        }
        if cfg_opts.scan_prologues {
            scan_common_prologues(code, base_addr, &mut func_seeds);
        }

        // Phase 1: Discover all reachable basic blocks & callees via worklist
        let mut block_leaders: BTreeSet<(u32, CpuMode)> = func_seeds.clone();
        let mut block_worklist: VecDeque<(u32, CpuMode)> = func_seeds.iter().copied().collect();
        let mut raw_blocks: BTreeMap<(u32, CpuMode), BasicBlock> = BTreeMap::new();
        let mut jump_tables: Vec<JumpTableInfo> = Vec::new();
        let mut interworking_transitions = 0usize;

        while let Some((leader_addr, mode)) = block_worklist.pop_front() {
            if raw_blocks.contains_key(&(leader_addr, mode)) {
                continue;
            }
            let min_step = if mode == CpuMode::Arm { 4 } else { 2 };
            if !in_bounds(leader_addr, min_step) {
                continue;
            }

            let mut cur_addr = leader_addr;
            let mut insts: Vec<DecodedInst> = Vec::new();
            let mut outgoing: Vec<CfgEdge> = Vec::new();
            let mut calls: Vec<CallTarget> = Vec::new();

            while in_bounds(cur_addr, min_step) {
                if cur_addr != leader_addr && block_leaders.contains(&(cur_addr, mode)) {
                    outgoing.push(CfgEdge {
                        target: cur_addr,
                        mode,
                        kind: EdgeKind::Fallthrough,
                    });
                    break;
                }

                let inst = decode_at(code, base_addr, cur_addr, mode);
                if matches!(inst.op, ArmOp::Unknown { .. }) {
                    break;
                }
                let next_addr = cur_addr.wrapping_add(u32::from(inst.size));

                // Check for ARM switch jump-table before pushing terminator
                if mode == CpuMode::Arm {
                    if let Some(jt) =
                        try_resolve_arm_jump_table(code, base_addr, end_addr, &insts, &inst)
                    {
                        for (idx, &(case_addr, case_mode)) in jt.entries.iter().enumerate() {
                            if in_bounds(case_addr, 2) {
                                outgoing.push(CfgEdge {
                                    target: case_addr,
                                    mode: case_mode,
                                    kind: EdgeKind::JumpTableCase { index: idx as u32 },
                                });
                                if block_leaders.insert((case_addr, case_mode)) {
                                    block_worklist.push_back((case_addr, case_mode));
                                }
                            }
                        }
                        jump_tables.push(jt);
                    }
                }

                insts.push(inst);

                match inst.op {
                    ArmOp::Branch {
                        target,
                        link: true,
                        exchange_to,
                    } => {
                        let callee_mode = exchange_to.unwrap_or(mode);
                        let aligned_target = align_for_mode(target, callee_mode);
                        if callee_mode != mode {
                            interworking_transitions += 1;
                        }
                        calls.push(CallTarget {
                            caller_inst_addr: inst.addr,
                            target: aligned_target,
                            mode: callee_mode,
                        });
                        if in_bounds(aligned_target, 2) {
                            func_seeds.insert((aligned_target, callee_mode));
                            if block_leaders.insert((aligned_target, callee_mode)) {
                                block_worklist.push_back((aligned_target, callee_mode));
                            }
                        }
                        // Conditional or unconditional `BL`/`BLX` falls through on return!
                        if in_bounds(next_addr, min_step) {
                            outgoing.push(CfgEdge {
                                target: next_addr,
                                mode,
                                kind: EdgeKind::Fallthrough,
                            });
                            if block_leaders.insert((next_addr, mode)) {
                                block_worklist.push_back((next_addr, mode));
                            }
                        }
                        break;
                    }
                    ArmOp::Branch {
                        target,
                        link: false,
                        exchange_to,
                    } => {
                        let dst_mode = exchange_to.unwrap_or(mode);
                        let aligned_target = align_for_mode(target, dst_mode);
                        let conditional = inst.cond != Cond::Al;
                        if in_bounds(aligned_target, 2) {
                            outgoing.push(CfgEdge {
                                target: aligned_target,
                                mode: dst_mode,
                                kind: EdgeKind::Branch { conditional },
                            });
                            if block_leaders.insert((aligned_target, dst_mode)) {
                                block_worklist.push_back((aligned_target, dst_mode));
                            }
                        }
                        if conditional && in_bounds(next_addr, min_step) {
                            outgoing.push(CfgEdge {
                                target: next_addr,
                                mode,
                                kind: EdgeKind::Fallthrough,
                            });
                            if block_leaders.insert((next_addr, mode)) {
                                block_worklist.push_back((next_addr, mode));
                            }
                        }
                        break;
                    }
                    ArmOp::BranchExchangeReg { link: true, .. } => {
                        // Indirect call (`BLX Rm`) returns to `next_addr`
                        if in_bounds(next_addr, min_step) {
                            outgoing.push(CfgEdge {
                                target: next_addr,
                                mode,
                                kind: EdgeKind::Fallthrough,
                            });
                            if block_leaders.insert((next_addr, mode)) {
                                block_worklist.push_back((next_addr, mode));
                            }
                        }
                        break;
                    }
                    _ if inst.is_unconditional_terminator() => {
                        // Check ARM interworking veneer: `LDR R12, [PC, #imm]; BX R12`
                        if let ArmOp::BranchExchangeReg { rm, link: false } = inst.op {
                            if let Some((veneer_target, veneer_mode)) =
                                resolve_constant_reg_veneer(code, base_addr, end_addr, &insts, rm)
                            {
                                if veneer_mode != mode {
                                    interworking_transitions += 1;
                                }
                                if in_bounds(veneer_target, 2) {
                                    func_seeds.insert((veneer_target, veneer_mode));
                                    if block_leaders.insert((veneer_target, veneer_mode)) {
                                        block_worklist.push_back((veneer_target, veneer_mode));
                                    }
                                }
                            }
                        }
                        break;
                    }
                    _ => {
                        cur_addr = next_addr;
                        if insts.len() >= 256 {
                            if in_bounds(cur_addr, min_step) {
                                outgoing.push(CfgEdge {
                                    target: cur_addr,
                                    mode,
                                    kind: EdgeKind::Fallthrough,
                                });
                                if block_leaders.insert((cur_addr, mode)) {
                                    block_worklist.push_back((cur_addr, mode));
                                }
                            }
                            break;
                        }
                    }
                }
            }

            if !insts.is_empty() {
                let last = insts.last().unwrap();
                let end_block_addr = last.addr.wrapping_add(u32::from(last.size));
                raw_blocks.insert(
                    (leader_addr, mode),
                    BasicBlock {
                        start_addr: leader_addr,
                        end_addr: end_block_addr,
                        mode,
                        instructions: insts,
                        outgoing,
                        calls,
                    },
                );
            }
        }

        // Split any existing blocks if a newly discovered leader landed inside their interior.
        split_blocks_at_leaders(&mut raw_blocks, &block_leaders);

        // Phase 2: Group connected same-mode blocks into `FunctionUnit`s starting from `func_seeds`
        for &k in raw_blocks.keys() {
            if func_seeds.is_empty() {
                func_seeds.insert(k);
            }
        }

        let mut functions: BTreeMap<(u32, CpuMode), FunctionUnit> = BTreeMap::new();
        for &(entry_addr, entry_mode) in &func_seeds {
            if !raw_blocks.contains_key(&(entry_addr, entry_mode)) {
                continue;
            }
            let mut fn_blocks = BTreeMap::new();
            let mut fn_calls = Vec::new();
            let mut q = VecDeque::new();
            q.push_back(entry_addr);

            while let Some(b_addr) = q.pop_front() {
                if fn_blocks.contains_key(&b_addr) {
                    continue;
                }
                let Some(blk) = raw_blocks.get(&(b_addr, entry_mode)) else {
                    continue;
                };
                fn_blocks.insert(b_addr, blk.clone());
                fn_calls.extend(blk.calls.iter().cloned());

                for edge in &blk.outgoing {
                    if edge.mode == entry_mode {
                        // Don't merge across another explicit function entry on unconditional tail jump
                        let is_other_fn = edge.target != entry_addr
                            && func_seeds.contains(&(edge.target, entry_mode))
                            && matches!(edge.kind, EdgeKind::Branch { conditional: false });
                        if !is_other_fn && !fn_blocks.contains_key(&edge.target) {
                            q.push_back(edge.target);
                        }
                    }
                }
            }

            if !fn_blocks.is_empty() {
                let suffix = match entry_mode {
                    CpuMode::Arm => "arm",
                    CpuMode::Thumb => "thumb",
                };
                let is_leaf = fn_calls.is_empty();
                functions.insert(
                    (entry_addr, entry_mode),
                    FunctionUnit {
                        entry_addr,
                        entry_mode,
                        name: format!("fn_{entry_addr:08x}_{suffix}"),
                        blocks: fn_blocks,
                        calls: fn_calls,
                        is_leaf,
                    },
                );
            }
        }

        let mut arm_block_count = 0usize;
        let mut thumb_block_count = 0usize;
        let mut total_instructions = 0usize;
        for blk in raw_blocks.values() {
            match blk.mode {
                CpuMode::Arm => arm_block_count += 1,
                CpuMode::Thumb => thumb_block_count += 1,
            }
            total_instructions += blk.instructions.len();
        }

        Self {
            module_name,
            base_addr,
            code_size: code.len() as u32,
            arm_block_count,
            thumb_block_count,
            total_instructions,
            interworking_transitions,
            jump_tables,
            functions,
        }
    }
}

fn align_for_mode(addr: u32, mode: CpuMode) -> u32 {
    match mode {
        CpuMode::Arm => addr & !3,
        CpuMode::Thumb => addr & !1,
    }
}

pub fn decode_at(code: &[u8], base_addr: u32, addr: u32, mode: CpuMode) -> DecodedInst {
    let off = (addr - base_addr) as usize;
    match mode {
        CpuMode::Arm => {
            if off + 4 > code.len() {
                return DecodedInst {
                    addr,
                    mode,
                    size: 4,
                    raw: 0,
                    cond: Cond::Al,
                    op: ArmOp::Unknown { raw: 0 },
                };
            }
            let raw = u32::from_le_bytes([code[off], code[off + 1], code[off + 2], code[off + 3]]);
            decode_arm(addr, raw)
        }
        CpuMode::Thumb => {
            if off + 2 > code.len() {
                return DecodedInst {
                    addr,
                    mode,
                    size: 2,
                    raw: 0,
                    cond: Cond::Al,
                    op: ArmOp::Unknown { raw: 0 },
                };
            }
            let hw0 = u16::from_le_bytes([code[off], code[off + 1]]);
            let hw1 = if off + 4 <= code.len() {
                Some(u16::from_le_bytes([code[off + 2], code[off + 3]]))
            } else {
                None
            };
            decode_thumb(addr, hw0, hw1)
        }
    }
}

fn scan_common_prologues(code: &[u8], base_addr: u32, seeds: &mut BTreeSet<(u32, CpuMode)>) {
    // 1. Scan 4-byte aligned ARM `STMFD SP!, {..., LR}` (`0xE92D_4xxx`)
    for (idx, chunk) in code.as_chunks::<4>().0.iter().enumerate() {
        let word = u32::from_le_bytes(*chunk);
        if (word & 0xFFFF_F000) == 0xE92D_4000 {
            let addr = base_addr.wrapping_add((idx * 4) as u32);
            seeds.insert((addr, CpuMode::Arm));
        }
    }
    // 2. Scan 2-byte aligned Thumb `PUSH {..., LR}` (`0xB5xx`) preceded by return/padding
    for (idx, chunk) in code.as_chunks::<2>().0.iter().enumerate() {
        let hw = u16::from_le_bytes(*chunk);
        if (hw & 0xFF00) == 0xB500 && (hw & 0x00F0) != 0 {
            let preceded_by_ret_or_pad = if idx == 0 {
                true
            } else {
                let prev = u16::from_le_bytes([code[(idx - 1) * 2], code[(idx - 1) * 2 + 1]]);
                prev == 0x4770 || (prev & 0xFF00) == 0xBD00 || prev == 0x0000
            };
            if preceded_by_ret_or_pad {
                let addr = base_addr.wrapping_add((idx * 2) as u32);
                seeds.insert((addr, CpuMode::Thumb));
            }
        }
    }
}

fn try_resolve_arm_jump_table(
    code: &[u8],
    base_addr: u32,
    end_addr: u32,
    prev_insts: &[DecodedInst],
    inst: &DecodedInst,
) -> Option<JumpTableInfo> {
    // Match `LDRLS PC, [PC, Rn, LSL #2]` or `ADDLS PC, PC, Rn, LSL #2` preceded by `CMP Rn, #max_idx`
    let (idx_reg, is_ldr_table) = match inst.op {
        ArmOp::MemSingle {
            load: true,
            size: MemSize::Word,
            rd: 15,
            rn: 15,
            offset:
                AddrOffset::Reg {
                    rm,
                    shift: ShiftOp::Lsl,
                    amount: 2,
                    add: true,
                },
            pre_index: true,
            ..
        } => (rm, true),
        ArmOp::Alu {
            op: AluOp::Add,
            rd: 15,
            rn: 15,
            rhs:
                ShiftedOp2::RegImm {
                    rm,
                    shift: ShiftOp::Lsl,
                    amount: 2,
                },
            ..
        } => (rm, false),
        _ => return None,
    };

    // Search backwards up to 4 instructions for `CMP idx_reg, #max_case`
    let mut max_case = None;
    for prev in prev_insts.iter().rev().take(4) {
        if let ArmOp::Alu {
            op: AluOp::Cmp,
            rn,
            rhs: ShiftedOp2::Imm { value, .. },
            ..
        } = prev.op
        {
            if rn == idx_reg && value <= 128 {
                max_case = Some(value);
                break;
            }
        }
    }
    let count = max_case? + 1;
    let table_addr = inst.addr.wrapping_add(8);
    let mut entries = Vec::with_capacity(count as usize);

    for i in 0..count {
        let entry_addr = table_addr.wrapping_add(i * 4);
        if entry_addr < base_addr || entry_addr.saturating_add(4) > end_addr {
            break;
        }
        if is_ldr_table {
            let off = (entry_addr - base_addr) as usize;
            let raw_ptr =
                u32::from_le_bytes([code[off], code[off + 1], code[off + 2], code[off + 3]]);
            let (t_addr, t_mode) = CpuMode::from_interworking_addr(raw_ptr);
            entries.push((t_addr, t_mode));
        } else {
            // `ADDLS PC, PC, Rn, LSL #2` jumps directly to the branch slot at `table_addr + i*4`
            entries.push((entry_addr, CpuMode::Arm));
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(JumpTableInfo {
            branch_inst_addr: inst.addr,
            table_addr,
            entries,
        })
    }
}

fn resolve_constant_reg_veneer(
    code: &[u8],
    base_addr: u32,
    end_addr: u32,
    insts: &[DecodedInst],
    bx_reg: u8,
) -> Option<(u32, CpuMode)> {
    if insts.len() < 2 {
        return None;
    }
    let prev = &insts[insts.len() - 2];
    if let ArmOp::MemSingle {
        load: true,
        size: MemSize::Word,
        rd,
        rn: 15,
        offset: AddrOffset::Imm(imm),
        pre_index: true,
        thumb_pc_align,
        ..
    } = prev.op
    {
        if rd == bx_reg {
            let pc_base = if thumb_pc_align {
                prev.pc_read_value() & !3
            } else {
                prev.pc_read_value()
            };
            let pool_addr = pc_base.wrapping_add(imm as u32);
            if pool_addr >= base_addr && pool_addr.saturating_add(4) <= end_addr {
                let off = (pool_addr - base_addr) as usize;
                let ptr =
                    u32::from_le_bytes([code[off], code[off + 1], code[off + 2], code[off + 3]]);
                return Some(CpuMode::from_interworking_addr(ptr));
            }
        }
    }
    None
}

fn split_blocks_at_leaders(
    blocks: &mut BTreeMap<(u32, CpuMode), BasicBlock>,
    leaders: &BTreeSet<(u32, CpuMode)>,
) {
    let keys: Vec<(u32, CpuMode)> = blocks.keys().copied().collect();
    for (start_addr, mode) in keys {
        let Some(blk) = blocks.get(&(start_addr, mode)).cloned() else {
            continue;
        };
        let split_pos = blk
            .instructions
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, inst)| leaders.contains(&(inst.addr, mode)))
            .map(|(idx, inst)| (idx, inst.addr));

        if let Some((idx, split_addr)) = split_pos {
            let mut truncated = blk;
            truncated.instructions.truncate(idx);
            truncated.end_addr = split_addr;
            truncated.calls.retain(|c| c.caller_inst_addr < split_addr);
            truncated.outgoing = vec![CfgEdge {
                target: split_addr,
                mode,
                kind: EdgeKind::Fallthrough,
            }];
            blocks.insert((start_addr, mode), truncated);
        }
    }
}
