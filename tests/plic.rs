//! PLIC 集成测试：UART RX 中断经 PLIC 交付
//! 1) M 态直收（MEIP → claim → 读 RBR → complete）
//! 2) 委派 S 态收（OpenSBI/Linux 风格：MEIP 委派为 SEIP）
//! 3) 寄存器视图（优先级 WARL、阈值、pending 只读）

mod common;

use common::*;
use riscv_emu::machine::Halt;
use riscv_emu::UART_IRQ;

const RESULTS: u64 = 0x800; // [0]=源号/pending [16]=收到的字节 [24]=M handler 留痕
const PLIC: u64 = 0x0C00_0000;
const UART: u64 = 0x1000_0000;

/// MMIO 绝对地址 → 相对 DRAM_BASE 的链接偏移（addr_of 按 pc 相对寻址）
fn pa(x: u64) -> i32 {
    (x as i64 - DRAM_BASE as i64) as i32
}

// 主循环用 s0/s1：handler（裸用 t3-t5）不保存/恢复，不能污染轮询寄存器
const S0: u32 = 8;
const S1: u32 = 9;

#[test]
fn uart_rx_interrupt_via_plic_m_mode() {
    let mut a = Asm::new();
    use reg::*;

    // M handler @0x200
    a.addr_of(T0, 0x200);
    a.emit32(csrw(0x305, T0)); // mtvec
    // PLIC ctx0：priority[10]=1、enable |= 1<<10（threshold 复位即 0）
    a.addr_of(T3, pa(PLIC));
    a.emit32(addi(T4, ZERO, 1));
    a.emit32(sw(T4, T3, (4 * UART_IRQ) as i32));
    a.addr_of(T3, pa(PLIC + 0x2000));
    a.emit32(ld(T4, T3, 0));
    a.emit32(addi(T5, ZERO, 1 << UART_IRQ));
    a.emit32(add(T4, T4, T5));
    a.emit32(sd(T4, T3, 0));
    // UART IER.0 = 1：使能接收中断（8250 语义）
    a.addr_of(T3, pa(UART + 1));
    a.emit32(addi(T4, ZERO, 1));
    a.emit32(sb(T4, T3, 0));
    // mie.MEIE=1，mstatus.MIE=1
    a.emit32(addi(T1, ZERO, 1));
    a.emit32(slli(T1, T1, 11));
    a.emit32(csrw(0x304, T1));
    a.emit32(addi(T1, ZERO, 8));
    a.emit32(csrs(0x300, T1));
    // 轮询结果
    a.addr_of(S0, RESULTS as i32);
    let poll = a.pc();
    a.emit32(ld(S1, S0, 0));
    a.emit32(beqz(S1, poll - a.pc()));
    a.emit32(addi(A0, ZERO, 11));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    // M handler：claim → 读 RBR → complete → mret
    a.pad_to(0x200);
    a.addr_of(T3, pa(PLIC + 0x20_0004)); // ctx0 claim
    a.emit32(lw(T4, T3, 0));
    a.addr_of(T5, RESULTS as i32);
    a.emit32(sd(T4, T5, 0)); // [0] = 源号
    a.addr_of(T3, pa(UART));
    a.emit32(lb(T4, T3, 0)); // RBR
    a.addr_of(T5, RESULTS as i32);
    a.emit32(sb(T4, T5, 16)); // [16] = 字节
    a.emit32(ld(T4, T5, 0)); // 源号回取
    a.addr_of(T3, pa(PLIC + 0x20_0004));
    a.emit32(sw(T4, T3, 0)); // complete
    a.emit32(MRET);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(5_000_000), Halt::Timeout, "未注入字节不应触发中断");

    m.bus.uart.receive(b'X');
    assert_eq!(m.run(5_000_000), Halt::Exit(11));
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS, 4).unwrap(), UART_IRQ as u64);
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS + 16, 1).unwrap(), b'X' as u64);
}

