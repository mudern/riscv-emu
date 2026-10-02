//! 规范一致性回归测试（每项对应一次规范审查发现的偏差）。
//!
//! 1. fmadd 的 rm 字段在 funct3（曾误取 funct7&7，混入 rs3 低位）+ DYN 生效
//! 2. mret 仅 M 态可执行
//! 3. EBREAK 的 mtval = 0（曾误填 3）
//! 4. 失败的 SC 也作废预约
//! 5. satp WARL（mode 仅 0/8，PPN 44 位）
//! 6. mepc[0] 恒 0
//! 7. misa 可写（清 F 位后浮点指令 illegal，恢复后可执行）
//! 8. mret 到 M 保留 MPRV
//! 9. FS dirty 语义：只读 FP 指令（fclass）不置 Dirty

mod common;

use common::*;
use riscv_emu::machine::Halt;

const RESULTS: u64 = 0x800;
const SCRATCH: u64 = 0x900;

// ---- FP / AMO / CSR 编码器 ----
fn fld(rd: u32, rs1: u32, imm: i32) -> u32 {
    let i = (imm as u32) & 0xFFF;
    (i << 20) | (rs1 << 15) | (0x3 << 12) | (rd << 7) | 0x07
}
fn fsd(rs2: u32, rs1: u32, imm: i32) -> u32 {
    let i = (imm as u32) & 0xFFF;
    (((i >> 5) & 0x7F) << 25) | (rs2 << 20) | (rs1 << 15) | (0x3 << 12) | ((i & 0x1F) << 7) | 0x27
}
fn fmadd_s(rd: u32, rs1: u32, rs2: u32, rs3: u32, rm: u32) -> u32 {
    (rs3 << 27) | (rs2 << 20) | (rs1 << 15) | (rm << 12) | (rd << 7) | 0x43
}
fn fadd_s(rd: u32, rs1: u32, rs2: u32) -> u32 {
    (rs2 << 20) | (rs1 << 15) | (rd << 7) | 0x53
}
fn fclass_s(rd: u32, rs1: u32) -> u32 {
    (0x70 << 25) | (rs1 << 15) | (1 << 12) | (rd << 7) | 0x53
}
fn fmv_w_x(rd: u32, rs1: u32) -> u32 {
    (0x78 << 25) | (rs1 << 15) | (rd << 7) | 0x53
}
fn lr_w(rd: u32, rs1: u32) -> u32 {
    (0x02 << 27) | (rs1 << 15) | (0x02 << 12) | (rd << 7) | 0x2F
}
fn sc_w(rd: u32, rs2: u32, rs1: u32) -> u32 {
    (0x03 << 27) | (rs2 << 20) | (rs1 << 15) | (0x02 << 12) | (rd << 7) | 0x2F
}
fn csrrc(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (3 << 12) | (rd << 7) | 0x73
}
fn csrrs(rd: u32, csr: u32, rs1: u32) -> u32 {
    (csr << 20) | (rs1 << 15) | (2 << 12) | (rd << 7) | 0x73
}

/// M 态 trap handler：记录 mcause→[0]、mtval→[8]，mepc += skip，mret 返回
fn emit_m_handler_mret(a: &mut Asm, off: usize, skip: i32) {
    use reg::*;
    a.pad_to(off);
    a.addr_of(T3, RESULTS as i32);
    a.emit32(csrr(T0, 0x342)); // mcause
    a.emit32(sd(T0, T3, 0));
    a.emit32(csrr(T0, 0x343)); // mtval
    a.emit32(sd(T0, T3, 8));
    a.emit32(csrr(T0, 0x341)); // mepc
    a.emit32(addi(T0, T0, skip));
    a.emit32(csrw(0x341, T0));
    a.emit32(MRET);
}

