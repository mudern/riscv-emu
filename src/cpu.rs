//! CPU hart：取指、执行、CSR 与异常交付、特权级、Sv39 翻译。
//!
//! 阶段 1 约定保留：M-mode `ecall` 被拦截为宿主调用（write/exit）；
//! U/S 态 ecall 走标准异常交付。

use std::io::{self, Write};

use crate::bus::Bus;
use crate::csr::{self, Csrs};
use crate::decode::{
    AluOp, AmoOp, BranchOp, CsrKind, Fmt, Inst, LoadOp, SystemOp, decode, decode_compressed,
};
use crate::exception::{Exception, TrapInfo};
use crate::mmu::{self, Access, Mmu};
use crate::pmp::PmpAccess;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Privilege {
    U = 0,
    S = 1,
    M = 3,
}

impl Privilege {
    pub fn from_bits(v: u64) -> Privilege {
        match v & 3 {
            0 => Privilege::U,
            1 => Privilege::S,
            _ => Privilege::M,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Privilege::U => "U",
            Privilege::S => "S",
            Privilege::M => "M",
        }
    }
}

pub struct Cpu {
    pub regs: [u64; 32],
    pub pc: u64,
    pub privilege: Privilege,
    pub csr: Csrs,
    pub mmu: Mmu,
    /// LR/SC 预约地址
    pub reservation: Option<u64>,
    /// ecall exit 设置
    pub exit: Option<i32>,
    pub instret: u64,
    pub trace: bool,
    /// 内置 SBI：开启后 S 态 ecall 不再走异常交付，而是按 SBI 调用处理
    pub sbi: bool,
    /// 外部中断控制器持有的中断线（PLIC）：MEIP/SEIP 位，运行循环每步刷新。
    /// 与 mip 中软件可写的 SEIP 位合并后才是有效视图。
    pub ext_mip: u64,
    /// 浮点寄存器 f0-f31（f32 以 NaN-boxing 形式存放于高 32 位）
    pub fregs: [u64; 32],
    /// 取指页缓存：(虚拟页, 物理页, satp, 特权级, PMP 代数)。
    /// satp 写 / sfence.vma / fence.i / PMP CSR 写时失效。
    fetch_page: Option<(u64, u64, u64, u8, u32)>,
    /// PMP 配置代数：PMP CSR 每次写递增，用于取指缓存失效
    pmp_gen: u32,
    /// 解码缓存（直接映射，按物理地址索引）
    icache: Vec<IcacheEntry>,
}

/// 解码缓存项：tag = 物理地址，raw 用于校验（防未 fence.i 的自修改代码）
#[derive(Clone, Copy)]
struct IcacheEntry {
    tag: u64,
    raw: u32,
    len: u8,
    inst: crate::decode::Inst,
}

const ICACHE_LEN: usize = 1 << 14;

type ExecResult = Result<(), (Exception, u64)>;

impl Cpu {
    pub fn new(entry: u64) -> Self {
        Cpu {
            regs: [0; 32],
            pc: entry,
            privilege: Privilege::M,
            csr: Csrs::default(),
            mmu: Mmu::new(),
            reservation: None,
            exit: None,
            instret: 0,
            trace: false,
            sbi: false,
            ext_mip: 0,
            fregs: [0; 32],
            fetch_page: None,
            pmp_gen: 0,
            icache: vec![IcacheEntry { tag: u64::MAX, raw: 0, len: 0, inst: crate::decode::Inst::Nop }; ICACHE_LEN],
        }
    }

    /// 失效取指相关缓存（fence.i / sfence.vma / satp / PMP 写）
    fn flush_fetch_caches(&mut self) {
        self.fetch_page = None;
        for e in &mut self.icache {
            e.tag = u64::MAX;
        }
    }

    fn reg(&self, idx: u8) -> u64 {
        self.regs[idx as usize]
    }

    fn set_reg(&mut self, idx: u8, val: u64) {
        if idx != 0 {
            self.regs[idx as usize] = val;
        }
    }

    /// mstatus.FS != Off
    fn fs_enabled(&self) -> bool {
        (self.csr.mstatus >> 13) & 3 != 0
    }

    /// 格式级浮点门控：S 指令要求 misa.F，D 指令要求 misa.D（且 FS != Off）
    fn fp_allowed(&self, fmt: Fmt) -> bool {
        let ext = match fmt {
            Fmt::S => 1 << 5, // F
            Fmt::D => 1 << 3, // D
        };
        self.csr.misa & ext != 0 && self.fs_enabled()
    }

    /// 浮点指令执行后置 FS=Dirty
    fn fs_mark_dirty(&mut self) {
        self.csr.mstatus |= 3 << 13;
    }

    fn freg32(&self, i: u8) -> u32 {
        let v = self.fregs[i as usize];
        if v >> 32 == 0xFFFF_FFFF {
            v as u32
        } else {
            (crate::fpu::CANON_F32) as u32
        }
    }
    fn set_freg32(&mut self, i: u8, v: u32) {
        self.fregs[i as usize] = 0xFFFF_FFFF_0000_0000 | v as u64;
    }
    fn set_freg64(&mut self, i: u8, v: u64) {
        self.fregs[i as usize] = v;
    }
    fn freg(&self, i: u8, fmt: Fmt) -> u64 {
        match fmt {
            Fmt::S => self.freg32(i) as u64,
            Fmt::D => self.fregs[i as usize],
        }
    }
    fn set_freg(&mut self, i: u8, fmt: Fmt, v: u64) {
        match fmt {
            Fmt::S => self.set_freg32(i, v as u32),
            Fmt::D => self.fregs[i as usize] = v,
        }
    }

    // ---- 地址翻译 ----

    /// 有效数据特权级（MPRV：M 态数据访问按 MPP 翻译）
    fn effective_data_priv(&self) -> Privilege {
        if self.privilege == Privilege::M && self.csr.mstatus & csr::MPRV != 0 {
            Privilege::from_bits((self.csr.mstatus & csr::MPP) >> 11)
        } else {
            self.privilege
        }
    }

