//! CSR 文件：M/S 两级 CSR、权限控制与位域视图（sie/sip 是 mie/mip 的视图）。
//!
//! mip 中的 MTIP 由运行循环按 CLINT 状态同步；SSIP/STIP/SEIP 由软件可写
//! （STIP/SEIP 仅在对应 mideleg 委派位置位时可写，符合特权规范）。

use crate::cpu::Privilege;
use crate::pmp::{PMP_COUNT, Pmp};

/// CSR 地址
#[allow(clippy::module_inception)]
pub mod csr {
    pub const SSTATUS: u16 = 0x100;
    pub const SIE: u16 = 0x104;
    pub const STVEC: u16 = 0x105;
    pub const SSCRATCH: u16 = 0x140;
    pub const SEPC: u16 = 0x141;
    pub const SCAUSE: u16 = 0x142;
    pub const STVAL: u16 = 0x143;
    pub const SIP: u16 = 0x144;
    pub const SATP: u16 = 0x180;
    pub const PMPCFG0: u16 = 0x3A0; // RV64 只有 pmpcfg0/2（各管 8 项）
    pub const PMPCFG2: u16 = 0x3A2;
    pub const PMPADDR0: u16 = 0x3B0; // ..= PMPADDR0+15
    pub const MSTATUS: u16 = 0x300;
    pub const MISA: u16 = 0x301;
    pub const MEDELEG: u16 = 0x302;
    pub const MIDELEG: u16 = 0x303;
    pub const MIE: u16 = 0x304;
    pub const MTVEC: u16 = 0x305;
    pub const MSCRATCH: u16 = 0x340;
    pub const MEPC: u16 = 0x341;
    pub const MCAUSE: u16 = 0x342;
    pub const MTVAL: u16 = 0x343;
    pub const MIP: u16 = 0x344;
    pub const MVENDORID: u16 = 0xF11;
    pub const MARCHID: u16 = 0xF12;
    pub const MIMPID: u16 = 0xF13;
    pub const MHARTID: u16 = 0xF14;
    pub const MCYCLE: u16 = 0xB00;
    pub const MINSTRET: u16 = 0xB02;
    pub const CYCLE: u16 = 0xC00;
    pub const TIME: u16 = 0xC01;
    pub const INSTRET: u16 = 0xC02;
}

// mstatus 位
pub const SIE: u64 = 1 << 1;
pub const MIE: u64 = 1 << 3;
pub const SPIE: u64 = 1 << 5;
pub const MPIE: u64 = 1 << 7;
pub const SPP: u64 = 1 << 11;
pub const MPP: u64 = 3 << 11; // 掩码
pub const MPRV: u64 = 1 << 17;
pub const SUM: u64 = 1 << 18;
pub const MXR: u64 = 1 << 19;
pub const TVM: u64 = 1 << 20;
pub const TW: u64 = 1 << 21;
pub const TSR: u64 = 1 << 22;

// 中断挂起/使能位（mip/mie/sip/sie 通用）
pub const SSIP: u64 = 1 << 1;
pub const MSIP: u64 = 1 << 3;
pub const STIP: u64 = 1 << 5;
pub const MTIP: u64 = 1 << 7;
pub const SEIP: u64 = 1 << 9;
pub const MEIP: u64 = 1 << 11;
pub const MIE_MASK: u64 = SSIP | MSIP | STIP | MTIP | SEIP | MEIP; // 0xAAA

const MSTATUS_WRITE_MASK: u64 =
    SIE | MIE | SPIE | MPIE | SPP | MPP | MPRV | SUM | MXR | TVM | TW | TSR;
const SSTATUS_MASK: u64 = SIE | SPIE | SPP | SUM | MXR;

/// 性能计数器快照（CSR 读取用）
pub struct Counters {
    pub cycle: u64,
    pub instret: u64,
    pub time: u64,
}

