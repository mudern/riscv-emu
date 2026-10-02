//! CPU hart：取指、执行、CSR 与异常交付、特权级、Sv39 翻译。
//!
//! 阶段 1 约定保留：M-mode `ecall` 被拦截为宿主调用（write/exit）；
//! U/S 态 ecall 走标准异常交付。

use std::io::{self, Write};

use crate::bus::Bus;
use crate::csr::{self, Counters, Csrs};
use crate::decode::{
    AluOp, AmoOp, BranchOp, CsrKind, Inst, LoadOp, SystemOp, decode, decode_compressed,
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
}

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

    fn counters(&self, bus: &Bus) -> Counters {
        Counters {
            cycle: self.instret,
            instret: self.instret,
            time: bus.clint.mtime(),
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
        self.mmu
            .translate(bus, self.csr.satp, eff, mxr, sum, vaddr, acc)
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

    /// 执行一条指令。返回 Err 表示无法交付的异常（guest 没有 handler）。
    pub fn step(&mut self, bus: &mut Bus, console: &mut Vec<u8>) -> Result<(), TrapInfo> {
        let pc = self.pc;
        if pc & 1 != 0 {
            return self.take_trap(Exception::InstructionMisaligned, pc);
        }

        let lo = self
            .translate(bus, pc, Access::Fetch)
            .map_err(|e| (e, pc))
            .and_then(|phys| self.pmp_check(phys, 2, Access::Fetch, pc))
            .and_then(|phys| bus.load(phys, 2).map_err(|e| (e, pc)));
        let lo = match lo {
            Ok(v) => v as u16,
            Err((e, t)) => return self.take_trap(e, t),
        };

        let (inst, next_pc, raw) = if lo & 3 == 3 {
            let hi = self
                .translate(bus, pc.wrapping_add(2), Access::Fetch)
                .map_err(|e| (e, pc))
                .and_then(|phys| self.pmp_check(phys, 2, Access::Fetch, pc))
                .and_then(|phys| bus.load(phys, 2).map_err(|e| (e, pc)));
            let hi = match hi {
                Ok(v) => v as u16,
                Err((e, t)) => return self.take_trap(e, t),
            };
            let word = lo as u32 | (hi as u32) << 16;
            match decode(word) {
                Ok(i) => (i, pc + 4, word),
                Err(_) => return self.take_trap(Exception::IllegalInstruction, word as u64),
            }
        } else {
            match decode_compressed(lo) {
                Ok(i) => (i, pc + 2, lo as u32),
                Err(_) => return self.take_trap(Exception::IllegalInstruction, lo as u64),
            }
        };

        if self.trace {
            println!("{pc:012x} [{}] {raw:08x} {inst:?}", self.privilege.name());
        }

        if let Err((cause, val)) = self.execute(inst, raw, pc, next_pc, bus, console) {
            return self.take_trap(cause, val);
        }
        self.instret += 1;
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
            Inst::Nop | Inst::Fence | Inst::FenceI => {}
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
                let phys = self
                    .translate(bus, vaddr, Access::Load)
                    .map_err(|e| (e, vaddr))?;
                self.pmp_check(phys, op.size(), Access::Load, vaddr)?;
                let raw = bus.load(phys, op.size()).map_err(|e| (e, vaddr))?;
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
                let phys = self
                    .translate(bus, vaddr, Access::Store)
                    .map_err(|e| (e, vaddr))?;
                self.pmp_check(phys, op.size(), Access::Store, vaddr)?;
                bus.store(phys, op.size(), self.reg(rs2))
                    .map_err(|e| (e, vaddr))?;
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
            Inst::Amo {
                op,
                w,
                rd,
                rs1,
                rs2,
            } => {
                let vaddr = self.reg(rs1);
                let acc = match op {
                    AmoOp::Lr => Access::Load,
                    _ => Access::Store,
                };
                let addr = self.translate(bus, vaddr, acc).map_err(|e| (e, vaddr))?;
                let size = if w { 4 } else { 8 };
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
                        let v = bus.load(addr, size).map_err(|e| (e, vaddr))?;
                        self.reservation = Some(vaddr);
                        self.set_reg(rd, v);
                    }
                    AmoOp::Sc => {
                        if self.reservation == Some(vaddr) {
                            bus.store(addr, size, self.reg(rs2) & mask)
                                .map_err(|e| (e, vaddr))?;
                            self.reservation = None;
                            self.set_reg(rd, 0);
                        } else {
                            self.set_reg(rd, 1);
                        }
                    }
                    _ => {
                        let old = bus.load(addr, size).map_err(|e| (e, vaddr))? & mask;
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
            }
            SystemOp::Ebreak => return Err((Exception::Breakpoint, 3)),
            SystemOp::Mret => {
                let ms = self.csr.mstatus;
                let mpp = (ms & csr::MPP) >> 11;
                // 空 PMP 下返回低特权级：任何取指都会被拒，按 QEMU 在 mret 处
                // 直接抛 instruction access fault（mtval=0）
                if mpp != Privilege::M as u64 && self.csr.pmp.num_rules() == 0 {
                    return Err((Exception::InstructionAccessFault, 0));
                }
                self.privilege = Privilege::from_bits(mpp);
                let mpie = (ms & csr::MPIE) >> 7;
                self.csr.mstatus = (ms & !(csr::MIE | csr::MPIE | csr::MPP))
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
                let spp = (ms & csr::SPP) >> 11;
                self.privilege = if spp == 1 { Privilege::S } else { Privilege::U };
                let spie = (ms & csr::SPIE) >> 5;
                self.csr.mstatus =
                    (ms & !(csr::SIE | csr::SPIE | csr::SPP)) | (spie << 1) | csr::SPIE;
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
                let counters = self.counters(bus);
                let old = self
                    .csr
                    .read(csr_addr, &counters, self.privilege)
                    .ok_or((Exception::IllegalInstruction, csr_addr as u64))?;
                let src = if imm { rs1 as u64 } else { self.reg(rs1) };
                // CSRRS/CSRRC 在源为 x0（或 zimm=0）时不写
                if kind == CsrKind::Rw || rs1 != 0 {
                    let new = match kind {
                        CsrKind::Rw => src,
                        CsrKind::Rs => old | src,
                        CsrKind::Rc => old & !src,
                    };
                    if !self.csr.write(csr_addr, new, self.privilege) {
                        return Err((Exception::IllegalInstruction, csr_addr as u64));
                    }
                    if csr_addr == csr::csr::SATP {
                        self.mmu.flush();
                    }
                }
                self.set_reg(rd, old);
            }
        }
        Ok(false)
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

    fn read_guest_mem(bus: &Bus, buf: u64, len: u64) -> Option<Vec<u8>> {
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
        let pending = self.csr.mip & self.csr.mie;
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
                | (((self.privilege == Privilege::S) as u64) << 11); // SPP
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
