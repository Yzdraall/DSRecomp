/// Architectural cycle budget per 60 Hz Nintendo DS frame (`~67.028 MHz / 59.826 Hz` scaled to instruction ticks).
pub const FRAME_INSTRUCTION_BUDGET: u64 = 250_000;
pub const MAX_CALL_NESTING: u32 = 48;

/// ARM946E-S CPU register file, condition flags, and AOT execution counters.
#[derive(Debug, Clone)]
pub struct CpuContext {
    pub regs: [u32; 16],
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    pub irq_disabled: bool,
    pub thumb: bool,
    pub spsr: u32,
    pub halted: bool,
    pub vblank_requested: bool,
    pub frame_ticks: u64,
    pub total_ticks: u64,
    pub aot_blocks_executed: u64,
    pub fallback_steps_executed: u64,
    pub call_depth: u32,
}

impl Default for CpuContext {
    fn default() -> Self {
        let mut regs = [0u32; 16];
        regs[13] = 0x0380_3F80; // Standard NitroSDK DTCM stack pointer
        regs[14] = 0x0200_0000;
        regs[15] = 0x0200_0800;
        Self {
            regs,
            n: false,
            z: false,
            c: false,
            v: false,
            irq_disabled: false,
            thumb: false,
            spsr: 0x0000_001F,
            halted: false,
            vblank_requested: false,
            frame_ticks: 0,
            total_ticks: 0,
            aot_blocks_executed: 0,
            fallback_steps_executed: 0,
            call_depth: 0,
        }
    }
}

impl CpuContext {
    pub fn with_entry(entry_raw: u32) -> Self {
        let mut ctx = Self::default();
        ctx.write_pc_interworking(entry_raw);
        ctx
    }

    #[inline]
    pub fn step_tick(&mut self, ticks: u64) {
        self.frame_ticks = self.frame_ticks.wrapping_add(ticks);
        self.total_ticks = self.total_ticks.wrapping_add(ticks);
    }

    #[inline]
    pub fn should_yield_frame(&self) -> bool {
        self.halted || self.vblank_requested || self.frame_ticks >= FRAME_INSTRUCTION_BUDGET
    }

    #[inline]
    pub fn should_break_aot(&self, expected_pc: u32, expected_thumb: bool) -> bool {
        self.should_yield_frame() || self.regs[15] != expected_pc || self.thumb != expected_thumb
    }

    #[inline]
    pub fn check_cond(&self, cond_code: u8) -> bool {
        match cond_code & 0x0F {
            0x0 => self.z,                        // EQ
            0x1 => !self.z,                       // NE
            0x2 => self.c,                        // CS / HS
            0x3 => !self.c,                       // CC / LO
            0x4 => self.n,                        // MI
            0x5 => !self.n,                       // PL
            0x6 => self.v,                        // VS
            0x7 => !self.v,                       // VC
            0x8 => self.c && !self.z,             // HI
            0x9 => !self.c || self.z,             // LS
            0xA => self.n == self.v,              // GE
            0xB => self.n != self.v,              // LT
            0xC => !self.z && (self.n == self.v), // GT
            0xD => self.z || (self.n != self.v),  // LE
            0xE => true,                          // AL
            _ => false,                           // NV
        }
    }

    #[inline]
    pub fn shift_imm_pair(&self, val: u32, shift_code: u8, amount: u8) -> (u32, bool) {
        match shift_code {
            // LSL
            0 => {
                if amount == 0 {
                    (val, self.c)
                } else if amount < 32 {
                    let c = ((val >> (32 - amount)) & 1) != 0;
                    (val << amount, c)
                } else if amount == 32 {
                    (0, (val & 1) != 0)
                } else {
                    (0, false)
                }
            }
            // LSR
            1 => {
                if amount == 0 || amount == 32 {
                    (0, ((val >> 31) & 1) != 0)
                } else if amount < 32 {
                    let c = ((val >> (amount - 1)) & 1) != 0;
                    (val >> amount, c)
                } else {
                    (0, false)
                }
            }
            // ASR
            2 => {
                let sval = val as i32;
                if amount == 0 || amount >= 32 {
                    let bit = (val >> 31) & 1;
                    (if bit != 0 { u32::MAX } else { 0 }, bit != 0)
                } else {
                    let c = ((val >> (amount - 1)) & 1) != 0;
                    ((sval >> amount) as u32, c)
                }
            }
            // ROR
            3 => {
                let eff = u32::from(amount) & 31;
                if amount == 0 {
                    (val, self.c)
                } else if eff == 0 {
                    (val, ((val >> 31) & 1) != 0)
                } else {
                    let res = val.rotate_right(eff);
                    (res, ((res >> 31) & 1) != 0)
                }
            }
            // RRX
            _ => {
                let c_in = u32::from(self.c) << 31;
                ((val >> 1) | c_in, (val & 1) != 0)
            }
        }
    }

