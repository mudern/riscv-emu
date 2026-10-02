//! 集成测试：手工编码的裸机程序在模拟器上执行。

use riscv_emu::{Exception, Halt, Machine};

pub const DRAM_BASE: u64 = 0x8000_0000;

// ---- 最小指令编码器 ----

fn r_type(f7: u32, rs2: u32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    (f7 << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | opc
}
fn i_type(imm: i32, rs1: u32, f3: u32, rd: u32, opc: u32) -> u32 {
    debug_assert!((-2048..=2047).contains(&imm), "imm 越界: {imm}");
    (((imm as u32) & 0xFFF) << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | opc
}
fn s_type(imm: i32, rs2: u32, rs1: u32, f3: u32, opc: u32) -> u32 {
    debug_assert!((-2048..=2047).contains(&imm), "imm 越界: {imm}");
    (((imm >> 5) as u32 & 0x7F) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (f3 << 12)
        | (((imm as u32) & 0x1F) << 7)
        | opc
}
fn b_type(imm: i32, rs2: u32, rs1: u32, f3: u32) -> u32 {
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
fn u_type(imm20: u32, rd: u32, opc: u32) -> u32 {
    (imm20 & 0xFFFFF) << 12 | (rd << 7) | opc
}

fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 0, rd, 0x13)
}
fn lui(rd: u32, imm20: u32) -> u32 {
    u_type(imm20, rd, 0x37)
}
fn auipc(rd: u32, imm20: u32) -> u32 {
    u_type(imm20, rd, 0x17)
}
fn sb(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(imm, rs2, rs1, 0, 0x23)
}
fn sd(rs2: u32, rs1: u32, imm: i32) -> u32 {
    s_type(imm, rs2, rs1, 3, 0x23)
}
fn mul(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(1, rs2, rs1, 0, rd, 0x33)
}
fn amoswap_d(rd: u32, rs1: u32, rs2: u32) -> u32 {
    // AMO：funct5[31:27]=0x01 amoswap，aq/rl=0，funct3=3 (.d)
    (0x01 << 27) | (rs2 << 20) | (rs1 << 15) | (3 << 12) | (rd << 7) | 0x2F
}
fn bne(rs1: u32, rs2: u32, off: i32) -> u32 {
    b_type(off, rs2, rs1, 1)
}
const ECALL: u32 = 0x0000_0073;
const MRET: u32 = 0x3020_0073;
fn csrrs(rd: u32, csr: u32) -> u32 {
    (csr << 20) | (2 << 12) | (rd << 7) | 0x73
}
fn csrw(csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (1 << 12) | 0x73
}

// 程序内固定偏移（同一 4KB 页内，addi 立即数够得着）
const FAIL: i32 = 0x1080;
const MSG: i32 = 0x1100;

#[derive(Default)]
struct Asm {
    buf: Vec<u8>,
}

impl Asm {
    fn pc(&self) -> i32 {
        self.buf.len() as i32
    }
    fn emit32(&mut self, w: u32) {
        self.buf.extend_from_slice(&w.to_le_bytes());
    }
    fn emit16(&mut self, h: u16) {
        self.buf.extend_from_slice(&h.to_le_bytes());
    }
    fn pad_to(&mut self, off: usize) {
        while self.buf.len() < off {
            self.buf.push(0);
        }
    }
    fn data(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

/// t0=5 t1=6 t2=7 a0=10 a1=11 a2=12 a3=13 t3=28 t4=29
/// 1) UART 写 'H'  2) mul 6*7=42  3) c.add 压缩指令
/// 4) sd 到 RAM  5) amoswap  6) bne 检查  7) ecall write/exit
#[test]
fn bare_metal_program() {
    let mut a = Asm::default();
    // t0 = 0x1000_0000 (UART), t1 = 'H'
    a.emit32(lui(5, 0x10000));
    a.emit32(addi(6, 0, 0x48));
    a.emit32(sb(6, 5, 0));
    // a0=6 a1=7, a2 = a0*a1 = 42
    a.emit32(addi(10, 0, 6));
    a.emit32(addi(11, 0, 7));
    a.emit32(mul(12, 10, 11));
    // a3 = 6 + 7 = 13 （压缩指令 c.add a3, a1）
    a.emit32(addi(13, 0, 6));
    a.emit16(0x96AE); // c.add a3, a1
    // t2 = 0x8000_1000（auipc PC 相对寻址，lui 0x8000x 会符号扩展成负数）
    let p = a.pc();
    a.emit32(auipc(7, 1)); // t2 = pc + 0x1000
    a.emit32(addi(7, 7, 0x1000 - p - 0x1000)); // t2 = 0x8000_1000
    a.emit32(sd(12, 7, 0));
    a.emit32(sd(13, 7, 8));
    // t3 = 42; a2 != t3 -> fail
    a.emit32(addi(28, 0, 42));
    let b1 = a.pc();
    a.emit32(bne(12, 28, FAIL - b1));
    // amoswap.d t4, t4, (t2)：t4 <- 42，内存 <- 7
    a.emit32(addi(29, 0, 7));
    a.emit32(amoswap_d(29, 7, 29));
    let b2 = a.pc();
    a.emit32(bne(29, 28, FAIL - b2));
    // ecall write(1, msg@0x1100, 5)
    let p = a.pc();
    a.emit32(auipc(11, 1));
    a.emit32(addi(11, 11, MSG - p - 0x1000));
    a.emit32(addi(17, 0, 64));
    a.emit32(addi(10, 0, 1));
    a.emit32(addi(12, 0, 5));
    a.emit32(ECALL);
    // ecall exit(42)
    a.emit32(addi(17, 0, 93));
    a.emit32(addi(10, 0, 42));
    a.emit32(ECALL);
    // fail: ecall exit(1)
    a.pad_to(FAIL as usize);
    a.emit32(addi(17, 0, 93));
    a.emit32(addi(10, 0, 1));
    a.emit32(ECALL);
    a.pad_to(MSG as usize);
    a.data(b"hello");

    let mut m = Machine::new(16);
    m.bus.uart.print = false;
    if std::env::var("TRACE").is_ok() {
        m.cpu.trace = true;
    }
    assert!(m.bus.write_dram(DRAM_BASE, &a.buf));
    m.cpu.pc = DRAM_BASE;

    assert_eq!(m.run(1_000_000), Halt::Exit(42), "应通过 ecall exit(42) 退出");
    assert_eq!(m.bus.uart.output, b"H");
    assert_eq!(m.console, b"hello");
    assert_eq!(m.bus.load(DRAM_BASE + 0x1000, 8).unwrap(), 7); // amoswap 换成了 7
    assert_eq!(m.bus.load(DRAM_BASE + 0x1008, 8).unwrap(), 13);
}

/// mtvec trap handler：illegal instruction 被 handler 跳过后继续执行。
#[test]
fn trap_handler_mret() {
    let mut a = Asm::default();
    // t0 = 0x8000_1080 (handler)，设 mtvec
    let p = a.pc();
    a.emit32(auipc(5, 1));
    a.emit32(addi(5, 5, 0x1080 - p - 0x1000));
    a.emit32(csrw(0x305, 5)); // mtvec
    // 非法指令 -> trap
    a.emit32(0x0000_0000);
    // 恢复执行：t3 = 99，存到 0x8000_1010
    a.emit32(addi(28, 0, 99));
    let p = a.pc();
    a.emit32(auipc(7, 1));
    a.emit32(addi(7, 7, 0x1000 - p - 0x1000)); // t2 = 0x8000_1000
    a.emit32(sd(28, 7, 0x10));
    // ecall exit(0)
    a.emit32(addi(17, 0, 93));
    a.emit32(addi(10, 0, 0));
    a.emit32(ECALL);
    // handler @0x1080: mepc += 4; mret
    a.pad_to(0x1080);
    a.emit32(csrrs(5, 0x341)); // t0 = mepc
    a.emit32(addi(5, 5, 4));
    a.emit32(csrw(0x341, 5)); // mepc = t0
    a.emit32(MRET);

    let mut m = Machine::new(16);
    m.bus.uart.print = false;
    assert!(m.bus.write_dram(DRAM_BASE, &a.buf));
    m.cpu.pc = DRAM_BASE;

    assert_eq!(m.run(1_000_000), Halt::Exit(0));
    assert_eq!(m.bus.load(DRAM_BASE + 0x1010, 8).unwrap(), 99);
    assert_eq!(m.cpu.csr.mcause, 2); // illegal instruction
}

/// 无 handler 时的非法指令应作为致命异常上报。
#[test]
fn fatal_trap_without_handler() {
    let mut a = Asm::default();
    a.emit32(0xFFFF_FFFF); // 永远非法

    let mut m = Machine::new(1);
    assert!(m.bus.write_dram(DRAM_BASE, &a.buf));
    m.cpu.pc = DRAM_BASE;

    match m.run(1000) {
        Halt::Trap(t) => {
            assert_eq!(t.pc, DRAM_BASE);
            assert_eq!(t.cause, Exception::IllegalInstruction);
        }
        other => panic!("期望 Trap，得到 {other:?}"),
    }
}

/// sifive_test 设备写 0x5555 退出。
#[test]
fn test_device_poweroff() {
    let mut a = Asm::default();
    a.emit32(lui(5, 0x100)); // t0 = 0x100000 (test 设备)
    a.emit32(lui(6, 5)); // t1 = 0x5000
    a.emit32(addi(6, 6, 0x555)); // t1 = 0x5555 (FINISHER_PASS)
    a.emit32(sd(6, 5, 0));
    a.emit32(ECALL); // 不应到达

    let mut m = Machine::new(1);
    m.bus.uart.print = false;
    assert!(m.bus.write_dram(DRAM_BASE, &a.buf));
    m.cpu.pc = DRAM_BASE;
    assert_eq!(m.run(1000), Halt::Exit(0));
}

/// amo/lr 32 位结果符号扩展到 64 位（内核引用计数依赖此语义；
/// 曾因零扩展导致 Linux 的 i_writecount 泄漏、exec 报 ETXTBSY）
#[test]
fn amo_w_results_sign_extend() {
    // amoswap.w a0, x0, (a1)：读回内存 0xFFFFFF80 → a0 = -128（符号扩展）
    let mut a = Asm::default();
    a.emit32(addi(11, 0, 1));
    a.emit32(slli(11, 11, 31)); // a1 = 0x80000000（避免 lui 的 32 位符号扩展）
    a.emit32(addi(11, 11, 0x7F0)); // +2032
    a.emit32(addi(11, 11, 0x10)); // +16 → a1 = 0x80000800
    a.emit32(addi(10, 0, -128)); // 数据 = 0xFFFFFFFFFFFFFF80
    a.emit32(sd(10, 11, 0)); // [0x800] = 0xFFFFFFFFFFFFFF80
    a.emit32(amoswap_w(10, 0, 11)); // a0 = [0x800] 低 32 位（swap 写入 x0=0）
    a.emit32(addi(17, 0, 93));
    a.emit32(i_type(0, 0, 0, 0, 0x73)); // ecall

    let mut m = Machine::new(1);
    assert!(m.bus.write_dram(DRAM_BASE, &a.buf));
    m.cpu.pc = DRAM_BASE;
    assert_eq!(m.run(1000), Halt::Exit(-128), "exit 码即 a0（旧值，符号扩展后）");
    assert_eq!(m.cpu.regs[10], (-128i64) as u64, "amoswap.w 结果应符号扩展");

    // lr.w 同理
    let mut a = Asm::default();
    a.emit32(addi(11, 0, 1));
    a.emit32(slli(11, 11, 31));
    a.emit32(addi(11, 11, 0x7F0));
    a.emit32(addi(11, 11, 0x10));
    a.emit32(addi(10, 0, -128));
    a.emit32(sd(10, 11, 0));
    a.emit32(lr_w(10, 11));
    a.emit32(addi(17, 0, 93));
    a.emit32(i_type(0, 0, 0, 0, 0x73));
    let mut m = Machine::new(1);
    assert!(m.bus.write_dram(DRAM_BASE, &a.buf));
    m.cpu.pc = DRAM_BASE;
    assert_eq!(m.run(1000), Halt::Exit(-128), "exit 码即 a0（旧值，符号扩展后）");
    assert_eq!(m.cpu.regs[10], (-128i64) as u64, "lr.w 结果应符号扩展");
}

fn slli(rd: u32, rs1: u32, sh: u32) -> u32 {
    i_type(sh as i32, rs1, 1, rd, 0x13)
}

/// amoswap.w rd, rs2, (rs1)：funct5=SWAP(0x01), funct3=W(2)
fn amoswap_w(rd: u32, rs2: u32, rs1: u32) -> u32 {
    (0x01 << 27) | (rs2 << 20) | (rs1 << 15) | (0x02 << 12) | (rd << 7) | 0x2F
}
/// lr.w rd, (rs1)：funct5=LR(0x02), rs2=0, funct3=W(2)
fn lr_w(rd: u32, rs1: u32) -> u32 {
    (0x02 << 27) | (rs1 << 15) | (0x02 << 12) | (rd << 7) | 0x2F
}
