//! PMP（物理内存保护）。
//!
//! 语义对齐特权规范 3.7（无 Smepmp/MML）：
//! - 16 项规则，G=0（粒度 4 字节，NA4 支持），PA 宽度 56 位（Sv39 PLEN）
//! - pmpaddr 实现 PA[55:2] → 54 位；位 [63:54] WARL 为零
//! - 匹配：**最低编号**且与访问**任意字节**重叠的条目即裁决；
//!   该条目必须覆盖全部字节，否则立即拒绝（不继续找下一条）
//! - L=0：M 态访问匹配即成功；L=1：R/W/X 对所有特权级生效
//! - 无匹配：M 态成功；S/U 态拒绝
//! - 锁定：L=1 锁 pmpcfgi/pmpaddri；L=1 且 A=TOR 时 pmpaddr[i-1] 一并锁定
//! - 写入规范化：保留位（bit6:5）读回零；R=0,W=1 保留组合 → W 强制零
//!   （A 字段 0/1/2/3 = OFF/TOR/NA4/NAPOT 全部合法，无保留编码）

use crate::cpu::Privilege;

pub const PMP_COUNT: usize = 16;

/// pmpcfg 字节字段
pub const CFG_L: u8 = 1 << 7; // 锁定
pub const CFG_A_MASK: u8 = 3 << 3;
pub const CFG_A_OFF: u8 = 0 << 3;
pub const CFG_A_TOR: u8 = 1 << 3;
pub const CFG_A_NA4: u8 = 2 << 3;
pub const CFG_A_NAPOT: u8 = 3 << 3;
pub const CFG_X: u8 = 1 << 2;
pub const CFG_W: u8 = 1 << 1;
pub const CFG_R: u8 = 1 << 0;

/// pmpcfg 有效位（bit 6:5 保留，读回零）
const CFG_MASK: u8 = CFG_L | CFG_A_MASK | CFG_X | CFG_W | CFG_R;
/// pmpaddr 实现位：PA 宽度 56 位（Sv39 PLEN）→ PA[55:2] 共 54 位
pub const PMPADDR_MASK: u64 = (1u64 << 54) - 1;
/// 物理地址宽度上限（Sv39 PLEN）
const PLEN: u64 = 1 << 56;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmpAccess {
    Read,
    Write,
    Exec,
}

#[derive(Default)]
pub struct Pmp {
    pub cfg: [u8; PMP_COUNT],
    pub addr: [u64; PMP_COUNT],
}

impl Pmp {
    /// 已配置（A != OFF）的规则数
    pub fn num_rules(&self) -> usize {
        self.cfg.iter().filter(|c| *c & CFG_A_MASK != 0).count()
    }

    /// 写 pmpaddr。忽略条件：本项 L=1；或**下一项** L=1 且 A=TOR
    /// （本项是锁定 TOR 项的下界，规范 3.7.1.2）
    pub fn write_addr(&mut self, idx: usize, val: u64) {
        if idx >= PMP_COUNT || self.cfg[idx] & CFG_L != 0 {
            return;
        }
        if idx + 1 < PMP_COUNT
            && self.cfg[idx + 1] & CFG_L != 0
            && self.cfg[idx + 1] & CFG_A_MASK == CFG_A_TOR
        {
            return;
        }
        self.addr[idx] = val & PMPADDR_MASK;
    }

    /// 写一项 pmpcfg 字节；L=1 时忽略；保留位/保留编码按 WARL 规范化
    pub fn write_cfg(&mut self, idx: usize, val: u8) {
        if idx >= PMP_COUNT || self.cfg[idx] & CFG_L != 0 {
            return;
        }
        let mut v = val & CFG_MASK;
        // R=0,W=1 为保留组合 → W 强制零
        if v & CFG_W != 0 && v & CFG_R == 0 {
            v &= !CFG_W;
        }
        self.cfg[idx] = v;
    }