    #[inline]
    pub fn shift_imm_val(&self, val: u32, shift_code: u8, amount: u8) -> u32 {
        self.shift_imm_pair(val, shift_code, amount).0
    }

    #[inline]
    pub fn shift_imm_carry(&self, val: u32, shift_code: u8, amount: u8) -> bool {
        self.shift_imm_pair(val, shift_code, amount).1
    }

    #[inline]
    pub fn shift_reg_pair(&self, val: u32, shift_code: u8, rs: u32) -> (u32, bool) {
        let amt = (rs & 0xFF) as u8;
        if amt == 0 {
            return (val, self.c);
        }
        match shift_code {
            0 => {
                if amt < 32 {
                    (val << amt, ((val >> (32 - amt)) & 1) != 0)
                } else if amt == 32 {
                    (0, (val & 1) != 0)
                } else {
                    (0, false)
                }
            }
            1 => {
                if amt < 32 {
                    (val >> amt, ((val >> (amt - 1)) & 1) != 0)
                } else if amt == 32 {
                    (0, ((val >> 31) & 1) != 0)
                } else {
                    (0, false)
                }
            }
            2 => {
                if amt < 32 {
                    (((val as i32) >> amt) as u32, ((val >> (amt - 1)) & 1) != 0)
                } else {
                    let neg = ((val >> 31) & 1) != 0;
                    (if neg { u32::MAX } else { 0 }, neg)
                }
            }
            _ => {
                let rot = u32::from(amt) & 31;
                if rot == 0 {
                    (val, ((val >> 31) & 1) != 0)
                } else {
                    let res = val.rotate_right(rot);
                    (res, ((res >> 31) & 1) != 0)
                }
            }
        }
    }

    #[inline]
    pub fn shift_reg_val(&self, val: u32, shift_code: u8, rs: u32) -> u32 {
        self.shift_reg_pair(val, shift_code, rs).0
    }

    #[inline]
    pub fn shift_reg_carry(&self, val: u32, shift_code: u8, rs: u32) -> bool {
        self.shift_reg_pair(val, shift_code, rs).1
    }

    #[inline]
    pub fn exec_alu(
        &mut self,
        op_code: u8,
        rn: u32,
        rhs: u32,
        shift_carry: bool,
        set_flags: bool,
    ) -> Option<u32> {
        match op_code & 0x0F {
            0x0 => Some(self.finish_logic(rn & rhs, shift_carry, set_flags)), // AND
            0x1 => Some(self.finish_logic(rn ^ rhs, shift_carry, set_flags)), // EOR
            0x2 => Some(self.finish_sub(rn, rhs, true, set_flags)),           // SUB
            0x3 => Some(self.finish_sub(rhs, rn, true, set_flags)),           // RSB
            0x4 => Some(self.finish_add(rn, rhs, false, set_flags)),          // ADD
            0x5 => Some(self.finish_add(rn, rhs, self.c, set_flags)),         // ADC
            0x6 => Some(self.finish_sub(rn, rhs, self.c, set_flags)),         // SBC
            0x7 => Some(self.finish_sub(rhs, rn, self.c, set_flags)),         // RSC
            0x8 => {
                self.finish_logic(rn & rhs, shift_carry, true); // TST
                None
            }
            0x9 => {
                self.finish_logic(rn ^ rhs, shift_carry, true); // TEQ
                None
            }
            0xA => {
                self.finish_sub(rn, rhs, true, true); // CMP
                None
            }
            0xB => {
                self.finish_add(rn, rhs, false, true); // CMN
                None
            }
            0xC => Some(self.finish_logic(rn | rhs, shift_carry, set_flags)), // ORR
            0xD => Some(self.finish_logic(rhs, shift_carry, set_flags)),      // MOV
            0xE => Some(self.finish_logic(rn & !rhs, shift_carry, set_flags)), // BIC
            _ => Some(self.finish_logic(!rhs, shift_carry, set_flags)),       // MVN
        }
    }