pub struct Csrs {
    pub misa: u64,
    pub mstatus: u64,
    pub medeleg: u64,
    pub mideleg: u64,
    pub mie: u64,
    /// 有效挂起位：MTIP 由机器循环同步，SSIP/STIP/SEIP 软件可写
    pub mip: u64,
    pub mtvec: u64,
    pub mscratch: u64,
    pub mepc: u64,
    pub mcause: u64,
    pub mtval: u64,
    pub stvec: u64,
    pub sscratch: u64,
    pub sepc: u64,
    pub scause: u64,
    pub stval: u64,
    pub satp: u64,
    /// PMP 规则（pmpcfg/pmpaddr CSR 的后端）
    pub pmp: Pmp,
}

impl Default for Csrs {
    fn default() -> Self {
        Self::new()
    }
}

impl Csrs {
    /// misa: RV64 IMA C
    pub fn new() -> Self {
        Csrs {
            misa: (2 << 62) | (1 << 0) | (1 << 2) | (1 << 8) | (1 << 12),
            mstatus: 0,
            medeleg: 0,
            mideleg: 0,
            mie: 0,
            mip: 0,
            mtvec: 0,
            mscratch: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
            stvec: 0,
            sscratch: 0,
            sepc: 0,
            scause: 0,
            stval: 0,
            satp: 0,
            pmp: Pmp::default(),
        }
    }

    /// S 态可写的中断挂起位：SSIP 恒可写；STIP/SEIP 仅在已委派时可写
    fn sip_writable(&self) -> u64 {
        SSIP | (if self.mideleg & STIP != 0 { STIP } else { 0 })
            | (if self.mideleg & SEIP != 0 { SEIP } else { 0 })
    }

    /// 读取 CSR；None 表示未知、无权限或非法访问（→ illegal instruction）。
    pub fn read(&self, addr: u16, c: &Counters, mode: Privilege) -> Option<u64> {
        let m_only = matches!(addr, 0x300..=0x7FF | 0xB00..=0xBFF | 0xF11..=0xF1F);
        if m_only && mode != Privilege::M {
            return None;
        }
        let s_only = matches!(addr, 0x100..=0x1FF);
        if s_only && mode < Privilege::S {
            return None;
        }
        if is_pmp_csr(addr) {
            return self.pmp_read(addr);
        }
        Some(match addr {
            csr::MSTATUS => self.mstatus | (2 << 32) | (2 << 34), // SXL/UXL = 64 位
            csr::MISA => self.misa,
            csr::MEDELEG => self.medeleg,
            csr::MIDELEG => self.mideleg,
            csr::MIE => self.mie,
            csr::MTVEC => self.mtvec,
            csr::MSCRATCH => self.mscratch,
            csr::MEPC => self.mepc,
            csr::MCAUSE => self.mcause,
            csr::MTVAL => self.mtval,
            csr::MIP => self.mip,
            csr::SSTATUS => (self.mstatus & SSTATUS_MASK) | (2 << 34), // UXL = 64 位
            csr::SIE => self.mie & self.sip_writable(),
            csr::STVEC => self.stvec,
            csr::SSCRATCH => self.sscratch,
            csr::SEPC => self.sepc,
            csr::SCAUSE => self.scause,
            csr::STVAL => self.stval,
            csr::SIP => self.mip & self.sip_writable(),
            csr::SATP => self.satp,
            csr::MVENDORID | csr::MARCHID | csr::MIMPID | csr::MHARTID => 0,
            csr::MCYCLE => c.cycle,
            csr::MINSTRET => c.instret,
            csr::CYCLE => c.cycle,
            csr::TIME => c.time,
            csr::INSTRET => c.instret,
            _ => return None,
        })
    }

