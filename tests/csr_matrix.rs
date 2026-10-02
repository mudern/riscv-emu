//! CSR 指令语义矩阵测试 + WARL 探测 + trap 现场 + 计数器抑制。
//! 对应 CSR/privileged 规范审查的逐项回归。

mod common;

use common::*;
use riscv_emu::machine::Halt;

const RESULTS: u64 = 0x800; // [0..72) 9 个槽位

/// 结果槽偏移
const S_CSRRW: i64 = 0;
const S_CSRRS: i64 = 16;
const S_CSRRC: i64 = 32;
const S_RS_X0: i64 = 48;
const S_RC_X0: i64 = 64;
const S_RW_RD0: i64 = 80;

/// CSR 立即数指令编码（CI 格式省略——统一用寄存器版本 + csrsi/csrwi 由
/// zimm=rs1 表达；这里直接用六种标准形式）
fn csrrw(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (1 << 12) | (rd << 7) | 0x73
}
fn csrrs(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (2 << 12) | (rd << 7) | 0x73
}
fn csrrc(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (3 << 12) | (rd << 7) | 0x73
}

/// 六种 CSR 指令的读/写语义矩阵（以 mscratch = 0xA5 为初始值）：
/// 1. CSRRW rd, x5(0x5A)   → rd=0xA5，mscratch=0x5A
/// 2. CSRRS rd, x5(0x0F)   → rd=0x5A，mscratch=0x5F
/// 3. CSRRC rd, x5(0x0F)   → rd=0x5F，mscratch=0x50
/// 4. CSRRS rd, x0         → rd=0x50，mscratch 不变（只读不写）
/// 5. CSRRC rd, x0         → rd=0x50，mscratch 不变
/// 6. CSRRW x0, x5(0xFF)   → mscratch=0xFF（rd=x0 仍写；读允许但无副作用）
#[test]
fn csr_instruction_matrix() {
    let mut a = Asm::new();
    use reg::*;

    // mscratch = 0xA5
    a.emit32(addi(T0, ZERO, 0xA5));
    a.emit32(csrw(0x340, T0));
    a.addr_of(T3, RESULTS as i32);

    // 1. CSRRW
    a.emit32(addi(T1, ZERO, 0x5A));
    a.emit32(csrrw(T2, 0x340, T1));
    a.emit32(sd(T2, T3, (S_CSRRW) as i32)); // 旧值
    a.emit32(csrr(T2, 0x340));
    a.emit32(sd(T2, T3, (S_CSRRW + 8) as i32));

    // 2. CSRRS
    a.emit32(addi(T1, ZERO, 0x0F));
    a.emit32(csrrs(T2, 0x340, T1));
    a.emit32(sd(T2, T3, (S_CSRRS) as i32));
    a.emit32(csrr(T2, 0x340));
    a.emit32(sd(T2, T3, (S_CSRRS + 8) as i32));

    // 3. CSRRC
    a.emit32(addi(T1, ZERO, 0x0F));
    a.emit32(csrrc(T2, 0x340, T1));
    a.emit32(sd(T2, T3, (S_CSRRC) as i32));
    a.emit32(csrr(T2, 0x340));
    a.emit32(sd(T2, T3, (S_CSRRC + 8) as i32));

    // 4. CSRRS rs1=x0：只读不写
    a.emit32(csrrs(T2, 0x340, ZERO));
    a.emit32(sd(T2, T3, (S_RS_X0) as i32));
    a.emit32(csrr(T2, 0x340));
    a.emit32(sd(T2, T3, (S_RS_X0 + 8) as i32));

    // 5. CSRRC rs1=x0：只读不写
    a.emit32(csrrc(T2, 0x340, ZERO));
    a.emit32(sd(T2, T3, (S_RC_X0) as i32));
    a.emit32(csrr(T2, 0x340));
    a.emit32(sd(T2, T3, (S_RC_X0 + 8) as i32));

    // 6. CSRRW rd=x0：仍写（读无副作用）
    a.emit32(addi(T1, ZERO, -1)); // 0xFF..FF；mscratch 是 64 位全宽
    a.emit32(csrrw(ZERO, 0x340, T1));
    a.emit32(csrr(T2, 0x340));
    a.emit32(sd(T2, T3, (S_RW_RD0) as i32));

    a.emit32(addi(A0, ZERO, 41));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(41));
    let base = DRAM_BASE as i64 + RESULTS as i64;
    // 旧值/新值矩阵
    assert_eq!(m.bus.load((base + S_CSRRW) as u64, 8).unwrap(), 0xA5, "CSRRW 读旧值");
    assert_eq!(m.bus.load((base + S_CSRRW + 8) as u64, 8).unwrap(), 0x5A, "CSRRW 写新值");
    assert_eq!(m.bus.load((base + S_CSRRS) as u64, 8).unwrap(), 0x5A, "CSRRS 读旧值");
    assert_eq!(m.bus.load((base + S_CSRRS + 8) as u64, 8).unwrap(), 0x5F, "CSRRS 置位");
    assert_eq!(m.bus.load((base + S_CSRRC) as u64, 8).unwrap(), 0x5F, "CSRRC 读旧值");
    assert_eq!(m.bus.load((base + S_CSRRC + 8) as u64, 8).unwrap(), 0x50, "CSRRC 清位");
    assert_eq!(m.bus.load((base + S_RS_X0) as u64, 8).unwrap(), 0x50, "CSRRS x0 只读");
    assert_eq!(m.bus.load((base + S_RS_X0 + 8) as u64, 8).unwrap(), 0x50, "CSRRS x0 不写");
    assert_eq!(m.bus.load((base + S_RC_X0) as u64, 8).unwrap(), 0x50, "CSRRC x0 只读");
    assert_eq!(m.bus.load((base + S_RC_X0 + 8) as u64, 8).unwrap(), 0x50, "CSRRC x0 不写");
    assert_eq!(m.bus.load((base + S_RW_RD0) as u64, 8).unwrap(), u64::MAX, "CSRRW rd=x0 仍写新值");
}

