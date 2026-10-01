//! CPU hart：取指、执行、CSR 与异常交付。
//! 阶段 1 只实现 M-mode。M-mode `ecall` 被模拟器拦截为宿主调用
//! （Linux 风格 write=64/exit=93），方便裸机程序输出和退出。

use std::io::{self, Write};

use crate::bus::Bus;
use crate::decode::{
    AluOp, AmoOp, BranchOp, CsrKind, Inst, LoadOp, SystemOp, decode, decode_compressed,
};
use crate::exception::{Exception, TrapInfo};

/// misa: RV64 IMA C
const MISA: u64 = (2 << 62) | (1 << 0) | (1 << 2) | (1 << 8) | (1 << 12);

const MSTATUS_MIE: u64 = 1 << 3;
const MSTATUS_MPIE: u64 = 1 << 7;
const MSTATUS_MPP_MASK: u64 = 3 << 11;

/// CSR 地址
pub mod csr {
    pub const MSTATUS: u16 = 0x300;
    pub const MISA: u16 = 0x301;
    pub const MIE: u16 = 0x304;
    pub const MTVEC: u16 = 0x305;
    pub const MSCRATCH: u16 = 0x340;
    pub const MEPC: u16 = 0x341;
    pub const MCAUSE: u16 = 0x342;
    pub const MTVAL: u16 = 0x343;
    pub const MIP: u16 = 0x344;
    pub const MVENDORID: u16 = 0xF11;
    pub const MARCHID: u16 = 0xF12;
    pub const MIMPID: u16 = 0xF13;
    pub const MHARTID: u16 = 0xF14;
    pub const MCYCLE: u16 = 0xB00;
    pub const MINSTRET: u16 = 0xB02;
    pub const CYCLE: u16 = 0xC00;
    pub const TIME: u16 = 0xC01;
    pub const INSTRET: u16 = 0xC02;
}

pub struct Counters {
    pub cycle: u64,
    pub instret: u64,
    pub time: u64,
}

pub struct Csrs {
    pub misa: u64,
    pub mstatus: u64,
    pub mie: u64,
    pub mip: u64,
    pub mtvec: u64,
    pub mscratch: u64,
    pub mepc: u64,
    pub mcause: u64,
    pub mtval: u64,
}

impl Default for Csrs {
    fn default() -> Self {
        Self::new()
    }
}

impl Csrs {
    pub fn new() -> Self {
        Csrs {
            misa: MISA,
            mstatus: 0,
            mie: 0,
            mip: 0,
            mtvec: 0,
            mscratch: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
        }
    }

    pub fn read(&self, addr: u16, c: &Counters) -> Option<u64> {
        Some(match addr {
            csr::MSTATUS => self.mstatus,
            csr::MISA => self.misa,
            csr::MIE => self.mie,
            csr::MTVEC => self.mtvec,
            csr::MSCRATCH => self.mscratch,
            csr::MEPC => self.mepc,
            csr::MCAUSE => self.mcause,
            csr::MTVAL => self.mtval,
            csr::MIP => self.mip,
            csr::MVENDORID | csr::MARCHID | csr::MIMPID | csr::MHARTID => 0,
            csr::MCYCLE | csr::CYCLE => c.cycle,
            csr::MINSTRET | csr::INSTRET => c.instret,
            csr::TIME => c.time,
            _ => return None,
        })
    }

    /// 写 CSR；对只读或未知 CSR 返回 false（触发 illegal instruction）。
    pub fn write(&mut self, addr: u16, val: u64) -> bool {
        match addr {
            csr::MSTATUS => self.mstatus = val,
            csr::MIE => self.mie = val,
            csr::MTVEC => self.mtvec = val,
            csr::MSCRATCH => self.mscratch = val,
            csr::MEPC => self.mepc = val,
            csr::MCAUSE => self.mcause = val,
            csr::MTVAL => self.mtval = val,
            csr::MIP => self.mip = val,
            csr::MCYCLE | csr::MINSTRET => {} // 计数器不可写，见 execute 的判断
            _ => return false,
        }
        true
    }

    fn writable(&self, addr: u16) -> bool {
        !matches!(addr, csr::MISA | csr::TIME | csr::CYCLE | csr::INSTRET | csr::MCYCLE | csr::MINSTRET | csr::MVENDORID | csr::MARCHID | csr::MIMPID | csr::MHARTID)
    }
}

pub struct Cpu {
    pub regs: [u64; 32],
    pub pc: u64,
    pub csr: Csrs,
    /// LR/SC 预约地址
    pub reservation: Option<u64>,
    /// ecall exit 设置
    pub exit: Option<i32>,
    pub instret: u64,
    pub trace: bool,
}

type ExecResult = Result<(), (Exception, u64)>;

