//! 内置 SBI 冒烟测试：开启 `cpu.sbi` 后 S 态 ecall 不再走异常交付，
//! 而是按 SBI 调用处理（legacy console_putchar / shutdown）。

mod common;

use common::*;
use riscv_emu::Halt;

#[test]
fn sbi_putchar_and_shutdown() {
    let mut a = Asm::new();
    use reg::*;

    // M：mepc = S 入口，MPP=S，PMP 开放，mret
    a.addr_of(T0, 0x100);
    a.emit32(csrw(0x341, T0));
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // S：console_putchar('H')，再 shutdown(0)
    a.pad_to(0x100);
    a.emit32(addi(A0, ZERO, 0x48)); // 'H'
    a.emit32(addi(A7, ZERO, 0x01)); // legacy console_putchar
    a.emit32(ECALL);
    a.emit32(addi(A0, ZERO, 0)); // shutdown type
    a.emit32(addi(A7, ZERO, 0x08)); // legacy shutdown
    a.emit32(ECALL);
    a.emit32(jal(ZERO, 0)); // 不应到达

    let mut m = a.into_machine(16);
    m.cpu.sbi = true;
    assert_eq!(m.run(100_000), Halt::Exit(0));
    assert_eq!(m.console, b"H");
}
