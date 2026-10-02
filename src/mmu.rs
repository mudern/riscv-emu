//! Sv39 地址翻译。`translate` 为无缓存走表（测试/直接调用）；
//! `Mmu` 是带直接映射 TLB 的包装（CPU 使用，satp 写与 sfence.vma 时冲刷）。
//! A/D 位自动置位（与 QEMU 默认行为一致）。

use crate::bus::Bus;
use crate::cpu::Privilege;
use crate::exception::Exception;

pub const PTE_V: u64 = 1 << 0;
pub const PTE_R: u64 = 1 << 1;
pub const PTE_W: u64 = 1 << 2;
pub const PTE_X: u64 = 1 << 3;
pub const PTE_U: u64 = 1 << 4;
pub const PTE_G: u64 = 1 << 5;
pub const PTE_A: u64 = 1 << 6;
pub const PTE_D: u64 = 1 << 7;

pub const SATP_SV39: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Fetch,
    Load,
    Store,
}

fn page_fault(access: Access) -> Exception {
    match access {
        Access::Fetch => Exception::InstructionPageFault,
        Access::Load => Exception::LoadPageFault,
        Access::Store => Exception::StorePageFault,
    }
}

/// 规范地址检查：vaddr[63:39] 必须是 vaddr[38] 的符号扩展
fn check_canonical(vaddr: u64, acc: Access) -> Result<(), Exception> {
    let sign = ((vaddr as i64) >> 38) as u64 & 0x1F_FFFF_FFFF;
    if sign != 0 && sign != 0x1F_FFFF_FFFF {
        return Err(page_fault(acc));
    }
    Ok(())
}

fn check_perm(
    flags: u64,
    access: Access,
    mode: Privilege,
    mxr: bool,
    sum: bool,
) -> Result<(), Exception> {
    let ok = match access {
        Access::Fetch => flags & PTE_X != 0,
        Access::Load => flags & PTE_R != 0 || (mxr && flags & PTE_X != 0),
        Access::Store => flags & PTE_W != 0,
    };
    if !ok {
        return Err(page_fault(access));
    }
    let user_page = flags & PTE_U != 0;
    if mode == Privilege::S && user_page && !sum {
        return Err(page_fault(access));
    }
    if mode == Privilege::U && !user_page {
        return Err(page_fault(access));
    }
    Ok(())
}

/// 走表 PMP 拒绝时抛出的异常：按原始访问类型的 access fault
/// （规范 3.7.2：PTE 读取被 PMP 拒绝时报原始访问的 access fault）
fn access_fault(acc: Access) -> Exception {
    match acc {
        Access::Fetch => Exception::InstructionAccessFault,
        Access::Load => Exception::LoadAccessFault,
        Access::Store => Exception::StoreAccessFault,
    }
}

/// 无缓存 Sv39 翻译。Bare 模式（satp.mode != Sv39）恒等映射。
/// `pmp`：页表访问的 PMP 检查（有效特权级 S，规范 3.7.2）。
#[allow(clippy::too_many_arguments)] // 参数即翻译上下文
pub fn translate(
    bus: &mut Bus,
    satp: u64,
    mode: Privilege,
    mxr: bool,
    sum: bool,
    vaddr: u64,
    acc: Access,
    pmp: &crate::pmp::Pmp,
) -> Result<u64, Exception> {
    if satp >> 60 != SATP_SV39 {
        return Ok(vaddr);
    }
    check_canonical(vaddr, acc)?;
    walk(bus, satp, mode, mxr, sum, vaddr, acc, pmp).map(|(pa, _)| pa)
}

