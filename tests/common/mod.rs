//! 测试共享：最小 RISC-V 指令编码器 + 裸机程序装配器。

#![allow(dead_code)]

use riscv_emu::Machine;

pub const DRAM_BASE: u64 = 0x8000_0000;

pub fn r_type(f7: u32, rs2: u32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    (f7 << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | opc
}
pub fn i_type(imm: i32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    debug_assert!((-2048..=2047).contains(&imm), "I-imm 越界: {imm}");
    (((imm as u32) & 0xFFF) << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | opc
}
pub fn s_type(imm: i32, rs2: u32, rs1: u32, f3: u32, opc: u32) -> u32 {
    debug_assert!((-2048..=2047).contains(&imm), "S-imm 越界: {imm}");
    (((imm >> 5) as u32 & 0x7F) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (f3 << 12)
        | (((imm as u32) & 0x1F) << 7)
        | opc
}
pub fn b_type(imm: i32, rs2: u32, rs1: u32, f3: u32) -> u32 {
    let i = imm as u32;
    (((i >> 12) & 1) << 31)
        | (((i >> 5) & 0x3F) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (f3 << 12)
        | (((i >> 1) & 0xF) << 8)
        | (((i >> 11) & 1) << 7)
        | 0x63
}
pub fn u_type(imm20: u32, rd: u32, opc: u32) -> u32 {
    (imm20 & 0xFFFFF) << 12 | (rd << 7) | opc
}

pub fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 0, rd, 0x13)
}
pub fn add(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, rs2, rs1, 0, rd, 0x33)
}
pub fn lui(rd: u32, imm20: u32) -> u32 {
    u_type(imm20, rd, 0x37)
}
pub fn auipc(rd: u32, imm20: u32) -> u32 {
    u_type(imm20, rd, 0x17)
}
pub fn ld(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 3, rd, 0x03)
}
pub fn lw(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 2, rd, 0x03)
}
pub fn lb(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 0, rd, 0x03)
}
pub fn sd(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(imm, rs2, rs1, 3, 0x23)
}
pub fn sb(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(imm, rs2, rs1, 0, 0x23)
}
pub fn beq(rs1: u32, rs2: u32, off: i32) -> u32 {
    b_type(off, rs2, rs1, 0)
}
pub fn bne(rs1: u32, rs2: u32, off: i32) -> u32 {
    b_type(off, rs2, rs1, 1)
}
pub fn beqz(rs1: u32, off: i32) -> u32 {
    b_type(off, 0, rs1, 0)
}
pub fn jal(rd: u32, off: i32) -> u32 {
    let i = off as u32;
    (((i >> 20) & 1) << 31)
        | (((i >> 1) & 0x3FF) << 21)
        | (((i >> 11) & 1) << 20)
        | (((i >> 12) & 0xFF) << 12)
        | (rd << 7)
        | 0x6F
}
pub const ECALL: u32 = 0x0000_0073;
pub const MRET: u32 = 0x3020_0073;
pub const SRET: u32 = 0x1020_0073;
pub const WFI: u32 = 0x1050_0073;

/// M 态下开放全地址空间（pmpaddr0=~0, pmpcfg0=NAPOT|RWX）。
/// QEMU 语义：mret 进 S/U 前必须配置 PMP，否则在 mret 处抛 instruction access fault。
/// 借用 t3 作暂存寄存器。
pub fn pmp_open_all(a: &mut Asm) {
    a.emit32(addi(reg::T3, reg::ZERO, -1));
    a.emit32(csrw(0x3B0, reg::T3)); // pmpaddr0
    a.emit32(addi(reg::T3, reg::ZERO, 0x1F)); // NAPOT | X | W | R
    a.emit32(csrw(0x3A0, reg::T3)); // pmpcfg0
}

pub fn csrw(csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (1 << 12) | 0x73
}
pub fn csrr(rd: u32, csr: u32) -> u32 {
    (csr << 20) | (2 << 12) | (rd << 7) | 0x73
}
pub fn csrs(csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (2 << 12) | 0x73
}
pub fn csrwi(csr: u32, zimm: u32) -> u32 {
    debug_assert!(zimm <= 31);
    (csr << 20) | (zimm << 15) | (5 << 12) | 0x73
}

pub fn mul(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(1, rs2, rs1, 0, rd, 0x33)
}
pub fn slli(rd: u32, rs1: u32, sh: u32) -> u32 {
    debug_assert!(sh < 32);
    i_type(sh as i32, rs1, 1, rd, 0x13)
}

/// 寄存器编号（ABI 名）
pub mod reg {
    pub const ZERO: u32 = 0;
    pub const RA: u32 = 1;
    pub const SP: u32 = 2;
    pub const A0: u32 = 10;
    pub const A1: u32 = 11;
    pub const A7: u32 = 17;
    pub const T0: u32 = 5;
    pub const T1: u32 = 6;
    pub const T2: u32 = 7;
    pub const T3: u32 = 28;
    pub const T4: u32 = 29;
    pub const T5: u32 = 30;
    pub const T6: u32 = 31;
}

#[derive(Default)]
pub struct Asm {
    pub buf: Vec<u8>,
}

impl Asm {
    pub fn new() -> Self {
        Asm { buf: Vec::new() }
    }

    pub fn pc(&self) -> i32 {
        self.buf.len() as i32
    }

    pub fn emit32(&mut self, w: u32) {
        self.buf.extend_from_slice(&w.to_le_bytes());
    }

    pub fn emit16(&mut self, h: u16) {
        self.buf.extend_from_slice(&h.to_le_bytes());
    }

    /// 绝对地址取值惯用组：auipc rd, %pcrel_hi ; addi rd, rd, %pcrel_lo
    pub fn addr_of(&mut self, rd: u32, target: i32) {
        let p = self.pc();
        let delta = target - p;
        let hi = (delta + 0x800) >> 12; // 算术移位，向下取整
        let lo = delta - (hi << 12);
        self.emit32(auipc(rd, (hi as u32) & 0xFFFFF));
        self.emit32(addi(rd, rd, lo));
    }

    pub fn pad_to(&mut self, off: usize) {
        while self.buf.len() < off {
            self.buf.push(0);
        }
    }

    pub fn data(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// 装入裸机并返回 Machine（UART 静音；TRACE=1 时开指令跟踪）
    pub fn into_machine(self, dram_mb: usize) -> Machine {
        let mut m = Machine::new(dram_mb);
        m.bus.uart.print = false;
        if std::env::var("TRACE").is_ok() {
            m.cpu.trace = true;
        }
        assert!(m.bus.write_dram(DRAM_BASE, &self.buf));
        m.cpu.pc = DRAM_BASE;
        m
    }
}
