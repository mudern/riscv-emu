//! PMP 集成测试：
//! 1) 空 PMP 下 mret 进 S → 在 mret 处 instruction access fault（对齐 QEMU）
//! 2) TOR 只读区（R|X）：S 态读/取指允许，写 → store access fault
//! 3) pmpaddr/pmpcfg 是 M 态 CSR，S 态访问 → illegal instruction

mod common;

use common::*;
use riscv_emu::Halt;

const RESULTS: u64 = 0x800; // [0]=mcause [8]=mepc [16]=mtval
const S_CODE: i32 = 0x100;
const M_HANDLER: i32 = 0x180;

#[test]
fn empty_pmp_mret_faults_at_mret() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, M_HANDLER);
    a.emit32(csrw(0x305, T0)); // mtvec
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11)); // MPP = S
    a.emit32(csrs(0x300, T2));
    let mret_pc = a.pc() as u64;
    a.emit32(MRET); // ← 无 PMP 规则：这里应 fault

    // S 出口（handler 重新 mret 的目标）：写 test 设备退出
    a.pad_to(S_CODE as usize);
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 5));
    a.emit32(addi(T1, T1, 0x555));
    a.emit32(sd(T1, T0, 0));

    // M handler：记录现场 → 开放 PMP → 重设 MPP=S → mret 进 S 出口
    a.pad_to(M_HANDLER as usize);
    a.addr_of(T4, RESULTS as i32);
    a.emit32(csrr(T0, 0x342)); // mcause
    a.emit32(sd(T0, T4, 0));
    a.emit32(csrr(T0, 0x341)); // mepc
    a.emit32(sd(T0, T4, 8));
    a.emit32(csrr(T0, 0x343)); // mtval
    a.emit32(sd(T0, T4, 16));
    pmp_open_all(&mut a);
    // MPP 整体重写为 S（fault 时 MPP=M，csrs OR 不掉高位）
    a.emit32(lui(T2, 1));
    a.emit32(addi(T2, T2, -0x780)); // MPIE | MPP=S = 0x880
    a.emit32(csrw(0x300, T2));
    a.addr_of(T0, S_CODE);
    a.emit32(csrw(0x341, T0)); // mepc = S 出口
    a.emit32(MRET);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(100_000), Halt::Exit(0));
    let base = DRAM_BASE + RESULTS;
    assert_eq!(m.bus.load(base, 8).unwrap(), 1, "mcause = instruction access fault");
    assert_eq!(m.bus.load(base + 8, 8).unwrap(), DRAM_BASE + mret_pc, "mepc = mret 的 pc");
    assert_eq!(m.bus.load(base + 16, 8).unwrap(), 0, "mtval = 0（与 QEMU 一致）");
    assert_eq!(m.cpu.privilege, riscv_emu::Privilege::S);
}

/// TOR 规则 [0x8000_0000, 0x8010_0000) 只给 R|X：
/// S 态取指/读通过，写 → store access fault（cause=7，mtval=虚拟地址）
#[test]
fn readonly_pmp_denies_s_store() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, M_HANDLER);
    a.emit32(csrw(0x305, T0));
    // pmpaddr0 = 0x8000_0000>>2 = 0x2000_0000，pmpaddr1 = 0x8010_0000>>2
    a.emit32(lui(T0, 0x20000));
    a.emit32(csrw(0x3B0, T0));
    a.emit32(lui(T1, 0x2_0004)); // 0x2004_0000 = 0x8010_0000>>2
    a.emit32(csrw(0x3B1, T1));
    // pmpcfg0 字节 0/1：TOR 边界 / TOR|R|X（不给 W）→ 0x0D08
    a.emit32(lui(T2, 1));
    a.emit32(addi(T2, T2, -0x2F8));
    a.emit32(csrw(0x3A0, T2));
    // mret 进 S
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    a.addr_of(T0, S_CODE);
    a.emit32(csrw(0x341, T0));
    a.emit32(MRET);

    // S：先读（R 允许），再写（W 拒绝 → fault）
    a.pad_to(S_CODE as usize);
    a.addr_of(T4, 0x900);
    a.emit32(ld(T5, T4, 0));
    let store_pc = a.pc() as u64;
    a.emit32(sd(T5, T4, 0)); // ← fault
    a.emit32(jal(ZERO, 0)); // 不应到达

    // M handler：记录现场后退出（M 态写 test 设备不受 PMP 限制）
    a.pad_to(M_HANDLER as usize);
    a.addr_of(T4, RESULTS as i32);
    a.emit32(csrr(T0, 0x342));
    a.emit32(sd(T0, T4, 0));
    a.emit32(csrr(T0, 0x341));
    a.emit32(sd(T0, T4, 8));
    a.emit32(csrr(T0, 0x343));
    a.emit32(sd(T0, T4, 16));
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 5));
    a.emit32(addi(T1, T1, 0x555));
    a.emit32(sd(T1, T0, 0));

    let mut m = a.into_machine(16);
    assert_eq!(m.run(100_000), Halt::Exit(0));
    let base = DRAM_BASE + RESULTS;
    assert_eq!(m.bus.load(base, 8).unwrap(), 7, "mcause = store access fault");
    assert_eq!(m.bus.load(base + 8, 8).unwrap(), DRAM_BASE + store_pc);
    assert_eq!(
        m.bus.load(base + 16, 8).unwrap(),
        DRAM_BASE + 0x900,
        "mtval = 故障虚拟地址"
    );
    assert_eq!(m.cpu.privilege, riscv_emu::Privilege::M);
}

/// pmpaddr/pmpcfg 是 M 态 CSR：S 态 csrr pmpaddr0 → illegal instruction（cause=2）
#[test]
fn s_mode_pmp_csr_illegal() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, M_HANDLER);
    a.emit32(csrw(0x305, T0));
    pmp_open_all(&mut a);
    a.emit32(addi(T2, ZERO, 1));
    a.emit32(slli(T2, T2, 11));
    a.emit32(csrs(0x300, T2));
    a.addr_of(T0, S_CODE);
    a.emit32(csrw(0x341, T0));
    a.emit32(MRET);

    // S：读 pmpaddr0 → illegal
    a.pad_to(S_CODE as usize);
    a.emit32(csrr(T0, 0x3B0));
    a.emit32(jal(ZERO, 0));

    // M handler：记录 mcause/mepc 后退出
    a.pad_to(M_HANDLER as usize);
    a.addr_of(T4, RESULTS as i32);
    a.emit32(csrr(T0, 0x342));
    a.emit32(sd(T0, T4, 0));
    a.emit32(csrr(T0, 0x341));
    a.emit32(sd(T0, T4, 8));
    a.emit32(lui(T0, 0x100));
    a.emit32(lui(T1, 5));
    a.emit32(addi(T1, T1, 0x555));
    a.emit32(sd(T1, T0, 0));

    let mut m = a.into_machine(16);
    assert_eq!(m.run(100_000), Halt::Exit(0));
    let base = DRAM_BASE + RESULTS;
    assert_eq!(m.bus.load(base, 8).unwrap(), 2, "mcause = illegal instruction");
    assert_eq!(
        m.bus.load(base + 8, 8).unwrap(),
        DRAM_BASE + S_CODE as u64,
        "mepc = csrr 的 pc"
    );
}