/// 三级走表：返回 (物理地址, TLB 回填项)。叶子页表项 A/D 自动置位。
#[allow(clippy::too_many_arguments)] // 参数即翻译上下文
fn walk(
    bus: &mut Bus,
    satp: u64,
    mode: Privilege,
    mxr: bool,
    sum: bool,
    vaddr: u64,
    acc: Access,
    pmp: &crate::pmp::Pmp,
) -> Result<(u64, TlbEntry), Exception> {
    let fault = page_fault(acc);
    let afault = access_fault(acc);
    // 规范 3.7.2：页表访问的 PMP 有效特权级为 S
    let pmp_ok = |pa: u64, acc: crate::pmp::PmpAccess| {
        pmp.allows(pa, 8, acc, Privilege::S)
    };
    let vpn = [
        vaddr >> 12 & 0x1FF,
        vaddr >> 21 & 0x1FF,
        vaddr >> 30 & 0x1FF,
    ];
    let mut ppn = satp & 0xFFF_FFFF_FFFF;

    for level in (0..3).rev() {
        let pte_addr = (ppn << 12) + vpn[level] * 8; // ppn*4096 + idx*8
        // 隐式页表读取：PMP 拒绝 → 原始访问类型的 access fault（非 page fault）
        if !pmp_ok(pte_addr, crate::pmp::PmpAccess::Read) {
            return Err(afault);
        }
        let pte = bus.load(pte_addr, 8).map_err(|_| afault)?;
        if pte & PTE_V == 0 {
            return Err(fault);
        }
        if pte & PTE_W != 0 && pte & PTE_R == 0 {
            return Err(fault); // W 而无 R：保留编码
        }
        // pte[63:54] 保留（Sv39 PPN 仅 44 位；我们也不实现 Svnapot/PBMT）
        if pte >> 54 != 0 {
            return Err(fault);
        }
        if pte & (PTE_R | PTE_W | PTE_X) != 0 {
            // 叶子
            if level > 0 {
                // 大页：低位 ppn 必须为 0（对齐检查）
                if (pte >> 10) & ((1 << (level * 9)) - 1) != 0 {
                    return Err(fault);
                }
            }
            check_perm(pte, acc, mode, mxr, sum)?;
            // A/D 位自动置位（写回）
            let need = PTE_A | if acc == Access::Store { PTE_D } else { 0 };
            let new_pte = if pte & need != need {
                // 隐式 PTE 写（A/D 置位）同样受 PMP 检查
                if !pmp_ok(pte_addr, crate::pmp::PmpAccess::Write) {
                    return Err(afault);
                }
                let p = pte | need;
                let _ = bus.store(pte_addr, 8, p);
                p
            } else {
                pte
            };
            let sub = (vaddr >> 12) & ((1u64 << (level * 9)) - 1);
            let page_ppn = (new_pte >> 10) + sub;
            let entry = TlbEntry {
                vpn: vaddr >> 12,
                ppn: page_ppn,
                flags: new_pte & 0xFF,
            };
            return Ok(((page_ppn << 12) | (vaddr & 0xFFF), entry));
        }
        // 非叶子：D/A/U 位保留（QEMU 同款检查），PPN 直接作为下一级基址
        if pte & (PTE_D | PTE_A | PTE_U) != 0 {
            return Err(fault);
        }
        ppn = pte >> 10;
    }
    Err(fault) // 走到 level 0 仍是非叶子
}

#[derive(Debug, Clone, Copy)]
struct TlbEntry {
    vpn: u64,
    ppn: u64,
    flags: u64,
}

const TLB_SIZE: usize = 1024;