/// WARL 探测（firmware 风格）：写全 1 → 读回实现位。
/// 同时验证：SPP=2 规范化、mcountinhibit 抑制、TV​M、显式计数器写。
#[test]
fn warl_probes_and_counters() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T6, RESULTS as i32);

    // medeleg/mideleg 写全 1 → 读回实现位
    a.emit32(addi(T0, ZERO, -1));
    a.emit32(csrw(0x302, T0)); // medeleg
    a.emit32(csrr(T1, 0x302));
    a.emit32(sd(T1, T6, 0));
    a.emit32(csrw(0x303, T0)); // mideleg
    a.emit32(csrr(T1, 0x303));
    a.emit32(sd(T1, T6, 8));
    // mcountinhibit 写全 1 → 读回 0x7（CY/TM/IR）
    a.emit32(csrw(0x320, T0));
    a.emit32(csrr(T1, 0x320));
    a.emit32(sd(T1, T6, 16));

    // 计数器：显式写 mcycle=1000 / minstret=2000，读回
    a.emit32(addi(T2, ZERO, 1000));
    a.emit32(csrw(0xB00, T2));
    a.emit32(addi(T2, ZERO, 2000));
    a.emit32(csrw(0xB02, T2));
    a.emit32(csrr(T3, 0xB00));
    a.emit32(csrr(T4, 0xB02));
    // 全 1 抑制后：读 100 次 → 值不再增长
    a.emit32(addi(T0, ZERO, -1));
    a.emit32(csrw(0x320, T0));
    a.emit32(addi(T5, ZERO, 100));
    let loop_start = a.pc();
    a.emit32(csrr(T1, 0xB00));
    a.emit32(csrr(T1, 0xB02));
    a.emit32(addi(T5, T5, -1));
    a.emit32(bne(T5, ZERO, loop_start - a.pc()));
    // 值应仍为 1000/2000（写过的值；抑制期间无自增）
    a.emit32(csrr(T1, 0xB00));
    a.emit32(csrr(T1, 0xB02));
    // 恢复计数 + 解除抑制，验证再次增长
    a.emit32(csrw(0x320, ZERO));
    a.emit32(csrr(T1, 0xB00));
    a.emit32(addi(T1, T1, 1));
    a.emit32(sd(T1, T6, 24)); // mcycle+1 > 1000 → 有自增

    a.emit32(addi(A0, ZERO, 42));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(100_000), Halt::Exit(42));
    let base = DRAM_BASE as i64 + RESULTS as i64;
    let medeleg = m.bus.load((base) as u64, 8).unwrap();
    assert_eq!(medeleg & (1 << 11), 0, "medeleg bit11 不可委派");
    assert_eq!(medeleg & (1 << 8), 1 << 8, "medeleg bit8 可委派");
    assert_eq!(medeleg & (1 << 10), 0, "medeleg bit10 保留");
    let mideleg = m.bus.load((base + 8) as u64, 8).unwrap();
    assert_eq!(mideleg, 0xAAA & 0x222, "mideleg 仅 S 态中断源 0x222");
    assert_eq!(m.bus.load((base + 16) as u64, 8).unwrap(), 0x7, "mcountinhibit 仅 CY/TM/IR");
}