    /// 写 CSR；false 表示只读、未知或无权限（→ illegal instruction）。
    pub fn write(&mut self, addr: u16, val: u64, mode: Privilege) -> bool {
        let m_only = matches!(addr, 0x300..=0x7FF | 0xB00..=0xBFF | 0xF11..=0xF1F);
        if m_only && mode != Privilege::M {
            return false;
        }
        let s_only = matches!(addr, 0x100..=0x1FF);
        if s_only && mode < Privilege::S {
            return false;
        }
        if is_pmp_csr(addr) {
            return self.pmp_write(addr, val);
        }
        match addr {
            csr::MSTATUS => {
                self.mstatus = (self.mstatus & !MSTATUS_WRITE_MASK) | (val & MSTATUS_WRITE_MASK)
            }
            csr::SSTATUS => self.mstatus = (self.mstatus & !SSTATUS_MASK) | (val & SSTATUS_MASK),
            csr::MEDELEG => self.medeleg = val & 0xFFFF,
            csr::MIDELEG => self.mideleg = val & (SSIP | STIP | SEIP), // 只有 S 态中断源可委派
            csr::MIE => self.mie = val & MIE_MASK,
            csr::SIE => {
                let mask = self.sip_writable();
                self.mie = (self.mie & !mask) | (val & mask)
            }
            csr::MIP | csr::SIP => {
                let mask = self.sip_writable();
                self.mip = (self.mip & !mask) | (val & mask)
            }
            csr::MTVEC => self.mtvec = val,
            csr::MSCRATCH => self.mscratch = val,
            csr::MEPC => self.mepc = val,
            csr::MCAUSE => self.mcause = val,
            csr::MTVAL => self.mtval = val,
            csr::STVEC => self.stvec = val,
            csr::SSCRATCH => self.sscratch = val,
            csr::SEPC => self.sepc = val,
            csr::SCAUSE => self.scause = val,
            csr::STVAL => self.stval = val,
            csr::SATP => self.satp = val,
            _ => return false,
        }
        true
    }

    /// pmpcfg0/2（RV64 各管 8 项）与 pmpaddr0-15；pmpcfg1/3 不存在。
    fn pmp_read(&self, addr: u16) -> Option<u64> {
        Some(match addr {
            csr::PMPCFG0 => Self::pack_cfg(&self.pmp.cfg[0..8]),
            csr::PMPCFG2 => Self::pack_cfg(&self.pmp.cfg[8..16]),
            a if (csr::PMPADDR0..csr::PMPADDR0 + PMP_COUNT as u16).contains(&a) => {
                self.pmp.addr[(a - csr::PMPADDR0) as usize]
            }
            _ => return None,
        })
    }

    fn pmp_write(&mut self, addr: u16, val: u64) -> bool {
        match addr {
            csr::PMPCFG0 => self.pmp_write_cfgs(0, val),
            csr::PMPCFG2 => self.pmp_write_cfgs(8, val),
            a if (csr::PMPADDR0..csr::PMPADDR0 + PMP_COUNT as u16).contains(&a) => {
                self.pmp.write_addr((a - csr::PMPADDR0) as usize, val)
            }
            _ => return false,
        }
        true
    }

    fn pack_cfg(cfgs: &[u8]) -> u64 {
        cfgs.iter()
            .enumerate()
            .fold(0, |v, (i, c)| v | (*c as u64) << (i * 8))
    }

    /// 逐字节写 pmpcfg（L=1 的项保持不变，由 Pmp::write_cfg 处理）
    fn pmp_write_cfgs(&mut self, first: usize, val: u64) {
        for i in 0..8 {
            self.pmp.write_cfg(first + i, (val >> (i * 8)) as u8);
        }
    }
}

