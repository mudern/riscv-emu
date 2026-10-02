//! 特权级集成测试：M→S→U 切换、ecall 委托、U 态 CSR 访问陷阱。

mod common;

use common::*;
use riscv_emu::Halt;

// 布局（页内偏移）
const S_ENTRY: i32 = 0x100;
const S_HANDLER: i32 = 0x180;
const U_ENTRY: i32 = 0x200;
const M_HANDLER: i32 = 0x280;
const RESULTS: u64 = 0x800; // [0]=U 标记 [8]=S-trap 计数 [16..]=scause 序列

#[test]
fn privilege_transitions_and_delegation() {
    let mut a = Asm::new();
    use reg::*;

    // ---- M 入口 ----
    a.addr_of(T0, M_HANDLER);
    a.emit32(csrw(0x305, T0)); // mtvec = m_handler
    // 委托 ecall-from-U(8)、illegal(2) 到 S
    a.emit32(addi(T1, ZERO, 0x100 | 0x4));
    a.emit32(csrw(0x302, T1)); // medeleg
    a.addr_of(T0, S_HANDLER);
    a.emit32(csrw(0x105, T0)); // stvec
    // mepc = S 入口，MPP=S
    a.addr_of(T1, S_ENTRY);
    a.emit32(csrw(0x341, T1)); // mepc
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11)); // MPP = 01 (S)
    a.emit32(csrs(0x300, T2)); // mstatus |= MPP
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // ---- S 入口：sret 进 U ----
    a.pad_to(S_ENTRY as usize);
    a.addr_of(T1, U_ENTRY);
    a.emit32(csrw(0x141, T1)); // sepc
    a.emit32(SRET); // SPP=0 → U

    // ---- S handler：记录 scause、计数、sepc+=4、sret ----
    a.pad_to(S_HANDLER as usize);
    a.addr_of(T4, RESULTS as i32);
    a.emit32(csrr(T0, 0x142)); // scause
    a.emit32(ld(T1, T4, 8)); // 计数
    a.emit32(slli(T1, T1, 3)); // 偏移 = count*8
    a.emit32(add(T1, T1, T4));
    a.emit32(sd(T0, T1, 16)); // results[16 + count*8] = scause
    a.emit32(ld(T1, T4, 8));
    a.emit32(addi(T1, T1, 1));
    a.emit32(sd(T1, T4, 8));
    a.emit32(csrr(T0, 0x141)); // sepc
    a.emit32(addi(T0, T0, 4));
    a.emit32(csrw(0x141, T0));
    a.emit32(SRET);

    // ---- U 入口 ----
    a.pad_to(U_ENTRY as usize);
    a.addr_of(T4, RESULTS as i32);
    a.emit32(addi(T5, ZERO, 0x5A)); // U 运行标记
    a.emit32(sd(T5, T4, 0));
    a.emit32(ECALL); // → S (cause 8)
    a.emit32(0x0000_0000); // illegal → S (cause 2)
    a.emit32(csrr(T0, 0x300)); // U 读 mstatus → illegal → S (cause 2)
    a.emit32(ECALL); // → S (cause 8)
    // 正常退出：U 写 sifive_test 设备（无 MMU，物理直达）
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 5));
    a.emit32(addi(T1, T1, 0x555)); // 0x5555
    a.emit32(sd(T1, T0, 0));
    a.emit32(ECALL); // 不应到达（保底陷入循环由超时兜底）

    // ---- M handler：不应到达，exit(99) ----
    a.pad_to(M_HANDLER as usize);
    a.emit32(addi(A0, ZERO, 99));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    match m.run(1_000_000) {
        Halt::Exit(0) => {}
        other => panic!("期望 Exit(0)，得到 {other:?}"),
    }

    let base = DRAM_BASE + RESULTS;
    assert_eq!(m.bus.load(base, 8).unwrap(), 0x5A, "U 态执行标记");
    let count = m.bus.load(base + 8, 8).unwrap();
    assert_eq!(count, 4, "S handler 应处理 4 个 trap");
    let causes: Vec<u64> = (0..4)
        .map(|i| m.bus.load(base + 16 + i * 8, 8).unwrap())
        .collect();
    assert_eq!(causes, vec![8, 2, 2, 8], "scause 序列");
    assert_eq!(m.cpu.privilege, riscv_emu::Privilege::U);
}

/// S 态 ecall 不委托：应进 M 态 handler（cause 9）
#[test]
fn s_ecall_goes_to_m() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, 0x180); // M handler
    a.emit32(csrw(0x305, T0)); // mtvec
    a.addr_of(T1, 0x100); // S 入口
    a.emit32(csrw(0x341, T1)); // mepc
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    pmp_open_all(&mut a);
    a.emit32(MRET);

    a.pad_to(0x100);
    a.emit32(ECALL); // S ecall → M
    a.emit32(jal(ZERO, 0)); // 不应回到这里：原地死循环兜底

    // M handler：记录 mcause，退出(0)
    a.pad_to(0x180);
    a.emit32(csrr(T0, 0x342)); // mcause
    a.addr_of(T4, 0x800);
    a.emit32(sd(T0, T4, 0));
    a.emit32(addi(A0, ZERO, 0));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(100_000), Halt::Exit(0));
    assert_eq!(m.bus.load(DRAM_BASE + 0x800, 8).unwrap(), 9);
}

/// U 态执行 sret 应触发 illegal instruction（委托到 S 后被 handler 跳过）
#[test]
fn u_mode_sret_illegal() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, 0x280); // m handler（未预期）
    a.emit32(csrw(0x305, T0));
    a.emit32(addi(T1, ZERO, 0x4)); // medeleg: illegal
    a.emit32(csrw(0x302, T1));
    a.addr_of(T0, 0x180); // S handler
    a.emit32(csrw(0x105, T0));
    a.addr_of(T1, 0x100);
    a.emit32(csrw(0x341, T1));
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // S: sret 到 U
    a.pad_to(0x100);
    a.addr_of(T1, 0x200);
    a.emit32(csrw(0x141, T1));
    a.emit32(SRET);

    // S handler: 记录 scause，sepc+=4，sret
    a.pad_to(0x180);
    a.emit32(csrr(T0, 0x142));
    a.addr_of(T4, 0x800);
    a.emit32(sd(T0, T4, 0));
    a.emit32(csrr(T0, 0x141));
    a.emit32(addi(T0, T0, 4));
    a.emit32(csrw(0x141, T0));
    a.emit32(SRET);

    // U: sret 本身非法 → S handler 跳过后退出
    a.pad_to(0x200);
    a.emit32(SRET);
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 5));
    a.emit32(addi(T1, T1, 0x555));
    a.emit32(sd(T1, T0, 0));

    // M handler: 退出 99
    a.pad_to(0x280);
    a.emit32(addi(A0, ZERO, 99));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(100_000), Halt::Exit(0));
    assert_eq!(m.bus.load(DRAM_BASE + 0x800, 8).unwrap(), 2, "sret in U 应为 illegal");
}