    #[inline]
    fn finish_logic(&mut self, res: u32, carry: bool, set_flags: bool) -> u32 {
        if set_flags {
            self.n = (res >> 31) != 0;
            self.z = res == 0;
            self.c = carry;
        }
        res
    }

    #[inline]
    fn finish_add(&mut self, a: u32, b: u32, carry_in: bool, set_flags: bool) -> u32 {
        let full = u64::from(a) + u64::from(b) + u64::from(u8::from(carry_in));
        let res = full as u32;
        if set_flags {
            self.n = (res >> 31) != 0;
            self.z = res == 0;
            self.c = (full >> 32) != 0;
            self.v = ((!(a ^ b)) & (a ^ res) & 0x8000_0000) != 0;
        }
        res
    }

    #[inline]
    fn finish_sub(&mut self, a: u32, b: u32, carry_in: bool, set_flags: bool) -> u32 {
        // ARM subtraction: `a - b - (!carry_in)` == `a + (!b) + carry_in`
        let full = u64::from(a) + u64::from(!b) + u64::from(u8::from(carry_in));
        let res = full as u32;
        if set_flags {
            self.n = (res >> 31) != 0;
            self.z = res == 0;
            self.c = (full >> 32) != 0;
            self.v = ((a ^ b) & (a ^ res) & 0x8000_0000) != 0;
        }
        res
    }

    pub fn exec_mul(
        &mut self,
        accumulate: bool,
        rd: usize,
        rm: usize,
        rs: usize,
        rn: usize,
        set_flags: bool,
    ) {
        let mut res = self.regs[rm].wrapping_mul(self.regs[rs]);
        if accumulate {
            res = res.wrapping_add(self.regs[rn]);
        }
        self.regs[rd] = res;
        if set_flags {
            self.n = (res >> 31) != 0;
            self.z = res == 0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exec_mull(
        &mut self,
        signed: bool,
        accumulate: bool,
        rd_hi: usize,
        rd_lo: usize,
        rm: usize,
        rs: usize,
        set_flags: bool,
    ) {
        let prod: u64 = if signed {
            (i64::from(self.regs[rm] as i32)).wrapping_mul(i64::from(self.regs[rs] as i32)) as u64
        } else {
            u64::from(self.regs[rm]).wrapping_mul(u64::from(self.regs[rs]))
        };
        let acc = if accumulate {
            (u64::from(self.regs[rd_hi]) << 32) | u64::from(self.regs[rd_lo])
        } else {
            0
        };
        let total = prod.wrapping_add(acc);
        self.regs[rd_lo] = total as u32;
        self.regs[rd_hi] = (total >> 32) as u32;
        if set_flags {
            self.n = (total >> 63) != 0;
            self.z = total == 0;
        }
    }

    #[inline]
    pub fn write_pc_alu(&mut self, val: u32) {
        self.regs[15] = if self.thumb { val & !1 } else { val & !3 };
    }

    #[inline]
    pub fn write_pc_interworking(&mut self, val: u32) {
        self.thumb = (val & 1) != 0;
        self.regs[15] = if self.thumb { val & !1 } else { val & !3 };
    }

    pub fn read_psr(&self, spsr: bool) -> u32 {
        if spsr {
            return self.spsr;
        }
        let mut cpsr = 0x0000_001Fu32; // System mode
        if self.n {
            cpsr |= 1 << 31;
        }
        if self.z {
            cpsr |= 1 << 30;
        }
        if self.c {
            cpsr |= 1 << 29;
        }
        if self.v {
            cpsr |= 1 << 28;
        }
        if self.irq_disabled {
            cpsr |= 1 << 7;
        }
        if self.thumb {
            cpsr |= 1 << 5;
        }
        cpsr
    }

    pub fn write_psr(&mut self, spsr: bool, mask: u8, val: u32) {
        if spsr {
            self.spsr = val;
            return;
        }
        if (mask & 0x8) != 0 {
            self.n = ((val >> 31) & 1) != 0;
            self.z = ((val >> 30) & 1) != 0;
            self.c = ((val >> 29) & 1) != 0;
            self.v = ((val >> 28) & 1) != 0;
        }
        if (mask & 0x1) != 0 {
            self.irq_disabled = ((val >> 7) & 1) != 0;
            self.thumb = ((val >> 5) & 1) != 0;
        }
    }
}