#[test]
fn uart_rx_interrupt_delegated_to_s() {
    let mut a = Asm::new();
    use reg::*;

    // ---- M 初始化：委托 SEIP(bit9) 到 S，PLIC 只开 S context，mret 进 S ----
    a.addr_of(T0, 0x100);
    a.emit32(csrw(0x341, T0)); // mepc = S 入口
    a.emit32(addi(T1, ZERO, 1 << 9));
    a.emit32(csrw(0x303, T1)); // mideleg |= SEIP
    a.addr_of(T3, pa(PLIC));
    a.emit32(addi(T4, ZERO, 1));
    a.emit32(sw(T4, T3, (4 * UART_IRQ) as i32)); // priority[10] = 1
    a.addr_of(T3, pa(PLIC + 0x2000 + 0x80)); // enable ctx1（S）
    a.emit32(ld(T4, T3, 0));
    a.emit32(addi(T5, ZERO, 1 << UART_IRQ));
    a.emit32(add(T4, T4, T5));
    a.emit32(sd(T4, T3, 0));
    // mtvec = M handler（不该触发），stvec = S handler
    a.addr_of(T0, 0x180);
    a.emit32(csrw(0x305, T0));
    a.addr_of(T0, 0x280);
    a.emit32(csrw(0x105, T0));
    // UART IER.0 = 1：使能接收中断（8250 语义）
    a.addr_of(T3, pa(UART + 1));
    a.emit32(addi(T4, ZERO, 1));
    a.emit32(sb(T4, T3, 0));
    // sie.SEIE=1，sstatus.SIE=1
    a.emit32(addi(T1, ZERO, 1 << 9));
    a.emit32(csrw(0x104, T1));
    a.emit32(addi(T1, ZERO, 2));
    a.emit32(csrs(0x100, T1));
    // MPP=S，mret 进 S
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // ---- S 入口：轮询 ----
    a.pad_to(0x100);
    a.addr_of(S0, RESULTS as i32);
    let poll = a.pc();
    a.emit32(ld(S1, S0, 0));
    a.emit32(beqz(S1, poll - a.pc()));
    // 退出：test 设备（S 态 ecall 非宿主调用），退出码 6
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 0x65));
    a.emit32(addi(T1, T1, 0x555)); // 0x65555 = (6<<16)|0x5555
    a.emit32(sd(T1, T0, 0));

    // ---- S handler：claim(ctx1) → RBR → complete → sret ----
    a.pad_to(0x280);
    a.addr_of(T3, pa(PLIC + 0x20_1004)); // ctx1 claim
    a.emit32(lw(T4, T3, 0));
    a.addr_of(T5, RESULTS as i32);
    a.emit32(sd(T4, T5, 0));
    a.addr_of(T3, pa(UART));
    a.emit32(lb(T4, T3, 0)); // RBR
    a.addr_of(T5, RESULTS as i32);
    a.emit32(sb(T4, T5, 16));
    a.emit32(ld(T4, T5, 0));
    a.addr_of(T3, pa(PLIC + 0x20_1004));
    a.emit32(sw(T4, T3, 0)); // complete
    a.emit32(SRET);

    // ---- M handler：不应触发（触发则在 [24] 留痕并以退出码 1 失败）----
    a.pad_to(0x180);
    a.addr_of(T5, RESULTS as i32);
    a.emit32(addi(T4, ZERO, 0x77));
    a.emit32(sb(T4, T5, 24));
    a.emit32(addi(A0, ZERO, 1));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    m.bus.uart.receive(b'S');
    assert_eq!(m.run(5_000_000), Halt::Exit(6));
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS, 4).unwrap(), UART_IRQ as u64);
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS + 16, 1).unwrap(), b'S' as u64);
    assert_ne!(
        m.bus.load(DRAM_BASE + RESULTS + 24, 1).unwrap(),
        0x77,
        "M handler 不应被触发（SEIP 已委派且 M context 未使能）"
    );
}

#[test]
fn plic_threshold_and_pending_regs() {
    let mut a = Asm::new();
    use reg::*;
    // priority[10] = 0x7F → WARL 截断到 3 位 = 7；threshold = 5；读回
    a.addr_of(T3, pa(PLIC));
    a.emit32(addi(T4, ZERO, 0x7F));
    a.emit32(sw(T4, T3, (4 * UART_IRQ) as i32));
    a.addr_of(T3, pa(PLIC + 0x20_0000));
    a.emit32(addi(T4, ZERO, 5));
    a.emit32(sw(T4, T3, 0));
    a.addr_of(T4, RESULTS as i32);
    a.addr_of(T3, pa(PLIC));
    a.emit32(lw(T5, T3, 4 * 10));
    a.emit32(sw(T5, T4, 0));
    a.addr_of(T3, pa(PLIC + 0x20_0000));
    a.emit32(lw(T5, T3, 0));
    a.emit32(sw(T5, T4, 8));
    a.addr_of(T3, pa(PLIC + 0x1000));
    a.emit32(lw(T5, T3, 0));
    a.emit32(sw(T5, T4, 16));
    a.emit32(addi(A0, ZERO, 12));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(1_000_000), Halt::Exit(12));
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS, 4).unwrap(),
        7,
        "优先级 0x7F 应 WARL 到 7"
    );
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS + 8, 4).unwrap(), 5, "阈值读回");
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS + 16, 4).unwrap(),
        0,
        "无注入应无挂起"
    );
}

/// 直接轮询 pending 寄存器：注入前为 0，注入后（无需使能交付）可见 UART 源
#[test]
fn plic_pending_only_when_line_high() {
    let mut a = Asm::new();
    use reg::*;
    // UART IER.0=1 使能接收中断（线电平跟随注入）
    a.addr_of(T3, pa(UART + 1));
    a.emit32(addi(T4, ZERO, 1));
    a.emit32(sb(T4, T3, 0));
    // 轮询 pending 字 0，非零则记录并退出
    a.addr_of(S0, RESULTS as i32);
    a.addr_of(T3, pa(PLIC + 0x1000));
    let poll = a.pc();
    a.emit32(lw(S1, T3, 0));
    a.emit32(beqz(S1, poll - a.pc()));
    a.emit32(sw(S1, S0, 0));
    a.emit32(addi(A0, ZERO, 13));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(500_000), Halt::Timeout, "注入前 pending 应为 0");
    m.bus.uart.receive(b'P');
    assert_eq!(m.run(500_000), Halt::Exit(13));
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS, 4).unwrap(),
        1 << UART_IRQ,
        "pending 位图应含 UART 源"
    );
}