/// 带直接映射 TLB 的翻译器。命中时用缓存的 PTE 标志位重新做权限检查
/// （SUM/MXR/特权级按当前状态评估，因此模式切换不会用到陈旧权限）。
pub struct Mmu {
    tlb: Vec<Option<TlbEntry>>,
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmu {
    pub fn new() -> Self {
        Mmu {
            tlb: vec![None; TLB_SIZE],
        }
    }

    pub fn flush(&mut self) {
        self.tlb.fill(None);
    }

    #[allow(clippy::too_many_arguments)] // 参数即翻译上下文，拆结构体反而绕
    pub fn translate(
        &mut self,
        bus: &mut Bus,
        satp: u64,
        mode: Privilege,
        mxr: bool,
        sum: bool,
        vaddr: u64,
        acc: Access,
        pmp: &crate::pmp::Pmp,
    ) -> Result<u64, Exception> {
        if satp >> 60 != SATP_SV39 {
            return Ok(vaddr);
        }
        check_canonical(vaddr, acc)?;
        let vpn = vaddr >> 12;
        let key = (vpn as usize) & (TLB_SIZE - 1);
        if let Some(e) = self.tlb[key]
            && e.vpn == vpn
        {
            check_perm(e.flags, acc, mode, mxr, sum)?;
            return Ok((e.ppn << 12) | (vaddr & 0xFFF));
        }
        let (pa, entry) = walk(bus, satp, mode, mxr, sum, vaddr, acc, pmp)?;
        self.tlb[key] = Some(entry);
        Ok(pa)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Bus, DRAM_BASE};

    fn full_pmp() -> crate::pmp::Pmp {
        let mut p = crate::pmp::Pmp::default();
        p.write_cfg(0, crate::pmp::CFG_A_NAPOT | crate::pmp::CFG_R | crate::pmp::CFG_W | crate::pmp::CFG_X);
        p.write_addr(0, crate::pmp::PMPADDR_MASK);
        p
    }

    /// 允许一切的空 PMP（测试中走表访问都在 M 态之外，需全开）
    fn walk_pmp() -> crate::pmp::Pmp {
        full_pmp()
    }

    fn write_pte(bus: &mut Bus, addr: u64, v: u64) {
        bus.store(addr, 8, v).unwrap();
    }

    /// 三级表：root@base, l1@base+0x1000, l0@base+0x2000
    fn map(bus: &mut Bus, root: u64, va: u64, pa: u64, flags: u64) {
        let vpn = [va >> 12 & 0x1FF, va >> 21 & 0x1FF, va >> 30 & 0x1FF];
        let (l1, l0) = (root + 0x1000, root + 0x2000);
        write_pte(bus, root + vpn[2] * 8, ((l1 >> 12) << 10) | PTE_V);
        write_pte(bus, l1 + vpn[1] * 8, ((l0 >> 12) << 10) | PTE_V);
        write_pte(bus, l0 + vpn[0] * 8, (pa << 10) | PTE_V | flags);
    }

    fn sv39(root: u64) -> u64 {
        (SATP_SV39 << 60) | (root >> 12)
    }

    #[test]
    fn bare_identity() {
        let mut bus = Bus::new(1024 * 1024);
        bus.store(DRAM_BASE + 8, 4, 0xDEAD_BEEF).unwrap();
        assert_eq!(
            translate(
                &mut bus,
                0,
                Privilege::S,
                false,
                false,
                DRAM_BASE + 8,
                Access::Load,
                &walk_pmp()
            ),
            Ok(DRAM_BASE + 8)
        );
    }

    #[test]
    fn w_without_r_is_reserved() {
        let mut bus = Bus::new(1024 * 1024);
        let root = DRAM_BASE + 0x10_000;
        // L2[0] 直接放一个 W-only 叶子：保留编码 → fault
        write_pte(&mut bus, root, ((DRAM_BASE >> 12) << 10) | PTE_V | PTE_W);
        assert_eq!(
            translate(
                &mut bus,
                sv39(root),
                Privilege::S,
                false,
                false,
                0x0,
                Access::Load,
                &walk_pmp()
            ),
            Err(Exception::LoadPageFault)
        );
    }

    #[test]
    fn walk_pmp_deny_gives_access_fault() {
        // 规范 3.7.2：走表访问的 PMP 有效特权级为 S；PTE 读取被 PMP 拒绝时
        // 报原始访问类型的 access fault（而非 page fault）
        let mut bus = Bus::new(1024 * 1024);
        let root = DRAM_BASE + 0x10_000;
        write_pte(&mut bus, root, ((DRAM_BASE) >> 12 << 10) | PTE_V | PTE_R);
        let mut deny = crate::pmp::Pmp::default(); // 空 PMP：S 态全拒
        assert_eq!(
            translate(
                &mut bus,
                sv39(root),
                Privilege::S,
                false,
                false,
                0x0,
                Access::Load,
                &deny
            ),
            Err(Exception::LoadAccessFault),
            "PTE 读取 PMP 拒绝 → load access fault"
        );
        assert_eq!(
            translate(
                &mut bus,
                sv39(root),
                Privilege::S,
                false,
                false,
                0x0,
                Access::Fetch,
                &deny
            ),
            Err(Exception::InstructionAccessFault),
            "取指走表被拒 → instruction access fault"
        );
        // A/D 置位的隐式 PTE 写被 PMP 拒绝 → access fault
        let mut rw_no_more = crate::pmp::Pmp::default();
        rw_no_more.write_addr(0, (root >> 2) | 0x3FF);
        rw_no_more.write_cfg(0, crate::pmp::CFG_A_NAPOT | crate::pmp::CFG_R); // 只读
        // 重建允许读取的规则：读取需 R ✓（已给），A/D 写需 W ✗
        let mut bus2 = Bus::new(1024 * 1024);
        let root2 = DRAM_BASE + 0x10_000;
        write_pte(&mut bus2, root2, ((DRAM_BASE) >> 12 << 10) | PTE_V | PTE_R | PTE_W);
        assert_eq!(
            translate(
                &mut bus2,
                sv39(root2),
                Privilege::S,
                false,
                false,
                0x0,
                Access::Store,
                &rw_no_more
            ),
            Err(Exception::StoreAccessFault),
            "A/D 隐式写 PMP 拒绝 → store access fault"
        );
        let _ = (&mut deny, &mut rw_no_more);
    }

    #[test]
    fn reserved_pte_bits_fault() {
        let mut bus = Bus::new(1024 * 1024);
        let root = DRAM_BASE + 0x10_000;
        let va = 0x4000_0000u64;
        let vpn = [va >> 12 & 0x1FF, va >> 21 & 0x1FF, va >> 30 & 0x1FF];
        // 位 54（保留高区）置 1 的叶子 PTE → fault
        write_pte(&mut bus, root + vpn[2] * 8, ((root2() >> 12) << 10) | PTE_V);
        write_pte(&mut bus, root + 0x1000 + vpn[1] * 8, ((root2() >> 12) << 10) | PTE_V);
        write_pte(
            &mut bus,
            root + 0x2000 + vpn[0] * 8,
            (0x8000_2000 >> 10) | PTE_V | PTE_R | (1 << 54),
        );
        assert_eq!(
            translate(&mut bus, sv39(root), Privilege::S, false, false, va, Access::Load, &walk_pmp()),
            Err(Exception::LoadPageFault),
            "pte[63:54] 保留位必须为 0"
        );

        // 非叶子 PTE 带 D 位 → fault（QEMU 同款保留检查）
        write_pte(&mut bus, root + vpn[2] * 8, ((root2() >> 12) << 10) | PTE_V | PTE_D);
        assert_eq!(
            translate(&mut bus, sv39(root), Privilege::S, false, false, va, Access::Load, &walk_pmp()),
            Err(Exception::LoadPageFault),
            "非叶子 PTE 的 D/A/U 位保留"
        );
    }

    fn root2() -> u64 {
        DRAM_BASE + 0x10_000
    }

    #[test]
    fn tlb_hit_and_flush() {
        let mut bus = Bus::new(1024 * 1024);
        let root = DRAM_BASE + 0x10_000;
        let va = 0x4000_0000u64;
        map(&mut bus, root, va, 0x8000_2000 >> 12, PTE_R | PTE_W | PTE_X);
        let mut mmu = Mmu::new();
        let satp = sv39(root);

        assert_eq!(
            mmu.translate(&mut bus, satp, Privilege::S, false, false, va, Access::Load, &walk_pmp()),
            Ok(0x8000_2000)
        );
        // TLB 命中：删掉页表后仍可翻译
        write_pte(&mut bus, root + 8, 0);
        assert_eq!(
            mmu.translate(
                &mut bus,
                satp,
                Privilege::S,
                false,
                false,
                va,
                Access::Store,
                &walk_pmp()
            ),
            Ok(0x8000_2000)
        );
        // 冲刷后 fault
        mmu.flush();
        assert!(
            mmu.translate(&mut bus, satp, Privilege::S, false, false, va, Access::Load, &walk_pmp())
                .is_err()
        );
    }

    #[test]
    fn tlb_permission_reevaluated_on_hit() {
        let mut bus = Bus::new(1024 * 1024);
        let root = DRAM_BASE + 0x10_000;
        let va = 0x4000_0000u64;
        // U 页：SUM=1 时填充，SUM=0 时命中也应 fault
        map(&mut bus, root, va, 0x8000_2000 >> 12, PTE_R | PTE_U);
        let mut mmu = Mmu::new();
        let satp = sv39(root);
        assert!(
            mmu.translate(&mut bus, satp, Privilege::S, false, true, va, Access::Load, &walk_pmp())
                .is_ok()
        );
        assert_eq!(
            mmu.translate(&mut bus, satp, Privilege::S, false, false, va, Access::Load, &walk_pmp()),
            Err(Exception::LoadPageFault)
        );
    }
}