    fn translate(&mut self, bus: &mut Bus, vaddr: u64, acc: Access) -> Result<u64, Exception> {
        let eff = if matches!(acc, Access::Fetch) {
            self.privilege
        } else {
            self.effective_data_priv()
        };
        if eff == Privilege::M || (self.csr.satp >> 60) != mmu::SATP_SV39 {
            return Ok(vaddr); // 裸地址
        }
        let mxr = self.csr.mstatus & csr::MXR != 0;
        let sum = self.csr.mstatus & csr::SUM != 0;
        // 走表访问也过 PMP，有效特权级为 S（规范 3.7.2）
        let pmp = &self.csr.pmp;
        self.mmu
            .translate(bus, self.csr.satp, eff, mxr, sum, vaddr, acc, pmp)
    }

    /// PMP 物理地址检查：fetch 按当前特权级、数据访问按有效特权级（MPRV）。
    /// 失败时 tval 记引发故障的虚拟地址。Ok 时原样返回 pa，便于链式调用。
    /// 页表走表访问不做 PMP 检查（与 QEMU 相同）。
    fn pmp_check(&self, pa: u64, len: u32, acc: Access, vaddr: u64) -> Result<u64, (Exception, u64)> {
        let mode = if matches!(acc, Access::Fetch) {
            self.privilege
        } else {
            self.effective_data_priv()
        };
        let pmp_acc = match acc {
            Access::Fetch => PmpAccess::Exec,
            Access::Load => PmpAccess::Read,
            Access::Store => PmpAccess::Write,
        };
        if self.csr.pmp.allows(pa, len as u64, pmp_acc, mode) {
            Ok(pa)
        } else {
            Err((
                match acc {
                    Access::Fetch => Exception::InstructionAccessFault,
                    Access::Load => Exception::LoadAccessFault,
                    Access::Store => Exception::StoreAccessFault,
                },
                vaddr,
            ))
        }
    }

    // ---- 执行 ----

    /// 翻译 + PMP 检查一个可能跨页的访问。跨页时拆成两段分别检查（异常按
    /// 先失败的那段汇报，tval 为该段的虚拟地址）。返回 (pa0, n0, pa1, n1)，
    /// 不跨页时 n1 = 0。
    fn split_access(
        &mut self,
        bus: &mut Bus,
        vaddr: u64,
        size: u32,
        acc: Access,
    ) -> Result<(u64, u32, u64, u32), (Exception, u64)> {
        if (vaddr & 0xFFF) + size as u64 <= 0x1000 {
            let pa = self.translate(bus, vaddr, acc).map_err(|e| (e, vaddr))?;
            self.pmp_check(pa, size, acc, vaddr)?;
            return Ok((pa, size, 0, 0));
        }
        let n0 = (0x1000 - (vaddr & 0xFFF)) as u32;
        let n1 = size - n0;
        let va1 = vaddr.wrapping_add(n0 as u64);
        let pa0 = self.translate(bus, vaddr, acc).map_err(|e| (e, vaddr))?;
        self.pmp_check(pa0, n0, acc, vaddr)?;
        let pa1 = self.translate(bus, va1, acc).map_err(|e| (e, va1))?;
        self.pmp_check(pa1, n1, acc, va1)?;
        Ok((pa0, n0, pa1, n1))
    }

    /// 带权限检查的 load（跨页自动拆分后按小端拼合）
    fn guest_load(
        &mut self,
        bus: &mut Bus,
        vaddr: u64,
        size: u32,
    ) -> Result<u64, (Exception, u64)> {
        let (pa0, n0, pa1, n1) = self.split_access(bus, vaddr, size, Access::Load)?;
        let lo = bus.load(pa0, n0).map_err(|e| (e, vaddr))?;
        if n1 == 0 {
            return Ok(lo);
        }
        let hi = bus.load(pa1, n1).map_err(|e| (e, vaddr.wrapping_add(n0 as u64)))?;
        Ok(lo | (hi << (n0 * 8)))
    }

    /// 带权限检查的 store（跨页自动拆分）
    fn guest_store(
        &mut self,
        bus: &mut Bus,
        vaddr: u64,
        size: u32,
        val: u64,
    ) -> Result<(), (Exception, u64)> {
        let (pa0, n0, pa1, n1) = self.split_access(bus, vaddr, size, Access::Store)?;
        bus.store(pa0, n0, val).map_err(|e| (e, vaddr))?;
        if n1 != 0 {
            bus.store(pa1, n1, val >> (n0 * 8))
                .map_err(|e| (e, vaddr.wrapping_add(n0 as u64)))?;
        }
        Ok(())
    }

    /// take_trap 结果适配：已交付 → Ok(None)（step 结束），未交付 → Err
    fn trap_out(r: Result<(), TrapInfo>) -> Result<Option<(Inst, u64, u32)>, TrapInfo> {
        match r {
            Ok(()) => Ok(None),
            Err(t) => Err(t),
        }
    }

    /// 取指翻译（带页缓存）。PMP 判定不缓存（同页可能跨规则边界），
    /// 由调用方按访问宽度逐条检查。
    fn fetch_translate(&mut self, bus: &mut Bus, pc: u64) -> Result<u64, (Exception, u64)> {
        let page = pc & !0xFFF;
        if let Some((vp, pp, sp, pv, g)) = self.fetch_page
            && vp == page
            && sp == self.csr.satp
            && pv == self.privilege as u8
            && g == self.pmp_gen
        {
            return Ok(pp | (pc & 0xFFF));
        }
        let pa = self
            .translate(bus, pc, Access::Fetch)
            .map_err(|e| (e, pc))?;
        self.pmp_check(pa, 2, Access::Fetch, pc)?;
        self.fetch_page = Some((
            page,
            pa & !0xFFF,
            self.csr.satp,
            self.privilege as u8,
            self.pmp_gen,
        ));
        Ok(pa)
    }

