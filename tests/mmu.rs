//! Sv39 地址翻译单元测试：直接构造页表调用 mmu::translate。

mod common;

use common::DRAM_BASE;
use riscv_emu::exception::Exception;
use riscv_emu::mmu::{self, Access, PTE_A, PTE_D, PTE_R, PTE_U, PTE_V, PTE_W, PTE_X};
use riscv_emu::{Bus, Privilege};

// 页表物理地址（RAM 内，4KB 对齐）
const L2: u64 = DRAM_BASE + 0x1000; // 根表
const L1_S: u64 = DRAM_BASE + 0x2000; // 二级表
const L0_U: u64 = DRAM_BASE + 0x4000; // 三级表（U 4KB 页）

// 映射计划：
//   VA 0x4000_0000 ..0x401F_FFFF → 2MB 大页 @ phys 0x100_0000（S 专用 RW）
//   VA 0x4020_0000              → 未对齐大页（应 fault）
//   VA 0x4040_0000              → 4KB @ phys 0x200_0000（U RW）
//   VA 0x4040_1000              → 4KB @ phys 0x200_1000（U 只读）
//   VA 0x4040_2000              → 4KB @ phys 0x200_2000（U 仅执行）

fn pte(ppn: u64, flags: u64) -> u64 {
    (ppn << 10) | flags
}

fn setup_tables() -> Bus {
    let mut bus = Bus::new(16 * 1024 * 1024);
    let w = |bus: &mut Bus, addr: u64, v: u64| bus.store(addr, 8, v).unwrap();

    // 根表：VA[38:30]=1 → L1_S / L1_U 的选择在两棵子树间共享：
    // 大页分支放在 L1_S[0]，U 分支放 L1_S[2] → L0_U
    w(&mut bus, L2, pte(L2 >> 12, 0)); // 占位
    w(&mut bus, L2 + 8, pte(L1_S >> 12, PTE_V)); // 指向 L1_S

    // L1_S[0]：2MB 大页（ppn=0x1000，2MB 对齐），S 页 RW
    w(&mut bus, L1_S, pte(0x1000, PTE_V | PTE_R | PTE_W | PTE_A | PTE_D));
    // L1_S[1]：未对齐大页（ppn 低 9 位非 0）
    w(&mut bus, L1_S + 8, pte(0x1001, PTE_V | PTE_R | PTE_W));
    // L1_S[2]：指向 L0_U
    w(&mut bus, L1_S + 2 * 8, pte(L0_U >> 12, PTE_V));

    // L0_U：VPN[11:0] = 0/1/2
    w(&mut bus, L0_U, pte(0x2000, PTE_V | PTE_R | PTE_W | PTE_U | PTE_A | PTE_D));
    w(&mut bus, L0_U + 8, pte(0x2001, PTE_V | PTE_R | PTE_U | PTE_A));
    w(&mut bus, L0_U + 2 * 8, pte(0x2002, PTE_V | PTE_X | PTE_U | PTE_A));

    bus
}

fn satp() -> u64 {
    (mmu::SATP_SV39 << 60) | (L2 >> 12)
}

fn tr(bus: &mut Bus, va: u64, acc: Access, priv_: Privilege, mxr: bool, sum: bool) -> Result<u64, Exception> {
    mmu::translate(bus, satp(), priv_, mxr, sum, va, acc)
}

#[test]
fn superpage_translation() {
    let mut bus = setup_tables();
    // S 态读写大页
    assert_eq!(
        tr(&mut bus, 0x4000_0000, Access::Load, Privilege::S, false, false),
        Ok(0x100_0000)
    );
    assert_eq!(
        tr(&mut bus, 0x4001_2345, Access::Store, Privilege::S, false, false),
        Ok(0x101_2345)
    );
}