#[test]
fn fmadd_rm_in_funct3_and_dyn() {
    let mut a = Asm::new();
    use reg::*;

    // FS = Initial，frm = RNE(0)
    a.emit32(addi(T0, ZERO, 1));
    a.emit32(slli(T0, T0, 13));
    a.emit32(csrs(0x300, T0));
    a.emit32(csrw(0x002, ZERO));

    // 常量（NaN-boxed 存内存）：f1=f2=1+2^-12 (0x3F80_0800)，f3=2^-11 (0x3A00_0000)
    // fmadd 用 rs3=f3（奇数）：旧 bug 把 rm 解成 funct7&7=(3<<2|0)&7 = RMM
    // 精确值 = (1+2^-12)^2 + 2^-11 = 1 + 2^-10 + 2^-24 → f32 平局
    // {1+2^-10, 1+2^-10+2^-23}
    a.addr_of(T2, SCRATCH as i32);
    a.emit32(lui(T1, 0x3F801));
    a.emit32(addi(T1, T1, -0x800)); // 0x3F801000-0x800 = 1 + 2^-12
    a.emit32(sw(T1, T2, 0));
    a.emit32(addi(T1, ZERO, -1));
    a.emit32(sw(T1, T2, 4));
    a.emit32(lui(T1, 0x3A000));
    a.emit32(sw(T1, T2, 8));
    a.emit32(addi(T1, ZERO, -1));
    a.emit32(sw(T1, T2, 12));
    a.emit32(fld(1, T2, 0));
    a.emit32(fld(2, T2, 0));
    a.emit32(fld(3, T2, 8));

    // fmadd.s f0, f1, f2, f3, rm=DYN(7)：精确值 1+2^-24（25 位 → 平局）
    // frm=RNE → 1.0 = 0x3F80_0000；旧 bug（rm 误取 RMM）→ 1+2^-23
    a.emit32(fmadd_s(0, 1, 2, 3, 7));
    a.emit32(fsd(0, T2, 16));
    // frm = RMM(4) → DYN 应读取 frm → 平局远离零 → 1+2^-23 = 0x3F80_0001
    a.emit32(addi(T0, ZERO, 4));
    a.emit32(csrw(0x002, T0));
    a.emit32(fmadd_s(0, 1, 2, 3, 7));
    a.emit32(fsd(0, T2, 24));
    a.emit32(addi(A0, ZERO, 31));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(31));
    eprintln!("[dbg] mem = {:#x} {:#x} {:#x} {:#x}",
        m.bus.load(DRAM_BASE + SCRATCH, 8).unwrap(),
        m.bus.load(DRAM_BASE + SCRATCH + 8, 8).unwrap(),
        m.bus.load(DRAM_BASE + SCRATCH + 16, 8).unwrap(),
        m.bus.load(DRAM_BASE + SCRATCH + 24, 8).unwrap());
    eprintln!("[dbg] f0={:016x} f1={:016x} f2={:016x} f3={:016x} frm={:#x}",
        m.cpu.fregs[0], m.cpu.fregs[1], m.cpu.fregs[2], m.cpu.fregs[3],
        m.cpu.csr.fcsr >> 5);
    let rne = m.bus.load(DRAM_BASE + SCRATCH + 16, 8).unwrap() as u32;
    let rmm = m.bus.load(DRAM_BASE + SCRATCH + 24, 8).unwrap() as u32;
    assert_eq!(
        rne, 0x3F80_2000,
        "DYN+frm=RNE → 平局取偶（rs3 奇数时旧 bug 误用 RMM 给出远离零值）"
    );
    assert_eq!(rmm, 0x3F80_2001, "DYN+frm=RMM → 平局远离零");
}

#[test]
fn mret_is_illegal_outside_m_mode() {
    let mut a = Asm::new();
    use reg::*;

    // M 初始化（必须在偏移 0）：mtvec/stvec，mepc=S 入口，MPP=S，PMP，mret 进 S
    a.addr_of(T0, 0x200);
    a.emit32(csrw(0x305, T0));
    a.addr_of(T0, 0x280);
    a.emit32(csrw(0x105, T0));
    a.addr_of(T0, 0x100);
    a.emit32(csrw(0x341, T0));
    a.emit32(addi(T1, ZERO, 1));
    a.emit32(slli(T1, T1, 11));
    a.emit32(csrs(0x300, T1));
    // 委托 illegal instruction（cause 2）到 S 态
    a.emit32(addi(T1, ZERO, 4));
    a.emit32(csrw(0x302, T1));
    pmp_open_all(&mut a);
    a.emit32(MRET);

    // S 入口 @0x100：直接执行 mret（M 态专用指令）→ S 态 illegal
    a.pad_to(0x100);
    a.emit32(MRET);
    a.emit32(addi(A0, ZERO, 2));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    // M handler（不该触发）与 S handler（记录 scause 后经 test 设备退出码 8）
    emit_m_handler_mret(&mut a, 0x200, 4);
    a.pad_to(0x280);
    a.addr_of(T3, RESULTS as i32);
    a.emit32(csrr(T0, 0x142)); // scause
    a.emit32(sd(T0, T3, 0));
    a.emit32(lui(T0, 0x100)); // test 设备
    a.emit32(lui(T1, 0x86)); // (8<<16)|0x5555 = 0x86000 - 0xAAB
    a.emit32(addi(T1, T1, -0x800));
    a.emit32(addi(T1, T1, -0x2AB));
    a.emit32(sd(T1, T0, 0));

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(8));
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS, 8).unwrap(),
        2,
        "S 态执行 mret 应产生 illegal instruction"
    );
}