    /// 取指 + 解码（页缓存 + 解码缓存）。`None` = trap 已交付（step 结束）。
    fn fetch_decode(&mut self, bus: &mut Bus, pc: u64) -> Result<Option<(Inst, u64, u32)>, TrapInfo> {
        // 4 字节窗口不跨页时一次读入
        let in_page = (pc & 0xFFF) <= 0xFFC;
        let pa = match self.fetch_translate(bus, pc) {
            Ok(p) => p,
            Err((e, t)) => return Self::trap_out(self.take_trap(e, t)),
        };
        let size = if in_page { 4 } else { 2 };
        if let Err((e, t)) = self.pmp_check(pa, size, Access::Fetch, pc) {
            return Self::trap_out(self.take_trap(e, t));
        }
        let word = match bus.load(pa, size) {
            Ok(w) => w,
            Err(e) => return Self::trap_out(self.take_trap(e, pc)),
        } as u32;
        let lo = word as u16;

        // 解码缓存命中：物理地址 + 原始编码一致
        let idx = ((pa >> 1) as usize) & (ICACHE_LEN - 1);
        let cached = self.icache[idx];
        if in_page {
            if lo & 3 == 3 {
                if cached.tag == pa && cached.raw == word && cached.len == 4 {
                    return Ok(Some((cached.inst, pc + 4, word)));
                }
                match decode(word) {
                    Ok(i) => {
                        self.icache[idx] = IcacheEntry { tag: pa, raw: word, len: 4, inst: i };
                        Ok(Some((i, pc + 4, word)))
                    }
                    Err(_) => Self::trap_out(self.take_trap(Exception::IllegalInstruction, word as u64)),
                }
            } else {
                let raw = lo as u32;
                if cached.tag == pa && cached.raw == raw && cached.len == 2 {
                    return Ok(Some((cached.inst, pc + 2, raw)));
                }
                match decode_compressed(lo) {
                    Ok(i) => {
                        self.icache[idx] = IcacheEntry { tag: pa, raw, len: 2, inst: i };
                        Ok(Some((i, pc + 2, raw)))
                    }
                    Err(_) => Self::trap_out(self.take_trap(Exception::IllegalInstruction, lo as u64)),
                }
            }
        } else {
            // 页尾：跨页的 32 位指令走慢路径（第二段单独翻译）
            if lo & 3 != 3 {
                let raw = lo as u32;
                if cached.tag == pa && cached.raw == raw && cached.len == 2 {
                    return Ok(Some((cached.inst, pc + 2, raw)));
                }
                match decode_compressed(lo) {
                    Ok(i) => {
                        self.icache[idx] = IcacheEntry { tag: pa, raw, len: 2, inst: i };
                        Ok(Some((i, pc + 2, raw)))
                    }
                    Err(_) => Self::trap_out(self.take_trap(Exception::IllegalInstruction, lo as u64)),
                }
            } else {
                let pa2 = match self.fetch_translate(bus, pc.wrapping_add(2)) {
                    Ok(p) => p,
                    Err((e, t)) => return Self::trap_out(self.take_trap(e, t)),
                };
                if let Err((e, t)) = self.pmp_check(pa2, 2, Access::Fetch, pc) {
                    return Self::trap_out(self.take_trap(e, t));
                }
                let hi = match bus.load(pa2, 2) {
                    Ok(h) => h as u16,
                    Err(e) => return Self::trap_out(self.take_trap(e, pc)),
                };
                let word = lo as u32 | (hi as u32) << 16;
                match decode(word) {
                    Ok(i) => Ok(Some((i, pc + 4, word))),
                    Err(_) => Self::trap_out(self.take_trap(Exception::IllegalInstruction, word as u64)),
                }
            }
        }
    }

    /// 执行一条指令。返回 Err 表示无法交付的异常（guest 没有 handler）。
    pub fn step(&mut self, bus: &mut Bus, console: &mut Vec<u8>) -> Result<(), TrapInfo> {
        let pc = self.pc;
        if pc & 1 != 0 {
            return self.take_trap(Exception::InstructionMisaligned, pc);
        }

        let (inst, next_pc, raw) = match self.fetch_decode(bus, pc) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(()), // trap 已交付
            Err(t) => return Err(t),
        };

        if self.trace {
            println!("{pc:012x} [{}] {raw:08x} {inst:?}", self.privilege.name());
        }