    /// NAPOT 解码：返回 (基址, 大小)。ones 上限与 pmpaddr 实现位一致（54），
    /// pmpaddr 全 1（54 位）时 base=0、size=2^56，覆盖整个 PA 空间
    fn napot_range(pmpaddr: u64) -> (u64, u64) {
        let ones = pmpaddr.trailing_ones().min(54);
        let size = 8u64 << ones;
        let base = (pmpaddr & !((1u64 << ones) - 1)) << 2;
        (base, size.saturating_sub(4))
    }

    /// 访问 [pa, pa+len) 是否允许。
    ///
    /// 规范 3.7.1.3：与访问**任意字节**重叠的最低编号条目裁决本次访问；
    /// 该条目必须覆盖**全部**字节，否则立即失败（不继续匹配后续条目）。
    /// L=0 时 M 态访问匹配即成功；L=1 时 R/W/X 对所有特权级生效。
    /// 无匹配：M 态成功，S/U 态失败（已实现 PMP 条目的情况下）。
    pub fn allows(&self, pa: u64, len: u64, access: PmpAccess, mode: Privilege) -> bool {
        // 越出 PA 宽度的访问不可能被任何规则覆盖 → 非 M 直接失败
        let end = match pa.checked_add(len) {
            Some(e) if e <= PLEN => e,
            _ => return mode == Privilege::M,
        };
        for i in 0..PMP_COUNT {
            let c = self.cfg[i];
            let (base, size) = match c & CFG_A_MASK {
                CFG_A_TOR => {
                    // 下界来自前一项 pmpaddr（无论其 A 为何）；entry0 下界 0
                    let lo = if i == 0 { 0 } else { self.addr[i - 1] << 2 };
                    let hi = self.addr[i] << 2;
                    if hi <= lo {
                        continue; // 空/倒置区域不匹配
                    }
                    (lo, hi - lo)
                }
                CFG_A_NA4 => (self.addr[i] << 2, 4),
                CFG_A_NAPOT => Self::napot_range(self.addr[i]),
                _ => continue, // OFF：不匹配
            };
            // 与任意字节重叠（size > 0 已由上面保证）
            if pa >= base + size || end <= base {
                continue;
            }
            // 第一条匹配任意字节的条目裁决：必须覆盖全部字节，否则失败
            if pa < base || end > base + size {
                return false;
            }
            if c & CFG_L == 0 && mode == Privilege::M {
                return true; // L=0：M 态匹配即成功
            }
            let need = match access {
                PmpAccess::Read => CFG_R,
                PmpAccess::Write => CFG_W,
                PmpAccess::Exec => CFG_X,
            };
            return c & need != 0;
        }
        mode == Privilege::M // 无匹配：M 成功，S/U 失败
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn napot_full_allow() {
        let mut pmp = Pmp::default();
        pmp.write_cfg(0, CFG_A_NAPOT | CFG_R | CFG_W | CFG_X);
        pmp.write_addr(0, PMPADDR_MASK); // 全 1（54 位）→ 覆盖 [0, 2^56)
        assert_eq!(pmp.num_rules(), 1);
        for mode in [Privilege::S, Privilege::U] {
            assert!(pmp.allows(0, 8, PmpAccess::Exec, mode));
            assert!(pmp.allows(0x8000_0000, 8, PmpAccess::Read, mode));
            assert!(pmp.allows(0x1000_0000, 1, PmpAccess::Write, mode));
        }
        assert!(pmp.allows(0, 8, PmpAccess::Write, Privilege::M), "M 态不受限");
    }

    #[test]
    fn empty_pmp_denies_less_privileged() {
        let pmp = Pmp::default();
        assert_eq!(pmp.num_rules(), 0);
        assert!(!pmp.allows(0x8000_0000, 4, PmpAccess::Read, Privilege::S));
        assert!(pmp.allows(0x8000_0000, 4, PmpAccess::Read, Privilege::M));
    }

    #[test]
    fn tor_region_and_permissions() {
        let mut pmp = Pmp::default();
        // RAM 区 [0x8000_0000, 0x8800_0000)：只读
        pmp.write_addr(0, 0x8000_0000 >> 2);
        pmp.write_addr(1, 0x8800_0000 >> 2);
        pmp.write_cfg(0, CFG_A_TOR); // OFF 的上一项当 TOR 下界
        pmp.write_cfg(1, CFG_A_TOR | CFG_R);
        assert!(pmp.allows(0x8000_1000, 8, PmpAccess::Read, Privilege::U));
        assert!(!pmp.allows(0x8000_1000, 8, PmpAccess::Write, Privilege::U));
        assert!(!pmp.allows(0x9000_0000, 8, PmpAccess::Read, Privilege::U), "区外无匹配应拒绝");
    }

    #[test]
    fn first_match_partial_coverage_fails() {
        let mut pmp = Pmp::default();
        // 规则 0：NA4 @0x8000_0000 覆盖 4 字节，只读
        pmp.write_cfg(0, CFG_A_NA4 | CFG_R);
        pmp.write_addr(0, 0x8000_0000 >> 2);
        // 8 字节访问从 0x8000_0000 开始：与规则 0 重叠但未全覆盖 → 直接失败
        assert!(
            !pmp.allows(0x8000_0000, 8, PmpAccess::Read, Privilege::U),
            "第一条匹配任意字节的条目必须覆盖全部字节，否则立即失败"
        );
        // 完整落在规则 0 内 → 允许
        assert!(pmp.allows(0x8000_0000, 4, PmpAccess::Read, Privilege::U));
    }

    #[test]
    fn first_match_priority_by_lowest_index() {
        let mut pmp = Pmp::default();
        // 规则 0（低优先裁决位）：NA4 只读 @0x8000_0004
        pmp.write_cfg(0, CFG_A_NA4 | CFG_R);
        pmp.write_addr(0, 0x8000_0004 >> 2);
        // 规则 1：NAPOT 大区域 RWX
        pmp.write_cfg(1, CFG_A_NAPOT | CFG_R | CFG_W | CFG_X);
        pmp.write_addr(1, (0x8000_0008 >> 2) | 1); // NAPOT [0x8000_0000, +8)
        // 访问 [4,8)：与规则 0 重叠（低编号裁决，只读）→ 读允许、写拒绝
        assert!(pmp.allows(0x8000_0004, 4, PmpAccess::Read, Privilege::U));
        assert!(!pmp.allows(0x8000_0004, 4, PmpAccess::Write, Privilege::U));
        // 访问 [0,4)：不与规则 0 重叠 → 规则 1 裁决 → 允许
        assert!(pmp.allows(0x8000_0000, 4, PmpAccess::Write, Privilege::U));
    }

    #[test]
    fn cross_boundary_access_denied() {
        let mut pmp = Pmp::default();
        // 规则 1：TOR [0x1000, 0x2000) RWX（entry0 作下界）
        pmp.write_addr(0, 0x1000 >> 2);
        pmp.write_addr(1, 0x2000 >> 2);
        pmp.write_cfg(1, CFG_A_TOR | CFG_R | CFG_W | CFG_X);
        // 跨区域边界的访问：起点在区域内 → 规则 1 裁决但未全覆盖 → 失败
        assert!(!pmp.allows(0x1FFC, 8, PmpAccess::Read, Privilege::U));
        // 起点在区域外但与区域重叠 → 规则 1 裁决失败
        assert!(!pmp.allows(0xFFC, 8, PmpAccess::Read, Privilege::U));
        // 完全落在区域内 → 允许
        assert!(pmp.allows(0x1FF0, 8, PmpAccess::Read, Privilege::U));
    }

    #[test]
    fn lock_prevents_writes() {
        let mut pmp = Pmp::default();
        // 先设地址、后锁定（L=1 后 cfg/addr 都不可再写）
        pmp.write_addr(0, PMPADDR_MASK);
        pmp.write_cfg(0, CFG_L | CFG_A_NAPOT | CFG_R);
        pmp.write_cfg(0, 0); // L=1：忽略
        pmp.write_addr(0, 0); // L=1：忽略
        assert_eq!(pmp.cfg[0] & CFG_A_MASK, CFG_A_NAPOT);
        assert_eq!(pmp.addr[0], PMPADDR_MASK, "写入截断到 54 位实现位");
    }

    #[test]
    fn locked_tor_locks_previous_addr() {
        let mut pmp = Pmp::default();
        pmp.write_addr(0, 0x8000_0000 >> 2); // TOR 下界
        pmp.write_addr(1, 0x8800_0000 >> 2); // TOR 上界
        pmp.write_cfg(1, CFG_L | CFG_A_TOR | CFG_R); // 锁定 entry1（TOR）
        // entry1 L=1 且 A=TOR → pmpaddr0 也被锁定
        pmp.write_addr(0, 0x1000 >> 2);
        assert_eq!(pmp.addr[0], 0x8000_0000 >> 2, "锁定的 TOR 项应锁定 pmpaddr[i-1]");
        // L=1 且 A=OFF 也锁定
        let mut pmp2 = Pmp::default();
        pmp2.write_cfg(0, CFG_L); // A=OFF + L
        pmp2.write_addr(0, 0x1234);
        assert_eq!(pmp2.addr[0], 0, "L=1、A=OFF 也锁定");
    }

    #[test]
    fn pmpaddr_implemented_bits_are_54() {
        let mut pmp = Pmp::default();
        pmp.write_addr(0, u64::MAX);
        assert_eq!(pmp.addr[0], PMPADDR_MASK, "位 [63:54] WARL 为零");
        // 粒度探测（软件流程）：写全 1 读回 → 最低置位 bit = G = 0 → 粒度 4B
        assert_eq!(pmp.addr[0].trailing_zeros(), 0, "G=0，NA4 支持");
    }

    #[test]
    fn pmpcfg_canonicalization() {
        let mut pmp = Pmp::default();
        // 保留位 6:5 读回零（L=bit7, A=4:3）
        pmp.write_cfg(0, 0xFF);
        assert_eq!(pmp.cfg[0], 0x9F, "保留位 6:5 读回零，L|NAPOT|RWX");
        // R=0,W=1 保留组合 → W 强制零
        pmp.write_cfg(1, CFG_W | CFG_X);
        assert_eq!(pmp.cfg[1] & CFG_W, 0, "R=0,W=1 → W 强制零");
        assert_eq!(pmp.cfg[1] & CFG_X, CFG_X);
        // NA4（A=2）是合法编码，原样保留
        pmp.write_cfg(2, CFG_A_NA4 | CFG_R);
        assert_eq!(pmp.cfg[2] & CFG_A_MASK, CFG_A_NA4, "A=2 = NA4 合法");
    }

    #[test]
    fn l_bit_enforces_m_mode() {
        let mut pmp = Pmp::default();
        // L=1 只读区域：M 态也受约束（地址须先于锁定写入）
        pmp.write_addr(0, (0x8000_0000 >> 2) | 1); // [0x8000_0000, +16)
        pmp.write_cfg(0, CFG_L | CFG_A_NAPOT | CFG_R);
        assert!(pmp.allows(0x8000_0000, 8, PmpAccess::Read, Privilege::M));
        assert!(!pmp.allows(0x8000_0000, 8, PmpAccess::Write, Privilege::M));
        assert!(!pmp.allows(0x8000_0000, 8, PmpAccess::Exec, Privilege::M));
        // L=0 的等价区域（规则 1）：M 态匹配即成功
        pmp.write_cfg(1, CFG_A_NAPOT | CFG_R);
        pmp.write_addr(1, (0x8000_1000 >> 2) | 1); // [0x8000_1000, +16)
        assert!(pmp.allows(0x8000_1000, 8, PmpAccess::Write, Privilege::M));
    }

    #[test]
    fn overflow_beyond_plen_denied() {
        let mut pmp = Pmp::default();
        pmp.write_cfg(0, CFG_A_NAPOT | CFG_R | CFG_W | CFG_X);
        pmp.write_addr(0, PMPADDR_MASK); // 覆盖 [0, 2^56)
        // 跨出 PLEN 的访问：非 M 拒绝（不可能被完整覆盖）
        assert!(!pmp.allows((1 << 56) - 4, 8, PmpAccess::Read, Privilege::U));
        assert!(pmp.allows((1 << 56) - 4, 4, PmpAccess::Read, Privilege::U));
    }
}
