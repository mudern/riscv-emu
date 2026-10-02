//! CLINT 定时器中断测试：
//! 1) M 态直接处理（MTIP）
//! 2) OpenSBI 风格转发：M 态固件收 MTIP 后置 mip.STIP，委托到 S 态处理

mod common;

use common::*;
use riscv_emu::Halt;

const RESULTS: u64 = 0x800; // [0]=计数 [8]=mcause [16]=scause

#[test]
fn m_timer_interrupt() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, 0x200); // M handler
    a.emit32(csrw(0x305, T0)); // mtvec
    // mtimecmp = 2000（约 200µs 后到期）
    a.emit32(lui(T0, 0x2004));
    a.emit32(addi(T1, ZERO, 2000));
    a.emit32(sd(T1, T0, 0));
    // mie.MTIE=1, mstatus.MIE=1
    a.emit32(addi(T1, ZERO, 0x80));
    a.emit32(csrw(0x304, T1));
    a.emit32(addi(T1, ZERO, 8));
    a.emit32(csrs(0x300, T1));
    // 等中断：轮询计数
    a.addr_of(T4, RESULTS as i32);
    let poll = a.pc();
    a.emit32(ld(T5, T4, 0));
    a.emit32(beqz(T5, poll - a.pc())); // beqz t5, poll
    // 收到后退出(7)
    a.emit32(addi(A0, ZERO, 7));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    // M handler: 清 mtimecmp（写 -1），计数+1，记录 mcause，mret
    a.pad_to(0x200);
    a.emit32(lui(T0, 0x2004));
    a.emit32(addi(T1, ZERO, -1));
    a.emit32(sd(T1, T0, 0));
    a.addr_of(T4, RESULTS as i32);
    a.emit32(ld(T5, T4, 0));
    a.emit32(addi(T5, T5, 1));
    a.emit32(sd(T5, T4, 0));
    a.emit32(csrr(T5, 0x342)); // mcause
    a.emit32(sd(T5, T4, 8));
    a.emit32(MRET);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(50_000_000), Halt::Exit(7));
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS, 8).unwrap(), 1);
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS + 8, 8).unwrap(),
        0x8000_0000_0000_0007,
        "mcause 应为 M 态定时器中断"
    );
}

/// M 态固件收 MTIP → 置 mip.STIP 转发 → S 态以 STIP（cause 5）处理
#[test]
fn s_timer_forwarded_like_opensbi() {
    let mut a = Asm::new();
    use reg::*;

    // ---- M 初始化 ----
    a.addr_of(T0, 0x100);
    a.emit32(csrw(0x341, T0)); // mepc = S 入口
    // mideleg: STIP(bit5)
    a.emit32(addi(T1, ZERO, 1 << 5));
    a.emit32(csrw(0x303, T1));
    // sstatus.SIE=1（sie.STIE 在 S 入口再开——避免 M 初始化窗口被
    // 转发的 STIP 打断：委派中断在 M 态同样按 S 集使能（QEMU hsie））
    a.emit32(addi(T1, ZERO, 2));
    a.emit32(csrs(0x100, T1)); // sstatus |= SIE
    // mtvec = M handler（转发用），stvec = S handler（中断使能之前设好）
    a.addr_of(T0, 0x180);
    a.emit32(csrw(0x305, T0)); // mtvec
    a.addr_of(T0, 0x280);
    a.emit32(csrw(0x105, T0)); // stvec
    // mtimecmp = mtime + 2000（相对 200µs；绝对值会被墙钟 mtime 越过）
    a.emit32(lui(T0, 0x2004)); // mtimecmp = 0x0200_4000
    a.emit32(lui(T3, 0x200C)); // mtime = 0x0200_BFF8（0x200C000-8）
    a.emit32(addi(T3, T3, -8));
    a.emit32(ld(T1, T3, 0));
    a.emit32(addi(T1, T1, 2000));
    a.emit32(sd(T1, T0, 0));
    // mie.MTIE=1（M 态自己收硬件定时器），mstatus.MIE=1
    a.emit32(addi(T1, ZERO, 0x80));
    a.emit32(csrs(0x304, T1));
    a.emit32(addi(T1, ZERO, 8));
    a.emit32(csrs(0x300, T1));
    // MPP=S，mret 进 S
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // ---- S 入口：开 sie.STIE 后轮询计数 ----
    a.pad_to(0x100);
    a.emit32(addi(T1, ZERO, 1 << 5));
    a.emit32(csrw(0x104, T1)); // sie.STIP=1
    a.addr_of(T4, RESULTS as i32);
    let poll = a.pc();
    a.emit32(ld(T5, T4, 0));
    a.emit32(beqz(T5, poll - a.pc()));
    // S 态退出走 test 设备（S 态 ecall 是异常而非宿主调用）；退出码取高 16 位
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 0x55));
    a.emit32(addi(T1, T1, 0x555)); // (5 << 16) | FINISHER_PASS
    a.emit32(sd(T1, T0, 0));

    // ---- M handler（MTIP）：清 mtimecmp，置 mip.STIP 转发，mret ----
    a.pad_to(0x180);
    a.emit32(lui(T0, 0x2004));
    a.emit32(addi(T1, ZERO, -1));
    a.emit32(sd(T1, T0, 0)); // mtimecmp = MAX
    a.emit32(addi(T1, ZERO, 1 << 5)); // STIP
    a.emit32(csrs(0x344, T1)); // mip |= STIP
    a.emit32(MRET);

    // ---- S handler（STIP）：清 sip.STIP，计数+1，记录 scause，sret ----
    a.pad_to(0x280);
    a.emit32(addi(T0, ZERO, 0)); // 清 STIP：csrw sip, 0（委托位可写）
    a.emit32(csrw(0x144, T0));
    a.addr_of(T4, RESULTS as i32);
    a.emit32(ld(T5, T4, 0));
    a.emit32(addi(T5, T5, 1));
    a.emit32(sd(T5, T4, 0));
    a.emit32(csrr(T5, 0x142)); // scause
    a.emit32(sd(T5, T4, 16));
    a.emit32(SRET);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(50_000_000), Halt::Exit(5));
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS, 8).unwrap(), 1);
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS + 16, 8).unwrap(),
        0x8000_0000_0000_0005,
        "scause 应为 S 态定时器中断"
    );
}