        if let Err((cause, val)) = self.execute(inst, raw, pc, next_pc, bus, console) {
            return self.take_trap(cause, val);
        }
        self.instret += 1;
        // 机器计数器（cycle 按 1 CPI 近似递增；受 mcountinhibit 抑制）
        if self.csr.mcountinhibit & 1 == 0 {
            self.csr.mcycle = self.csr.mcycle.wrapping_add(1);
        }
        if self.csr.mcountinhibit & 4 == 0 {
            self.csr.minstret = self.csr.minstret.wrapping_add(1);
        }
        Ok(())
    }

    fn execute(
        &mut self,
        inst: Inst,
        raw: u32,
        pc: u64,
        next_pc: u64,
        bus: &mut Bus,
        console: &mut Vec<u8>,
    ) -> ExecResult {
        match inst {
            Inst::Nop | Inst::Fence => {}
            Inst::FenceI => {
                self.flush_fetch_caches(); // 自修改代码：fence.i 后重取
            }
            Inst::Lui { rd, imm } => self.set_reg(rd, imm as u64),
            Inst::Auipc { rd, imm } => self.set_reg(rd, pc.wrapping_add(imm as u64)),
            Inst::Jal { rd, imm } => {
                self.set_reg(rd, next_pc);
                self.pc = pc.wrapping_add(imm as u64);
                return Ok(());
            }
            Inst::Jalr { rd, rs1, imm } => {
                let target = self.reg(rs1).wrapping_add(imm as u64) & !1;
                self.set_reg(rd, next_pc);
                self.pc = target;
                return Ok(());
            }
            Inst::Branch { op, rs1, rs2, imm } => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                let taken = match op {
                    BranchOp::Eq => a == b,
                    BranchOp::Ne => a != b,
                    BranchOp::Lt => (a as i64) < (b as i64),
                    BranchOp::Ge => (a as i64) >= (b as i64),
                    BranchOp::Ltu => a < b,
                    BranchOp::Geu => a >= b,
                };
                if taken {
                    self.pc = pc.wrapping_add(imm as u64);
                    return Ok(());
                }
            }
            Inst::Load { op, rd, rs1, imm } => {
                let vaddr = self.reg(rs1).wrapping_add(imm as u64);
                let raw = self.guest_load(bus, vaddr, op.size())?;
                let val = match op {
                    LoadOp::B => (raw as u8 as i8) as i64 as u64,
                    LoadOp::Bu => raw & 0xFF,
                    LoadOp::H => (raw as u16 as i16) as i64 as u64,
                    LoadOp::Hu => raw & 0xFFFF,
                    LoadOp::W => (raw as u32 as i32) as i64 as u64,
                    LoadOp::Wu => raw & 0xFFFF_FFFF,
                    LoadOp::D => raw,
                };
                self.set_reg(rd, val);
            }
            Inst::Store { op, rs1, rs2, imm } => {
                let vaddr = self.reg(rs1).wrapping_add(imm as u64);
                self.guest_store(bus, vaddr, op.size(), self.reg(rs2))?;
            }
            Inst::OpImm { op, rd, rs1, imm } => {
                let a = self.reg(rs1);
                let b = match op {
                    AluOp::Sll
                    | AluOp::Srl
                    | AluOp::Sra
                    | AluOp::Sllw
                    | AluOp::Srlw
                    | AluOp::Sraw => (imm as u64) & 63,
                    _ => imm as u64,
                };
                self.set_reg(rd, alu(op, a, b));
            }
            Inst::Op { op, rd, rs1, rs2 } => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                self.set_reg(rd, alu(op, a, b));
            }
            Inst::FLoad { fmt, rd, rs1, imm } => {
                if !self.fp_allowed(fmt) {
                    return Err((Exception::IllegalInstruction, raw as u64));
                }
                let vaddr = self.reg(rs1).wrapping_add(imm as u64);
                let size = match fmt {
                    Fmt::S => 4,
                    Fmt::D => 8,
                };
                let val = self.guest_load(bus, vaddr, size)?;
                match fmt {
                    Fmt::S => self.set_freg32(rd, val as u32),
                    Fmt::D => self.set_freg64(rd, val),
                }
                self.fs_mark_dirty();
            }
            Inst::FStore { fmt, rs1, rs2, imm } => {
                if !self.fp_allowed(fmt) {
                    return Err((Exception::IllegalInstruction, raw as u64));
                }
                let vaddr = self.reg(rs1).wrapping_add(imm as u64);
                let size = match fmt {
                    Fmt::S => 4,
                    Fmt::D => 8,
                };
                let val = match fmt {
                    Fmt::S => self.freg32(rs2) as u64,
                    Fmt::D => self.fregs[rs2 as usize],
                };
                self.guest_store(bus, vaddr, size, val)?;
                self.fs_mark_dirty();
            }
            Inst::Fp(op) => {
                self.exec_fp(op, raw)?;
            }
            Inst::Amo {
                op,
                w,
                rd,
                rs1,
                rs2,
            } => {
                let vaddr = self.reg(rs1);
                let size = if w { 4 } else { 8 };
                // LR/SC 及 AMO 都要求自然对齐（QEMU 语义：LR → load misaligned，
                // 其余 → store/amo misaligned）
                if vaddr & (size as u64 - 1) != 0 {
                    return Err((
                        if matches!(op, AmoOp::Lr) {
                            Exception::LoadMisaligned
                        } else {
                            Exception::StoreMisaligned
                        },
                        vaddr,
                    ));
                }
                let acc = match op {
                    AmoOp::Lr => Access::Load,
                    _ => Access::Store,
                };
                let addr = self.translate(bus, vaddr, acc).map_err(|e| (e, vaddr))?;
                // PMP：LR 需要 R；SC/AMO 同时需要 R 和 W（按 store 汇报）
                match op {
                    AmoOp::Lr => {
                        self.pmp_check(addr, size, Access::Load, vaddr)?;
                    }
                    _ => {
                        self.pmp_check(addr, size, Access::Load, vaddr)
                            .map_err(|_| (Exception::StoreAccessFault, vaddr))?;
                        self.pmp_check(addr, size, Access::Store, vaddr)?;
                    }
                }
                let mask: u64 = if w { 0xFFFF_FFFF } else { u64::MAX };
                match op {
                    AmoOp::Lr => {
                        let mut v = bus.load(addr, size).map_err(|e| (e, vaddr))?;
                        // 规范：32 位 AMO/LR 结果符号扩展到 64 位
                        if w {
                            v = (v as u32 as i32) as i64 as u64;
                        }
                        // 预约基于物理地址（规范：预约集包含 PA）
                        self.reservation = Some(addr);
                        self.set_reg(rd, v);
                    }
                    AmoOp::Sc => {
                        if self.reservation == Some(addr) {
                            bus.store(addr, size, self.reg(rs2) & mask)
                                .map_err(|e| (e, vaddr))?;
                            self.reservation = None;
                            self.set_reg(rd, 0);
                        } else {
                            // 规范：失败的 SC 也必须作废预约
                            self.reservation = None;
                            self.set_reg(rd, 1);
                        }
                    }
                    _ => {
                        let mut old = bus.load(addr, size).map_err(|e| (e, vaddr))? & mask;
                        if w {
                            old = (old as u32 as i32) as i64 as u64;
                        }
                        let b = self.reg(rs2) & mask;
                        let new = match op {
                            AmoOp::Swap => b,
                            AmoOp::Add => old.wrapping_add(b) & mask,
                            AmoOp::Xor => old ^ b,
                            AmoOp::And => old & b,
                            AmoOp::Or => old | b,
                            AmoOp::Min => ((old as i32).min(b as i32)) as u32 as u64,
                            AmoOp::Max => ((old as i32).max(b as i32)) as u32 as u64,
                            AmoOp::Minu => old.min(b),
                            AmoOp::Maxu => old.max(b),
                            AmoOp::Lr | AmoOp::Sc => unreachable!(),
                        };
                        bus.store(addr, size, new).map_err(|e| (e, vaddr))?;
                        self.set_reg(rd, old);
                    }
                }
            }
            Inst::System(op) => {
                if self.system(op, raw, bus, console)? {
                    return Ok(());
                }
            }
        }
        self.pc = next_pc;
        Ok(())
    }

    /// 返回 true 表示 pc 已由指令自身更新（mret/sret）。
    fn system(
        &mut self,
        op: SystemOp,
        raw: u32,
        bus: &mut Bus,
        console: &mut Vec<u8>,
    ) -> Result<bool, (Exception, u64)> {
        match op {
            SystemOp::Wfi => {
                // 单 hart 且无其它事件源，等同等待一条指令；TW 置位时非 M 态执行它非法
                if self.csr.mstatus & csr::TW != 0 && self.privilege != Privilege::M {
                    return Err((Exception::IllegalInstruction, raw as u64));
                }
            }
            SystemOp::SfenceVma => {
                if self.privilege == Privilege::U
                    || (self.privilege == Privilege::S && self.csr.mstatus & csr::TVM != 0)
                {
                    return Err((Exception::IllegalInstruction, raw as u64));
                }
                self.mmu.flush();
                self.flush_fetch_caches();
            }
            SystemOp::Ebreak => return Err((Exception::Breakpoint, 0)),
            SystemOp::Mret => {
                // 规范：mret 仅 M 态可执行
                if self.privilege != Privilege::M {
                    return Err((Exception::IllegalInstruction, raw as u64));
                }
                let ms = self.csr.mstatus;
                let mpp = (ms & csr::MPP) >> 11;
                // 空 PMP 时低特权级取指会在下一条指令处自然触发
                // instruction access fault（mtval = 目标 pc），无需特判
                self.privilege = Privilege::from_bits(mpp);
                let mpie = (ms & csr::MPIE) >> 7;
                // 规范/QEMU：mret 退出 M 态时清 MPRV（防 M 态翻译权限泄漏）
                let mprv = if mpp != Privilege::M as u64 { csr::MPRV } else { 0 };
                self.csr.mstatus = (ms & !(csr::MIE | csr::MPIE | csr::MPP | mprv))
                    | (mpie << 3)
                    | csr::MPIE
                    | ((Privilege::U as u64) << 11);
                self.pc = self.csr.mepc;
                return Ok(true);
            }
            SystemOp::Sret => {
                if self.privilege == Privilege::U
                    || (self.privilege == Privilege::S && self.csr.mstatus & csr::TSR != 0)
                {
                    return Err((Exception::IllegalInstruction, raw as u64));
                }
                let ms = self.csr.mstatus;
                let spp = (ms & csr::SPP) >> 8;
                self.privilege = if spp == 1 { Privilege::S } else { Privilege::U };
                let spie = (ms & csr::SPIE) >> 5;
                self.csr.mstatus =
                    (ms & !(csr::SIE | csr::SPIE | csr::SPP | csr::MPRV)) | (spie << 1) | csr::SPIE;
                self.pc = self.csr.sepc;
                return Ok(true);
            }
            SystemOp::Ecall => match self.privilege {
                Privilege::M => self.host_ecall(bus, console),
                Privilege::S => {
                    if self.sbi {
                        self.sbi_call(bus, console);
                    } else {
                        return Err((Exception::EcallFromS, 0));
                    }
                }
                Privilege::U => return Err((Exception::EcallFromU, 0)),
            },
            SystemOp::Csr {
                kind,
                imm,
                csr: csr_addr,
                rd,
                rs1,
            } => {
                let time = bus.clint.mtime();
                let old = self
                    .csr
                    .read(csr_addr, self.privilege, self.ext_mip, time)
                    .ok_or((Exception::IllegalInstruction, csr_addr as u64))?;
                let src = if imm { rs1 as u64 } else { self.reg(rs1) };
                // CSRRS/CSRRC 在源为 x0（或 zimm=0）时不写
                if kind == CsrKind::Rw || rs1 != 0 {
                    // mip/sip 的 RS/RC：rd 读到的是含 PLIC 信号的 OR 视图，
                    // 但回写只使用软件位（规范 3.1.9：控制器信号不参与 RMW）
                    if kind != CsrKind::Rw && matches!(csr_addr, csr::csr::MIP | csr::csr::SIP) {
                        let sw_old = if csr_addr == csr::csr::MIP {
                            self.csr.mip
                        } else {
                            self.csr.mip & (self.csr.mideleg & csr::MIE_MASK)
                        };
                        let new = match kind {
                            CsrKind::Rs => sw_old | src,
                            CsrKind::Rc => sw_old & !src,
                            _ => unreachable!(),
                        };
                        self.csr.write(csr_addr, new, self.privilege);
                    } else if !self.csr.write(csr_addr, new_or_src(kind, src, old), self.privilege) {
                        return Err((Exception::IllegalInstruction, csr_addr as u64));
                    }
                    match csr_addr {
                        csr::csr::SATP => {
                            self.mmu.flush();
                            self.flush_fetch_caches();
                        }
                        // PMP 写：PMP 判定即时生效 + 取指缓存失效
                        0x3A0 | 0x3A2 | 0x3B0..=0x3BF => {
                            self.pmp_gen = self.pmp_gen.wrapping_add(1);
                            self.fetch_page = None;
                            self.flush_fetch_caches();
                        }
                        // 写浮点 CSR 修改 FP 架构状态 → FS=Dirty（规范 3.1.6.7）
                        csr::csr::FFLAGS | csr::csr::FRM | csr::csr::FCSR => {
                            self.fs_mark_dirty();
                        }
                        _ => {}
                    }
                }
                self.set_reg(rd, old);
            }
        }
        Ok(false)
    }

    /// 浮点指令执行（misa 扩展缺失或 mstatus.FS=Off 时 illegal，mtval=指令位型）
    fn exec_fp(&mut self, op: crate::decode::FOp, raw: u32) -> ExecResult {
        use crate::decode::Fmt as F;
        use crate::fpu;
        if !self.fp_allowed(op.fmt()) {
            return Err((Exception::IllegalInstruction, raw as u64));
        }
        // 统一出口：写 f 寄存器/fflags 的指令置 FS=Dirty
        macro_rules! finish {
            () => {
                self.fs_mark_dirty();
                return Ok(());
            };
        }
        // 只读 FP 状态（结果写 GPR）的指令不改变 FS
        macro_rules! finish_ro {
            () => {
                return Ok(());
            };
        }
        match op {
            crate::decode::FOp::Arith { op, fmt, rd, rs1, rs2, rm } => {
                let mode = fpu::effective_rm(rm, self.csr.fcsr).ok_or((Exception::IllegalInstruction, 0))?;
                let (val, flags) = match fmt {
                    F::S => {
                        let (v, f) = fpu::arith32(op, self.freg32(rs1), self.freg32(rs2), mode);
                        self.set_freg32(rd, v);
                        (v as u64, f)
                    }
                    F::D => {
                        let (v, f) = fpu::arith64(op, self.fregs[rs1 as usize], self.fregs[rs2 as usize], mode);
                        self.set_freg64(rd, v);
                        (v, f)
                    }
                };
                self.csr.fcsr |= flags;
                let _ = val;
                finish!();
            }
            crate::decode::FOp::MulAdd { kind, fmt, rd, rs1, rs2, rs3, rm } => {
                let mode = fpu::effective_rm(rm, self.csr.fcsr).ok_or((Exception::IllegalInstruction, 0))?;
                let (val, flags) = match fmt {
                    F::S => fpu::muladd32(kind, self.freg32(rs1), self.freg32(rs2), self.freg32(rs3), mode),
                    F::D => fpu::muladd64(kind, self.fregs[rs1 as usize], self.fregs[rs2 as usize], self.fregs[rs3 as usize], mode),
                };
                self.set_freg(rd, fmt, val);
                self.csr.fcsr |= flags;
                finish!();
            }
            crate::decode::FOp::Sgnj { neg, xor, fmt, rd, rs1, rs2 } => {
                let (a, b) = (self.freg(rs1, fmt), self.freg(rs2, fmt));
                let bits = match fmt {
                    F::S => 32,
                    F::D => 64,
                };
                let sign: u64 = 1 << (bits - 1);
                let mask: u64 = sign - 1;
                let sign_bit = if xor {
                    (a ^ b) & sign
                } else if neg {
                    (b ^ sign) & sign
                } else {
                    b & sign
                };
                let val = (a & mask) | sign_bit;
                self.set_freg(rd, fmt, val);
                finish!();
            }
            crate::decode::FOp::MinMax { max, fmt, rd, rs1, rs2 } => {
                let val: u64 = match fmt {
                    F::S => {
                        let v = if max {
                            fpu::fmax32(self.freg32(rs1), self.freg32(rs2))
                        } else {
                            fpu::fmin32(self.freg32(rs1), self.freg32(rs2))
                        };
                        v as u64
                    }
                    F::D => {
                        if max {
                            fpu::fmax64(self.fregs[rs1 as usize], self.fregs[rs2 as usize])
                        } else {
                            fpu::fmin64(self.fregs[rs1 as usize], self.fregs[rs2 as usize])
                        }
                    }
                };
                self.set_freg(rd, fmt, val);
                finish!();
            }
            crate::decode::FOp::Cmp { kind, fmt, rd, rs1, rs2 } => {
                let (r, flags) = match fmt {
                    F::S => fpu::fcmp32(kind, self.freg32(rs1), self.freg32(rs2)),
                    F::D => fpu::fcmp64(kind, self.fregs[rs1 as usize], self.fregs[rs2 as usize]),
                };
                self.set_reg(rd, r as u64);
                self.csr.fcsr |= flags;
                finish_ro!();
            }
            crate::decode::FOp::Cvtf2i { signed, is32, fmt, rd, rs1, rm } => {
                let mode = fpu::effective_rm(rm, self.csr.fcsr).ok_or((Exception::IllegalInstruction, 0))?;
                let exact = match fmt {
                    F::S => f32::from_bits(self.freg32(rs1)) as f64,
                    F::D => f64::from_bits(self.fregs[rs1 as usize]),
                };
                let (val, flags) = fpu::cvt_f_to_i(exact, mode, signed, is32);
                self.set_reg(rd, val);
                self.csr.fcsr |= flags;
                finish_ro!();
            }
            crate::decode::FOp::Cvti2f { signed, src32, fmt, rd, rs1, rm } => {
                let mode = fpu::effective_rm(rm, self.csr.fcsr).ok_or((Exception::IllegalInstruction, 0))?;
                let (val, flags) = fpu::cvt_i_to_f(self.reg(rs1), signed, src32, matches!(fmt, F::D), mode);
                self.set_freg(rd, fmt, val);
                self.csr.fcsr |= flags;
                finish!();
            }
            crate::decode::FOp::Cvtf2f { to64, rd, rs1, rm } => {
                let mode = fpu::effective_rm(rm, self.csr.fcsr).ok_or((Exception::IllegalInstruction, 0))?;
                let (val, flags) = if to64 {
                    fpu::cvt_f_to_f(self.freg32(rs1) as u64, true, mode)
                } else {
                    fpu::cvt_f_to_f(self.fregs[rs1 as usize], false, mode)
                };
                self.set_freg(rd, if to64 { F::D } else { F::S }, val);
                self.csr.fcsr |= flags;
                finish!();
            }
            crate::decode::FOp::FmvXf { fmt, rd, rs1 } => {
                let val = match fmt {
                    F::S => self.freg32(rs1) as i32 as i64 as u64,
                    F::D => self.fregs[rs1 as usize],
                };
                self.set_reg(rd, val);
                finish_ro!();
            }
            crate::decode::FOp::FmvFx { fmt, rd, rs1 } => {
                match fmt {
                    F::S => self.set_freg32(rd, self.reg(rs1) as u32),
                    F::D => self.set_freg64(rd, self.reg(rs1)),
                }
                finish!();
            }
            crate::decode::FOp::Fclass { fmt, rd, rs1 } => {
                let val = match fmt {
                    F::S => fpu::fclass32(self.freg32(rs1)),
                    F::D => fpu::fclass64(self.fregs[rs1 as usize]),
                };
                self.set_reg(rd, val);
                finish_ro!();
            }
        }
    }

    /// 阶段 1 约定：M-mode ecall 作为宿主调用（Linux 风格 ABI）。
    fn host_ecall(&mut self, bus: &mut Bus, console: &mut Vec<u8>) {
        match self.reg(17) {
            64 => {
                // write(fd, buf, count)
                let (fd, buf, len) = (self.reg(10), self.reg(11), self.reg(12));
                match Self::read_guest_mem(bus, buf, len) {
                    Some(bytes) if fd == 1 || fd == 2 => {
                        console.extend_from_slice(&bytes);
                        if fd == 1 {
                            let mut out = io::stdout().lock();
                            let _ = out.write_all(&bytes);
                            let _ = out.flush();
                        } else {
                            let mut err = io::stderr().lock();
                            let _ = err.write_all(&bytes);
                            let _ = err.flush();
                        }
                        self.set_reg(10, len);
                    }
                    _ => self.set_reg(10, u64::MAX), // -1
                }
            }
            93 => self.exit = Some(self.reg(10) as i32),
            _ => self.set_reg(10, u64::MAX), // 未实现的调用返回 -1
        }
    }

    fn read_guest_mem(bus: &mut Bus, buf: u64, len: u64) -> Option<Vec<u8>> {
        let mut bytes = Vec::with_capacity(len as usize);
        for i in 0..len {
            let addr = buf.checked_add(i)?;
            bytes.push(bus.load(addr, 1).ok()? as u8);
        }
        Some(bytes)
    }

    /// 内置 SBI（Cpu::sbi 开启）：S 态 ecall 按 SBI 规范处理。
    /// a0=错误码，a1=返回值；调用后正常推进 pc。
    fn sbi_call(&mut self, bus: &mut Bus, console: &mut Vec<u8>) {
        const BASE: u64 = 0x10;
        const TIME: u64 = 0x5449_4D45; // "TIME"
        const IPI: u64 = 0x73_5049; //   "sPI"
        const RFENCE: u64 = 0x5246_4E43; // "RFNC"
        const SRST: u64 = 0x5352_5354; // "SRST"
        const DBCN: u64 = 0x4442_434E; // "DBCN"

        let (eid, fid) = (self.reg(17), self.reg(16));
        let (a0, a1, _a2) = (self.reg(10), self.reg(11), self.reg(12));
        let ret: (i64, u64) = match (eid, fid) {
            // ---- legacy ----
            (0x00, _) => {
                bus.clint.mtimecmp = a0;
                (0, 0)
            } // sbi_set_timer
            (0x01, _) => {
                self.sbi_putc(bus, a0 as u8, console);
                (0, 0)
            } // console_putchar
            (0x02, _) => (-1, 0), // console_getchar：无输入
            (0x03, _) => (0, 0),  // clear_ipi
            (0x04, _) => (0, 0),  // send_ipi（单 hart）
            (0x05, _) => (0, 0),  // remote_fence_i
            (0x08, _) => {
                self.exit = Some(0);
                (0, 0)
            } // shutdown
            // ---- BASE ----
            (BASE, 0) => (0, 1 << 24), // sbi_spec_version：v1.0.0
            (BASE, 1) => (0, 1),       // impl_id：1（自定）
            (BASE, 2) => (0, 0),       // impl_version
            (BASE, 3) => {
                let known = matches!(a0, BASE | TIME | IPI | RFENCE | SRST | DBCN);
                (0, known as u64)
            } // probe_extension
            (BASE, 4) => (0, 0),       // mvendorid
            (BASE, 5) => (0, 0),       // marchid
            (BASE, 6) => (0, 0),       // mimpid
            // ---- TIME ----
            (TIME, 0) => {
                bus.clint.mtimecmp = a0;
                (0, 0)
            } // set_timer
            // ---- IPI ----
            (IPI, 0) => (0, 0), // send_ipi：单 hart 无操作
            // ---- RFENCE ----
            (RFENCE, f) if f <= 3 => {
                self.mmu.flush();
                (0, 0)
            }
            // ---- SRST ----
            (SRST, 0) => {
                // shutdown(0)/reboot(1) 都作正常关机
                self.exit = Some(0);
                (0, 0)
            }
            // ---- DBCN ----
            (DBCN, 0) => {
                // write(num_bytes, base_lo, base_hi)：物理地址
                match Self::read_guest_mem(bus, a1, a0.min(4096)) {
                    Some(bytes) => {
                        for &b in &bytes {
                            self.sbi_putc(bus, b, console);
                        }
                        (0, bytes.len() as u64)
                    }
                    None => (-2, 0), // INVALID_PARAM
                }
            }
            (DBCN, 1) => (0, 0), // read：无输入
            (DBCN, 2) => {
                self.sbi_putc(bus, a0 as u8, console);
                (0, 0)
            } // write_byte
            _ => (-3, 0),        // SBI_ERR_NOT_SUPPORTED
        };
        self.set_reg(10, ret.0 as u64);
        self.set_reg(11, ret.1);
    }

    fn sbi_putc(&self, bus: &mut Bus, b: u8, console: &mut Vec<u8>) {
        console.push(b);
        bus.uart.write(0, b);
    }

    // ---- trap / 中断 ----

    /// 有挂起且使能的中断时注入并返回 true（由运行循环在指令间调用）。
    pub fn take_pending_interrupt(&mut self) -> bool {
        let pending = (self.csr.mip | self.ext_mip) & self.csr.mie;
        if pending == 0 {
            return false;
        }
        // 优先级（规范推荐顺序的近似）
        const IRQS: [(u64, u64); 6] = [
            (csr::MEIP, 11),
            (csr::MSIP, 3),
            (csr::MTIP, 7),
            (csr::SEIP, 9),
            (csr::SSIP, 1),
            (csr::STIP, 5),
        ];
        for (bit, code) in IRQS {
            if pending & bit == 0 {
                continue;
            }
            // 委派位决定归属（QEMU：委派中断不进 M 取集，S 集在 M 态也使能）
            let to_s = (self.csr.mideleg >> code) & 1 == 1;
            let enabled = if to_s {
                // S 目标中断：M 态下不投递；S 态看 SIE；U 态总是使能
                self.privilege < Privilege::S
                    || (self.privilege == Privilege::S && self.csr.mstatus & csr::SIE != 0)
            } else {
                // M 目标中断：从更低特权级总是使能
                self.privilege != Privilege::M || self.csr.mstatus & csr::MIE != 0
            };
            if enabled {
                self.deliver_trap(self.pc, (1 << 63) | code, 0, to_s);
                return true;
            }
        }
        false
    }

    /// 交付异常：优先按 medeleg 委托到 S 态，否则进 M 态。
    /// 没有 handler 时作为致命错误返回。
    fn take_trap(&mut self, cause: Exception, val: u64) -> Result<(), TrapInfo> {
        let to_s = self.privilege != Privilege::M && (self.csr.medeleg >> cause.code()) & 1 == 1;
        if to_s && self.csr.stvec == 0 || !to_s && self.csr.mtvec == 0 {
            return Err(TrapInfo {
                pc: self.pc,
                cause,
                val,
            });
        }
        self.deliver_trap(self.pc, cause.code(), val, to_s);
        Ok(())
    }

    /// 写入 trap 上下文并跳转。cause 含中断位时调用方已置位。
    fn deliver_trap(&mut self, pc: u64, cause: u64, val: u64, to_s: bool) {
        if self.trace {
            println!(
                "---- trap -> {} @ {pc:012x} cause={cause:#x} val={val:#x}",
                if to_s { "S" } else { "M" }
            );
        }
        let interrupt = cause >> 63 == 1;
        if to_s {
            let ms = self.csr.mstatus;
            let sie = ms & csr::SIE;
            self.csr.sepc = pc;
            self.csr.scause = cause;
            self.csr.stval = val;
            self.csr.mstatus = (ms & !(csr::SIE | csr::SPIE | csr::SPP))
                | (sie << 4) // SPIE <- SIE
                | (((self.privilege != Privilege::U) as u64) << 8); // SPP <- 非 U 即 1（含 M→S 陷阱）
            self.privilege = Privilege::S;
            self.pc = trap_target(self.csr.stvec, cause, interrupt);
        } else {
            let ms = self.csr.mstatus;
            let mie = ms & csr::MIE;
            self.csr.mepc = pc;
            self.csr.mcause = cause;
            self.csr.mtval = val;
            self.csr.mstatus = (ms & !(csr::MIE | csr::MPIE | csr::MPP))
                | (mie << 4) // MPIE <- MIE
                | ((self.privilege as u64) << 11); // MPP
            self.privilege = Privilege::M;
            self.pc = trap_target(self.csr.mtvec, cause, interrupt);
        }
    }
}