/// 地址是否是已实现的 PMP CSR（pmpcfg0/2 + pmpaddr0-15；
/// pmpcfg1/3 及 pmpcfg4-15 在 RV64 不存在，走 unknown → illegal）
fn is_pmp_csr(addr: u16) -> bool {
    matches!(addr, 0x3A0 | 0x3A2 | 0x3B0..=0x3BF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters() -> Counters {
        Counters {
            cycle: 1,
            instret: 2,
            time: 3,
        }
    }

    #[test]
    fn permission() {
        let mut csr = Csrs::new();
        assert_eq!(csr.read(csr::SSTATUS, &counters(), Privilege::U), None);
        assert_eq!(csr.read(csr::MSTATUS, &counters(), Privilege::S), None);
        assert!(!csr.write(csr::MEPC, 1, Privilege::S));
        assert_eq!(csr.read(csr::CYCLE, &counters(), Privilege::U), Some(1));
    }

    #[test]
    fn sstatus_is_view_of_mstatus() {
        let mut csr = Csrs::new();
        assert!(csr.write(csr::SSTATUS, !0, Privilege::S));
        assert_eq!(csr.mstatus & !SSTATUS_MASK, 0, "sstatus 只能影响 S 可见位");
        // M 态写 mstatus.MIE 不应被 sstatus 读出
        assert!(csr.write(csr::MSTATUS, MIE, Privilege::M));
        let view = csr.read(csr::SSTATUS, &counters(), Privilege::S).unwrap();
        assert_eq!(view & MIE, 0);
        assert_eq!(view & (2 << 34), 2 << 34, "UXL 应读出 64 位");
        // mstatus 读出 SXL/UXL
        let ms = csr.read(csr::MSTATUS, &counters(), Privilege::M).unwrap();
        assert_eq!(ms & (2 << 32) | (2 << 34), (2 << 32) | (2 << 34));
    }

    #[test]
    fn sip_writable_only_when_delegated() {
        let mut csr = Csrs::new();
        let c = counters();
        // 未委派：写 sip.STIP 被忽略
        assert!(csr.write(csr::SIP, STIP, Privilege::S));
        assert_eq!(csr.read(csr::SIP, &c, Privilege::S).unwrap(), 0);
        // 委派后可写
        csr.mideleg = STIP;
        assert!(csr.write(csr::SIP, STIP, Privilege::S));
        assert_eq!(csr.read(csr::SIP, &c, Privilege::S).unwrap(), STIP);
        // M 态 mip 同样受委派位约束（除 SSIP）
        csr.mideleg = 0;
        assert!(csr.write(csr::MIP, 0, Privilege::M)); // 尝试清 STIP → 被忽略
        assert_eq!(csr.mip & STIP, STIP, "未委派的 STIP 不应可写");
        assert!(csr.write(csr::MIP, SSIP, Privilege::M));
        assert_eq!(csr.mip & SSIP, SSIP);
    }

    #[test]
    fn pmp_csr_roundtrip_and_lock() {
        let mut csr = Csrs::new();
        let c = counters();
        // pmpaddr 属 M 态：S 态读写都拒绝
        assert!(!csr.write(csr::PMPADDR0, 1, Privilege::S));
        assert_eq!(csr.read(csr::PMPADDR0, &c, Privilege::S), None);
        assert!(csr.write(csr::PMPADDR0, 0x1234, Privilege::M));
        assert_eq!(csr.read(csr::PMPADDR0, &c, Privilege::M), Some(0x1234));
        // pmpcfg0 字节 0 = NAPOT|RWX；pmpcfg1 在 RV64 不存在
        assert!(csr.write(csr::PMPCFG0, 0x1F, Privilege::M));
        assert_eq!(csr.read(csr::PMPCFG0, &c, Privilege::M), Some(0x1F));
        assert_eq!(csr.pmp.num_rules(), 1);
        assert_eq!(csr.read(0x3A1, &c, Privilege::M), None);
        // 写 L 后该项锁死：再写被忽略
        assert!(csr.write(csr::PMPCFG0, 0x80, Privilege::M));
        assert!(csr.write(csr::PMPCFG0, 0, Privilege::M));
        assert_eq!(csr.read(csr::PMPCFG0, &c, Privilege::M), Some(0x80));
    }
}