impl Cpu {
    pub fn new(entry: u64) -> Self {
        Cpu {
            regs: [0; 32],
            pc: entry,
            csr: Csrs::new(),
            reservation: None,
            exit: None,
            instret: 0,
            trace: false,
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

    /// 执行一条指令。返回 Err 表示无法交付的异常（guest 没有 mtvec handler）。
    pub fn step(&mut self, bus: &mut Bus, console: &mut Vec<u8>) -> Result<(), TrapInfo> {
        let pc = self.pc;
        if pc & 1 != 0 {
            return self.take_trap(Exception::InstructionMisaligned, pc);
        }

        let lo = match bus.load(pc, 2) {
            Ok(v) => v as u16,
            Err(_) => return self.take_trap(Exception::InstructionAccessFault, pc),
        };
        let (inst, next_pc, raw) = if lo & 3 == 3 {
            let hi = match bus.load(pc + 2, 2) {
                Ok(v) => v as u16,
                Err(_) => return self.take_trap(Exception::InstructionAccessFault, pc + 2),
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
            println!("{pc:012x}: {raw:08x} {inst:?}");
        }

        if let Err((cause, val)) = self.execute(inst, pc, next_pc, bus, console) {
            return self.take_trap(cause, val);
        }
        self.instret += 1;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn execute(
        &mut self,
        inst: Inst,
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
                let addr = self.reg(rs1).wrapping_add(imm as u64);
                let raw = bus.load(addr, op.size()).map_err(|e| (e, addr))?;
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
                let addr = self.reg(rs1).wrapping_add(imm as u64);
                bus.store(addr, op.size(), self.reg(rs2))
                    .map_err(|e| (e, addr))?;
            }
            Inst::OpImm { op, rd, rs1, imm } => {
                let a = self.reg(rs1);
                let b = match op {
                    AluOp::Sll | AluOp::Srl | AluOp::Sra
                    | AluOp::Sllw | AluOp::Srlw | AluOp::Sraw => (imm as u64) & 63,
                    _ => imm as u64,
                };
                self.set_reg(rd, alu(op, a, b));
            }
            Inst::Op { op, rd, rs1, rs2 } => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                self.set_reg(rd, alu(op, a, b));
            }
            Inst::Amo { op, w, rd, rs1, rs2 } => {
                let addr = self.reg(rs1);
                let size = if w { 4 } else { 8 };
                let mask: u64 = if w { 0xFFFF_FFFF } else { u64::MAX };
                match op {
                    AmoOp::Lr => {
                        let v = bus.load(addr, size).map_err(|e| (e, addr))?;
                        self.reservation = Some(addr);
                        self.set_reg(rd, v);
                    }
                    AmoOp::Sc => {
                        if self.reservation == Some(addr) {
                            bus.store(addr, size, self.reg(rs2) & mask)
                                .map_err(|e| (e, addr))?;
                            self.reservation = None;
                            self.set_reg(rd, 0);
                        } else {
                            self.set_reg(rd, 1);
                        }
                    }
                    _ => {
                        let old = bus.load(addr, size).map_err(|e| (e, addr))? & mask;
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
                        bus.store(addr, size, new).map_err(|e| (e, addr))?;
                        self.set_reg(rd, old);
                    }
                }
            }
            Inst::System(op) => {
                if self.system(op, bus, console)? {
                    return Ok(()); // pc 已由指令自身设置（如 mret）
                }
            }
        }
        self.pc = next_pc;
        Ok(())
    }

    /// 返回 true 表示 pc 已更新，调用方不要推进 next_pc。
    fn system(&mut self, op: SystemOp, bus: &mut Bus, console: &mut Vec<u8>) -> Result<bool, (Exception, u64)> {
        match op {
            SystemOp::Wfi => {} // 单核无中断源，等同 nop
            SystemOp::Ebreak => return Err((Exception::Breakpoint, 3)),
            SystemOp::Mret => {
                self.pc = self.csr.mepc;
                let ms = self.csr.mstatus;
                let mpie = (ms & MSTATUS_MPIE) >> 7;
                self.csr.mstatus = (ms & !(MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP_MASK))
                    | (mpie << 3)
                    | MSTATUS_MPIE;
                return Ok(true);
            }
            SystemOp::Ecall => self.host_ecall(bus, console),
            SystemOp::Csr { kind, imm, csr, rd, rs1 } => {
                let counters = self.counters(bus);
                let old = self
                    .csr
                    .read(csr, &counters)
                    .ok_or((Exception::IllegalInstruction, csr as u64))?;
                let src = if imm {
                    rs1 as u64 // zimm
                } else {
                    self.reg(rs1)
                };
                // CSRRS/CSRRC 在源为 x0（或 zimm=0）时不写
                if kind != CsrKind::Rw && rs1 == 0 {
                    self.set_reg(rd, old);
                    return Ok(false);
                }
                if !self.csr.writable(csr) {
                    return Err((Exception::IllegalInstruction, csr as u64));
                }
                let new = match kind {
                    CsrKind::Rw => src,
                    CsrKind::Rs => old | src,
                    CsrKind::Rc => old & !src,
                };
                self.csr.write(csr, new);
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

    /// 交付异常：有 mtvec 就进 guest handler，否则作为致命错误返回。
    fn take_trap(&mut self, cause: Exception, val: u64) -> Result<(), TrapInfo> {
        if self.csr.mtvec == 0 {
            return Err(TrapInfo { pc: self.pc, cause, val });
        }
        self.csr.mepc = self.pc;
        self.csr.mcause = cause.code();
        self.csr.mtval = val;
        let ms = self.csr.mstatus;
        let mie = (ms & MSTATUS_MIE) >> 3;
        self.csr.mstatus = (ms & !(MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP_MASK))
            | (mie << 7) // MPIE <- MIE
            | (3 << 11); // MPP <- M
        self.pc = self.csr.mtvec & !3;
        Ok(())
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
        AluOp::Remu => {
            if b == 0 {
                a
            } else {
                a % b
            }
        }
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
        AluOp::Remuw => {
            if (b as u32) == 0 {
                a
            } else {
                ((a as u32) % (b as u32)) as u64
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div_semantics() {
        assert_eq!(alu(AluOp::Div, 42, 7), 6);
        assert_eq!(alu(AluOp::Div, 42, 0), u64::MAX); // 除 0 商全 1
        assert_eq!(alu(AluOp::Div, i64::MIN as u64, -1i64 as u64), i64::MIN as u64);
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