#[test]
fn ebreak_mtval_is_zero() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, 0x200);
    a.emit32(csrw(0x305, T0));
    // c.ebreak（2 字节）→ handler 记录 mtval，mepc += 2，mret
    a.emit16(0x9002);
    a.addr_of(T3, RESULTS as i32);
    a.emit32(ld(T0, T3, 8));
    a.emit32(addi(A0, ZERO, 32));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);
    a.pad_to(0x200);
    a.addr_of(T3, RESULTS as i32);
    a.emit32(csrr(T0, 0x343));
    a.emit32(sd(T0, T3, 8));
    a.emit32(csrr(T0, 0x341));
    a.emit32(addi(T0, T0, 2));
    a.emit32(csrw(0x341, T0));
    a.emit32(MRET);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(32));
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS + 8, 8).unwrap(),
        0,
        "EBREAK 的 mtval 应为 0（对齐 QEMU）"
    );
}

#[test]
fn failed_sc_clears_reservation() {
    let mut a = Asm::new();
    use reg::*;

    // a1 = DRAM_BASE + 0x800（地址 A），a5 = DRAM_BASE + 0x900（地址 B）
    a.emit32(addi(10, ZERO, 1));
    a.emit32(slli(10, 10, 31));
    a.emit32(addi(11, 10, 0x7F0));
    a.emit32(addi(11, 11, 0x10)); // a1 = +0x800
    a.emit32(addi(15, 10, 0x7F0));
    a.emit32(addi(15, 15, 0x110)); // a5 = +0x900
    // lr.w a0, (A) → 预约 A
    a.emit32(lr_w(10, 11));
    // sc.w a2, x0, (B) → 失败 a2=1，且预约作废
    a.emit32(sc_w(12, ZERO, 15));
    // sc.w a3, x0, (A) → 必须仍失败 a3=1（若预约未被清则会成功 a3=0）
    a.emit32(sc_w(13, ZERO, 11));
    a.addr_of(T3, RESULTS as i32);
    a.emit32(sd(12, T3, 0));
    a.emit32(sd(13, T3, 8));
    a.emit32(addi(A0, ZERO, 33));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(33));
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS, 8).unwrap(), 1, "B 上的 SC 失败");
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS + 8, 8).unwrap(),
        1,
        "失败的 SC 必须作废预约：随后的 SC(A) 仍失败"
    );
}

#[test]
fn satp_warl_and_mepc_mask() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T3, RESULTS as i32);
    // satp = 全 1 → mode 0xF→0，PPN 截 44 位 = 0xF_FFFF_FFFF
    a.emit32(addi(T0, ZERO, -1));
    a.emit32(csrw(0x180, T0));
    a.emit32(csrr(T1, 0x180));
    a.emit32(sd(T1, T3, 0));
    // satp = 8<<60 → Sv39 保留
    a.emit32(addi(T0, ZERO, 8));
    a.emit32(slli(T0, T0, 60));
    a.emit32(csrw(0x180, T0));
    a.emit32(csrr(T1, 0x180));
    a.emit32(sd(T1, T3, 8));
    // mepc = 5 → 位 0 清零 → 4
    a.emit32(addi(T0, ZERO, 5));
    a.emit32(csrw(0x341, T0));
    a.emit32(csrr(T1, 0x341));
    a.emit32(sd(T1, T3, 16));
    a.emit32(addi(A0, ZERO, 34));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(34));
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS, 8).unwrap(),
        0x0000_000F_FFFF_FFFF,
        "satp mode WARL=0，PPN 截 44 位"
    );
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS + 8, 8).unwrap(),
        8 << 60,
        "satp mode=Sv39 保留"
    );
    assert_eq!(m.bus.load(DRAM_BASE + RESULTS + 16, 8).unwrap(), 4, "mepc[0] 恒 0");
}

