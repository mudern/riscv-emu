//! PMP（物理内存保护）。
//!
//! 语义与 QEMU 对齐：
//! - 16 项规则；M 态不受限（不做 MML）
//! - 非 M 态的取指/读写按 PA 过 PMP，取**第一条匹配**的规则判权，无匹配即拒绝
//! - `mret` 到低特权级时若规则数为 0，直接抛 instruction access fault
//!   （对齐 QEMU `check_ret_from_m_mode`；空 PMP 下低特权级无法访问任何内存）

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

    /// 写 pmpaddr；对应规则 L=1 时忽略
    pub fn write_addr(&mut self, idx: usize, val: u64) {
        if idx < PMP_COUNT && self.cfg[idx] & CFG_L == 0 {
            self.addr[idx] = val & 0x00FF_FFFF_FFFF_FFFF; // RV64：56 位有效
        }
    }

    /// 写一项 pmpcfg 字节；L=1 时忽略
    pub fn write_cfg(&mut self, idx: usize, val: u8) {
        if idx < PMP_COUNT && self.cfg[idx] & CFG_L == 0 {
            self.cfg[idx] = val;
        }
    }

    /// NAPOT 解码：返回 (基址, 大小)。ones 上限 56（pmpaddr 有效位宽），
    /// pmpaddr 全 1 时得到 base=0、size=2^59，覆盖整个 virt 机器地址空间。
    fn napot_range(pmpaddr: u64) -> (u64, u64) {
        let ones = pmpaddr.trailing_ones().min(56);
        let size = 8u64 << ones;
        let base = (pmpaddr & !((1u64 << ones) - 1)) << 2;
        (base, size)
    }

    /// 访问 [pa, pa+len) 是否允许（对第一个匹配的规则判权；简化为区间包含）
    pub fn allows(&self, pa: u64, len: u64, access: PmpAccess, mode: Privilege) -> bool {
        if mode == Privilege::M {
            return true; // 无 MML：M 态不受限
        }
        let end = pa.saturating_add(len);
        for i in 0..PMP_COUNT {
            let c = self.cfg[i];
            let (base, size) = match c & CFG_A_MASK {
                CFG_A_TOR => {
                    let lo = if i == 0 { 0 } else { self.addr[i - 1] << 2 };
                    let hi = self.addr[i] << 2;
                    (lo, hi.saturating_sub(lo))
                }
                CFG_A_NA4 => (self.addr[i] << 2, 4),
                CFG_A_NAPOT => Self::napot_range(self.addr[i]),
                _ => continue, // OFF：不匹配
            };
            let hit = pa >= base && end <= base.saturating_add(size) && size > 0;
            if hit {
                let need = match access {
                    PmpAccess::Read => CFG_R,
                    PmpAccess::Write => CFG_W,
                    PmpAccess::Exec => CFG_X,
                };
                return c & need != 0;
            }
        }
        false // 无匹配：非 M 态拒绝
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn napot_full_allow() {
        let mut pmp = Pmp::default();
        pmp.write_cfg(0, CFG_A_NAPOT | CFG_R | CFG_W | CFG_X);
        pmp.write_addr(0, u64::MAX);
        assert_eq!(pmp.num_rules(), 1);
        for mode in [Privilege::S, Privilege::U] {
            assert!(pmp.allows(0, 8, PmpAccess::Exec, mode));
            assert!(pmp.allows(0x8000_0000, 8, PmpAccess::Read, mode));
            assert!(pmp.allows(0x1000_0000, 1, PmpAccess::Write, mode));
        }
        assert!(
            pmp.allows(0, 8, PmpAccess::Write, Privilege::M),
            "M 态不受限"
        );
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
        assert!(
            !pmp.allows(0x9000_0000, 8, PmpAccess::Read, Privilege::U),
            "区外无匹配应拒绝"
        );
    }

    #[test]
    fn first_match_wins() {
        let mut pmp = Pmp::default();
        // 规则 0 全空间 RWX，规则 1 再收窄也不影响（第一条匹配优先）
        pmp.write_cfg(0, CFG_A_NAPOT | CFG_R | CFG_W | CFG_X);
        pmp.write_addr(0, u64::MAX);
        pmp.write_cfg(1, CFG_A_NA4);
        pmp.write_addr(1, 0x8000_0000 >> 2);
        assert!(pmp.allows(0x8000_0000, 4, PmpAccess::Write, Privilege::U));
    }

    #[test]
    fn lock_prevents_writes() {
        let mut pmp = Pmp::default();
        // 先设地址、后锁定（L=1 后 cfg/addr 都不可再写）
        pmp.write_addr(0, u64::MAX);
        pmp.write_cfg(0, CFG_L | CFG_A_NAPOT | CFG_R);
        pmp.write_cfg(0, 0); // L=1：忽略
        pmp.write_addr(0, 0); // L=1：忽略
        assert_eq!(pmp.cfg[0] & CFG_A_MASK, CFG_A_NAPOT);
        assert_eq!(pmp.addr[0], 0x00FF_FFFF_FFFF_FFFF, "写入截断到 56 位有效位");
    }
}