#[test]
fn u_page_permissions() {
    let mut bus = setup_tables();
    // U 读写自己的页
    assert_eq!(
        tr(&mut bus, 0x4040_0000, Access::Load, Privilege::U, false, false),
        Ok(0x200_0000)
    );
    assert_eq!(
        tr(&mut bus, 0x4040_0000, Access::Store, Privilege::U, false, false),
        Ok(0x200_0000)
    );
    // U 写只读页
    assert_eq!(
        tr(&mut bus, 0x4040_1000, Access::Store, Privilege::U, false, false),
        Err(Exception::StorePageFault)
    );
    // U 读只读页 OK
    assert_eq!(
        tr(&mut bus, 0x4040_1000, Access::Load, Privilege::U, false, false),
        Ok(0x200_1000)
    );
    // U 访问 S 页（大页）→ fault
    assert_eq!(
        tr(&mut bus, 0x4000_0000, Access::Load, Privilege::U, false, false),
        Err(Exception::LoadPageFault)
    );
}

#[test]
fn execute_only_and_mxr() {
    let mut bus = setup_tables();
    // 取指允许
    assert_eq!(
        tr(&mut bus, 0x4040_2000, Access::Fetch, Privilege::U, false, false),
        Ok(0x200_2000)
    );
    // 无 MXR 时读执行页 → fault；有 MXR → OK
    assert_eq!(
        tr(&mut bus, 0x4040_2000, Access::Load, Privilege::U, false, false),
        Err(Exception::LoadPageFault)
    );
    assert_eq!(
        tr(&mut bus, 0x4040_2000, Access::Load, Privilege::U, true, false),
        Ok(0x200_2000)
    );
}

#[test]
fn sum_bit() {
    let mut bus = setup_tables();
    // S 态默认不能访问 U 页
    assert_eq!(
        tr(&mut bus, 0x4040_0000, Access::Load, Privilege::S, false, false),
        Err(Exception::LoadPageFault)
    );
    // SUM=1 时可以
    assert_eq!(
        tr(&mut bus, 0x4040_0000, Access::Load, Privilege::S, false, true),
        Ok(0x200_0000)
    );
    // 但取指 U 页即使 SUM 也不行
    assert_eq!(
        tr(&mut bus, 0x4040_0000, Access::Fetch, Privilege::S, false, true),
        Err(Exception::InstructionPageFault)
    );
}

#[test]
fn fault_cases() {
    let mut bus = setup_tables();
    // 未映射 VA
    assert_eq!(
        tr(&mut bus, 0x5000_0000, Access::Load, Privilege::S, false, false),
        Err(Exception::LoadPageFault)
    );
    // 未对齐大页
    assert_eq!(
        tr(&mut bus, 0x4020_0000, Access::Load, Privilege::S, false, false),
        Err(Exception::LoadPageFault)
    );
    // 非规范地址（bit63:39 不是 bit38 的符号扩展）
    assert_eq!(
        tr(&mut bus, 1 << 45, Access::Load, Privilege::S, false, false),
        Err(Exception::LoadPageFault)
    );
    // S 写只读大页？大页是 RW，改为写 X-only U 页无效——S 页本身 RW，改为读 S 页 fetch（无 X 位）
    assert_eq!(
        tr(&mut bus, 0x4000_0000, Access::Fetch, Privilege::S, false, false),
        Err(Exception::InstructionPageFault)
    );
}

#[test]
fn a_d_bits_set_on_access() {
    let mut bus = setup_tables();
    // 构造一个 A=0 的可写 U 页：替换 L0_U[3]
    bus.store(L0_U + 3 * 8, 8, pte(0x2003, PTE_V | PTE_R | PTE_W | PTE_U))
        .unwrap();
    assert_eq!(
        tr(&mut bus, 0x4040_3000, Access::Store, Privilege::U, false, false),
        Ok(0x200_3000)
    );
    let pte_val = bus.load(L0_U + 3 * 8, 8).unwrap();
    assert_eq!(pte_val & (PTE_A | PTE_D), PTE_A | PTE_D);
}