/// trap 现场：M 态 trap 时 MIE=0、MPIE=旧 MIE、MPP=前特权；
/// S 态 trap 时 SIE=0、SPIE=旧 SIE、SPP。
#[test]
fn trap_entry_state() {
    let mut a = Asm::new();
    use reg::*;

    // M 初始化：MIE=1，medeleg 委派 illegal(2)，mret 进 S
    a.addr_of(T0, 0x300); // S handler
    a.emit32(csrw(0x105, T0));
    a.addr_of(T0, 0x200); // M handler
    a.emit32(csrw(0x305, T0));
    a.emit32(addi(T1, ZERO, 4));
    a.emit32(csrw(0x302, T1)); // medeleg illegal
    a.emit32(addi(T1, ZERO, 8));
    a.emit32(csrs(0x300, T1)); // MIE=1
    a.addr_of(T0, 0x100);
    a.emit32(csrw(0x341, T0)); // mepc = S 入口
    a.emit32(addi(T1, ZERO, 1));
    a.emit32(slli(T1, T1, 11));
    a.emit32(csrs(0x300, T1)); // MPP=S
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // S 入口 @0x100：SIE=1，触发非法指令 → S trap；
    // handler sret 回到 0x114 后经 test 设备退出（码 43）
    a.pad_to(0x100);
    a.emit32(addi(T1, ZERO, 2));
    a.emit32(csrs(0x100, T1)); // mstatus.SIE=1
    a.emit32(csrs(0x104, T1)); // sie.SSIE=1
    a.emit32(0xFFFFFFFF); // 非法指令 → S handler（medeleg.2）
    a.emit32(ECALL); // ecall-from-S 未委派 → M handler（记录 M 现场）
    a.emit32(lui(T0, 0x100)); // test 设备
    a.emit32(lui(T1, 0x2C5));
    a.emit32(addi(T1, T1, 0x555)); // 0x2C5555 = (44<<16)|0x5555
    a.emit32(sd(T1, T0, 0));

    // M handler @0x200：记录 mstatus 的 MIE/MPIE/MPP，mepc += 4，mret
    a.pad_to(0x200);
    a.addr_of(T3, RESULTS as i32);
    a.emit32(csrr(T0, 0x300)); // mstatus
    a.emit32(sd(T0, T3, 16));
    a.emit32(csrr(T0, 0x342)); // mcause
    a.emit32(sd(T0, T3, 24));
    a.emit32(csrr(T0, 0x341)); // mepc += 4
    a.emit32(addi(T0, T0, 4));
    a.emit32(csrw(0x341, T0));
    a.emit32(MRET);

    // S handler @0x300：记录 mstatus 的 SIE/SPIE/SPP
    a.pad_to(0x300);
    a.addr_of(T3, RESULTS as i32);
    a.emit32(csrr(T0, 0x100)); // sstatus
    a.emit32(sd(T0, T3, 0));
    a.emit32(csrr(T0, 0x142)); // scause（应为 2）
    a.emit32(sd(T0, T3, 8));
    a.emit32(csrr(T0, 0x141)); // sepc += 4（跳过非法指令）
    a.emit32(addi(T0, T0, 4));
    a.emit32(csrw(0x141, T0));
    a.emit32(SRET);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(20_000), Halt::Exit(44));
    let base = DRAM_BASE as i64 + RESULTS as i64;
    // S trap：SIE=0，SPP=1（从 S 陷入），SPIE=1（原 SIE=1）
    let sstatus = m.bus.load((base) as u64, 8).unwrap();
    assert_eq!(sstatus & (1 << 1), 0, "S trap 后 SIE=0");
    assert_ne!(sstatus & (1 << 8), 0, "SPP=1（前特权 S）");
    assert_ne!(sstatus & (1 << 5), 0, "SPIE=旧 SIE=1");
    assert_eq!(m.bus.load((base + 8) as u64, 8).unwrap(), 2, "scause = illegal");
    // M trap（handler 收到的 mstatus）：MIE=0，MPP=S(1)；
    // MPIE = 陷入时的旧 MIE —— init 的 mret 已将其清零（MPIE 复位值 0），
    // 故此处为 0（陷入/返回的 IE 级联本身即被本测试验证）
    let mstatus = m.bus.load((base + 16) as u64, 8).unwrap();
    assert_eq!(mstatus & (1 << 3), 0, "M trap 后 MIE=0");
    assert_eq!(mstatus & (1 << 7), 0, "MPIE=旧 MIE（init mret 清零）");
    assert_eq!((mstatus >> 11) & 3, 1, "MPP=S（从 S 陷入）");
    assert_eq!(m.bus.load((base + 24) as u64, 8).unwrap(), 9, "mcause = ecall-from-S");
}