#[test]
fn misa_write_gates_fp() {
    let mut a = Asm::new();
    use reg::*;

    a.addr_of(T0, 0x200);
    a.emit32(csrw(0x305, T0));
    // FS = Dirty（保证浮点非法只可能来自 misa.F 缺失）
    a.emit32(addi(T0, ZERO, 3));
    a.emit32(slli(T0, T0, 13));
    a.emit32(csrs(0x300, T0));

    // 清 misa.F（CSRRC 1<<5）
    a.emit32(addi(T0, ZERO, 32));
    a.emit32(csrrc(ZERO, 0x301, T0));
    // fmv.w.x f0, x0 → 应 illegal（misa.F 已关）
    a.emit32(fmv_w_x(0, ZERO));
    // 恢复 misa.F（CSRS）
    a.emit32(csrrs(ZERO, 0x301, T0));
    // 再执行 → 合法，无 trap
    a.emit32(fmv_w_x(0, ZERO));
    a.emit32(addi(A0, ZERO, 35));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);
    // handler：记录 mcause，mepc += 4，mret
    emit_m_handler_mret(&mut a, 0x200, 4);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(35));
    assert_eq!(
        m.bus.load(DRAM_BASE + RESULTS, 8).unwrap(),
        2,
        "清 misa.F 后恰一次 illegal（恢复后浮点可执行、无再陷入）"
    );
}

#[test]
fn mret_to_m_retains_mprv() {
    let mut a = Asm::new();
    use reg::*;

    // mepc = 0x100（mret 目标），MPRV=1，MPP=M，mret → 仍在 M，MPRV 保留
    a.addr_of(T0, 0x100);
    a.emit32(csrw(0x341, T0));
    a.emit32(addi(T0, ZERO, 1));
    a.emit32(slli(T0, T0, 17)); // MPRV
    a.emit32(csrs(0x300, T0));
    a.emit32(addi(T0, ZERO, 3));
    a.emit32(slli(T0, T0, 11)); // MPP=M
    a.emit32(csrs(0x300, T0));
    pmp_open_all(&mut a); // mret 后 MPRV=1&MPP=U 的访存按 U 查 PMP
    a.emit32(MRET);
    a.pad_to(0x100);
    a.emit32(csrr(T1, 0x300));
    a.addr_of(T3, RESULTS as i32);
    a.emit32(sd(T1, T3, 0));
    a.emit32(addi(A0, ZERO, 36));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(36));
    let mstatus = m.bus.load(DRAM_BASE + RESULTS, 8).unwrap();
    assert_ne!(mstatus & (1 << 17), 0, "mret 到 M 不得清 MPRV");
}

#[test]
fn fs_dirty_semantics() {
    let mut a = Asm::new();
    use reg::*;

    // FS = Clean（2<<13）
    a.emit32(addi(T0, ZERO, 2));
    a.emit32(slli(T0, T0, 13));
    a.emit32(csrw(0x300, T0));
    // fclass.s f0, f0（只读 FP 状态）→ FS 应保持 Clean
    a.emit32(fclass_s(0, 0));
    a.emit32(csrr(T1, 0x300));
    a.addr_of(T3, RESULTS as i32);
    a.emit32(sd(T1, T3, 0));
    // fadd.s f0, f0, f0（写 f 寄存器）→ FS = Dirty
    a.emit32(fadd_s(0, 0, 0));
    a.emit32(csrr(T1, 0x300));
    a.emit32(sd(T1, T3, 8));
    a.emit32(addi(A0, ZERO, 37));
    a.emit32(addi(A7, ZERO, 93));
    a.emit32(ECALL);

    let mut m = a.into_machine(16);
    assert_eq!(m.run(10_000), Halt::Exit(37));
    let after_class = m.bus.load(DRAM_BASE + RESULTS, 8).unwrap();
    let after_add = m.bus.load(DRAM_BASE + RESULTS + 8, 8).unwrap();
    assert_eq!((after_class >> 13) & 3, 2, "只读 FP 指令不得置 Dirty");
    assert_eq!((after_add >> 13) & 3, 3, "写 FP 状态的指令置 Dirty");
}