/// tvec 的入口地址：direct 模式取基址；vectored 模式下中断跳 base+4*cause，
/// 异常仍跳基址。基址按规范取 4 字节对齐。
fn trap_target(tvec: u64, cause: u64, interrupt: bool) -> u64 {
    let base = tvec & !3;
    if interrupt && (tvec & 3) == 1 {
        base + 4 * (cause & 0x1F)
    } else {
        base
    }
}

/// CSR 写入值：RW 直接写，RS/RC 基于旧值合并
fn new_or_src(kind: CsrKind, src: u64, old: u64) -> u64 {
    match kind {
        CsrKind::Rw => src,
        CsrKind::Rs => old | src,
        CsrKind::Rc => old & !src,
    }
}

fn alu(op: AluOp, a: u64, b: u64) -> u64 {
    match op {
        AluOp::Add => a.wrapping_add(b),
        AluOp::Sub => a.wrapping_sub(b),
        AluOp::Sll => a << (b & 63),
        AluOp::Slt => ((a as i64) < (b as i64)) as u64,
        AluOp::Sltu => (a < b) as u64,
        AluOp::Xor => a ^ b,
        AluOp::Srl => a >> (b & 63),
        AluOp::Sra => ((a as i64) >> (b & 63)) as u64,
        AluOp::Or => a | b,
        AluOp::And => a & b,
        AluOp::Addw => (a.wrapping_add(b) as u32) as i32 as i64 as u64,
        AluOp::Subw => (a.wrapping_sub(b) as u32) as i32 as i64 as u64,
        AluOp::Sllw => ((a << (b & 31)) as u32) as i32 as i64 as u64,
        AluOp::Srlw => ((a as u32) >> (b & 31)) as i32 as i64 as u64,
        AluOp::Sraw => (((a as u32) as i32) >> (b & 31)) as i64 as u64,
        AluOp::Mul => a.wrapping_mul(b),
        AluOp::Mulh => (((a as i64 as i128) * (b as i64 as i128)) >> 64) as u64,
        AluOp::Mulhsu => (((a as i64 as i128) * (b as u128 as i128)) >> 64) as u64,
        AluOp::Mulhu => (((a as u128) * (b as u128)) >> 64) as u64,
        AluOp::Div => {
            let (x, y) = (a as i64, b as i64);
            if y == 0 {
                u64::MAX
            } else if x == i64::MIN && y == -1 {
                i64::MIN as u64
            } else {
                (x / y) as u64
            }
        }
        AluOp::Divu => a.checked_div(b).unwrap_or(u64::MAX),
        AluOp::Rem => {
            let (x, y) = (a as i64, b as i64);
            if y == 0 {
                a
            } else if x == i64::MIN && y == -1 {
                0
            } else {
                (x % y) as u64
            }
        }
        AluOp::Remu => a.checked_rem(b).unwrap_or(a),
        AluOp::Mulw => ((a as i32).wrapping_mul(b as i32)) as i64 as u64,
        AluOp::Divw => {
            let (x, y) = (a as i32, b as i32);
            if y == 0 {
                u64::MAX
            } else if x == i32::MIN && y == -1 {
                i32::MIN as i64 as u64
            } else {
                (x / y) as i64 as u64
            }
        }
        AluOp::Divuw => (a as u32)
            .checked_div(b as u32)
            .map(|v| v as u64)
            .unwrap_or(u64::MAX),
        AluOp::Remw => {
            let (x, y) = (a as i32, b as i32);
            if y == 0 {
                (a as i32) as i64 as u64
            } else if x == i32::MIN && y == -1 {
                0
            } else {
                (x % y) as i64 as u64
            }
        }
        AluOp::Remuw => (a as u32).checked_rem(b as u32).map_or(a, |v| v as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div_semantics() {
        assert_eq!(alu(AluOp::Div, 42, 7), 6);
        assert_eq!(alu(AluOp::Div, 42, 0), u64::MAX); // 除 0 商全 1
        assert_eq!(
            alu(AluOp::Div, i64::MIN as u64, -1i64 as u64),
            i64::MIN as u64
        );
        assert_eq!(alu(AluOp::Rem, 42, 0), 42); // 除 0 余数为被除数
        assert_eq!(alu(AluOp::Rem, i64::MIN as u64, -1i64 as u64), 0);
        assert_eq!(alu(AluOp::Divuw, u32::MAX as u64, 0), u64::MAX);
    }

    #[test]
    fn mul_semantics() {
        assert_eq!(alu(AluOp::Mul, 1 << 62, 4), 0); // 溢出回绕
        assert_eq!(alu(AluOp::Mulh, -1i64 as u64, -1i64 as u64), 0);
        assert_eq!(alu(AluOp::Mulh, i64::MIN as u64, -1i64 as u64), 0); // 2^63 高 64 位为 0
        assert_eq!(alu(AluOp::Mulhsu, -1i64 as u64, u64::MAX), u64::MAX); // -1 * (2^64-1) 高位 = -1
        assert_eq!(alu(AluOp::Mulhu, u64::MAX, u64::MAX), 0xFFFF_FFFF_FFFF_FFFE);
    }

    #[test]
    fn w_ops_sign_extend() {
        assert_eq!(alu(AluOp::Addw, -1i64 as u64, 1), 0);
        assert_eq!(alu(AluOp::Addw, 0x7FFF_FFFF, 1), 0xFFFF_FFFF_8000_0000);
        assert_eq!(alu(AluOp::Sllw, 1, 31), 0xFFFF_FFFF_8000_0000);
        assert_eq!(alu(AluOp::Srlw, 0xFFFF_FFFF_8000_0000, 31), 1);
        assert_eq!(alu(AluOp::Sraw, 0xFFFF_FFFF_8000_0000, 31), u64::MAX);
        assert_eq!(alu(AluOp::Srl, 0xFFFF_FFFF_8000_0000, 63), 1); // 64 位逻辑右移零扩展
    }
}
