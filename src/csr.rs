//! CSR 文件：M/S 两级 CSR、权限控制与位域视图。
//!
//! 可写性 / 可见性分离（规范 3.1.9 / 4.1.3）：
//! - mip 写掩码 = SSIP|STIP|SEIP（规范：SSIP 恒可写；stimecmp 未实现时
//!   STIP 可写；SEIP 可写并与 PLIC 信号逻辑或）。MSIP/MTIP/MEIP 只读，
//!   分别由 CLINT msip/CLINT mtimecmp/PLIC 驱动。
//! - sip/sie 可见位 = mideleg & 标准中断位（sip 是 mip/mie 的委派子集）；
//!   sip 写掩码同样只覆盖已委派位。
//!
//! WARL 在写入时规范化（"写非法值、读回合法值"）。

use crate::cpu::Privilege;
use crate::pmp::{PMP_COUNT, Pmp};

/// CSR 地址
#[allow(clippy::module_inception)]
pub mod csr {
    pub const FFLAGS: u16 = 0x001;
    pub const FRM: u16 = 0x002;
    pub const FCSR: u16 = 0x003;
    pub const SSTATUS: u16 = 0x100;
    pub const SIE: u16 = 0x104;
    pub const STVEC: u16 = 0x105;
    pub const SCOUNTEREN: u16 = 0x106;
    pub const SENVCFG: u16 = 0x10A;
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
    pub const MCOUNTEREN: u16 = 0x306;
    pub const MENVCFG: u16 = 0x30A;
    pub const MCOUNTINHIBIT: u16 = 0x320;
    pub const MSCRATCH: u16 = 0x340;
    pub const MEPC: u16 = 0x341;
    pub const MCAUSE: u16 = 0x342;
    pub const MTVAL: u16 = 0x343;
    pub const MIP: u16 = 0x344;
    pub const MCYCLE: u16 = 0xB00;
    pub const MINSTRET: u16 = 0xB02;
    pub const MVENDORID: u16 = 0xF11;
    pub const MARCHID: u16 = 0xF12;
    pub const MIMPID: u16 = 0xF13;
    pub const MHARTID: u16 = 0xF14;
    pub const CYCLE: u16 = 0xC00;
    pub const TIME: u16 = 0xC01;
    pub const INSTRET: u16 = 0xC02;
}

// mstatus 位
pub const SIE: u64 = 1 << 1;
pub const MIE: u64 = 1 << 3;
pub const SPIE: u64 = 1 << 5;
pub const MPIE: u64 = 1 << 7;
pub const SPP: u64 = 1 << 8;
pub const MPP: u64 = 3 << 11; // 掩码
pub const FS: u64 = 3 << 13; // 掩码：Off/Initial/Clean/Dirty
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

/// mip 软件可写位（规范 3.1.9）：SSIP 恒可写；stimecmp 未实现时 STIP 可写；
/// SEIP 可写（与 PLIC 信号逻辑或）。MSIP/MTIP/MEIP 只读。
const MIP_WRITABLE: u64 = SSIP | STIP | SEIP;

/// 本实现可委派的同步异常（medeleg WARL 掩码）：排除 ecall-from-M（11，
/// 陷阱不下行）与保留位（10、14）
const MEDELEG_MASK: u64 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 6)
    | (1 << 8)
    | (1 << 9)
    | (1 << 12)
    | (1 << 13)
    | (1 << 15);

const MSTATUS_WRITE_MASK: u64 =
    SIE | MIE | SPIE | MPIE | SPP | MPP | FS | MPRV | SUM | MXR | TVM | TW | TSR;
const SSTATUS_MASK: u64 = SIE | SPIE | SPP | FS | SUM | MXR;

/// mstatus.MPP 合法值 {0,1,3}；写入保留值 2 时规范化为 0（U）
fn canonicalize_mstatus(v: u64) -> u64 {
    if v & MPP == 2 << 11 {
        v & !MPP
    } else {
        v
    }
}

/// Sv39 的 satp 有效低位：ASID 16 位（bits 59:44）+ PPN 44 位（bits 43:0）
const SATP_LOW_MASK: u64 = (1 << 60) - 1;

pub struct Csrs {
    pub misa: u64,
    pub mstatus: u64,
    pub medeleg: u64,
    pub mideleg: u64,
    pub mie: u64,
    /// 有效挂起位：MSIP/MTIP 由机器循环按 CLINT 同步，MEIP 只读取自
    /// ext_mip；SSIP/STIP/SEIP 软件可写
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
    /// 计数器访问使能：mcounteren 管 S/U，scounteren 管 U
    pub mcounteren: u64,
    pub scounteren: u64,
    /// 计数抑制（WARL）：CY(bit0) 抑制 mcycle、IR(bit2) 抑制 minstret；
    /// TM 无效果（mtime 不可抑制）
    pub mcountinhibit: u64,
    /// 机器计数器（M 态可写；按 mcountinhibit 抑制自增；cycle 与
    /// instret 独立维护）
    pub mcycle: u64,
    pub minstret: u64,
    /// fcsr：fflags（低 5 位）| frm（位 5-7）
    pub fcsr: u32,
    pub menvcfg: u64,
    pub senvcfg: u64,
    /// PMP 规则（pmpcfg/pmpaddr CSR 的后端）
    pub pmp: Pmp,
}

impl Default for Csrs {
    fn default() -> Self {
        Self::new()
    }
}

impl Csrs {
    /// misa: RV64 IMA F D C S U
    pub fn new() -> Self {
        Csrs {
            misa: (2 << 62)
                | (1 << 0) // I
                | (1 << 2) // M
                | (1 << 3) // D
                | (1 << 5) // F
                | (1 << 8) // A
                | (1 << 12) // C
                | (1 << 18) // S
                | (1 << 20), // U
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
            mcounteren: 0,
            scounteren: 0,
            mcountinhibit: 0,
            mcycle: 0,
            minstret: 0,
            fcsr: 0,
            menvcfg: 0,
            senvcfg: 0,
            pmp: Pmp::default(),
        }
    }

    /// mstatus.FS 是否为 Dirty
    pub fn fs_dirty(&self) -> bool {
        (self.mstatus >> 13) & 3 == 3
    }

    /// misa 是否支持浮点扩展（F 或 D）
    pub fn has_fp(&self) -> bool {
        self.misa & ((1 << 5) | (1 << 3)) != 0
    }

    /// sip/sie 可见位 = 已委派的标准中断位（规范 4.1.3：sip 是 mip 的子集）
    fn sip_visible(&self) -> u64 {
        self.mideleg & MIE_MASK
    }

    /// 读取 CSR；None 表示未知、无权限或非法访问（→ illegal instruction）。
    ///
    /// `ext_mip`：PLIC 持有的外部中断线（MEIP），读 mip/sip 时并入视图；
    /// `time`：CLINT mtime 当前值。
    pub fn read(&self, addr: u16, mode: Privilege, ext_mip: u64, time: u64) -> Option<u64> {
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
        if matches!(addr, csr::FFLAGS | csr::FRM | csr::FCSR) {
            // 浮点 CSR 随 F/D 扩展存在；FS=Off 时访问 → illegal
            if !self.has_fp() || (self.mstatus >> 13) & 3 == 0 {
                return None;
            }
            return Some(match addr {
                csr::FFLAGS => (self.fcsr & 0x1F) as u64,
                csr::FRM => ((self.fcsr >> 5) & 7) as u64,
                _ => (self.fcsr & 0xFF) as u64,
            });
        }
        // mstatus.SD（bit 63）只读：FS 为 Dirty 时置 1（无 V/XS 扩展）
        let sd = if self.fs_dirty() { 1 << 63 } else { 0 };
        // TVM=1 时 S 态访问 satp → illegal（读也非法）
        if addr == csr::SATP && mode == Privilege::S && self.mstatus & TVM != 0 {
            return None;
        }
        Some(match addr {
            csr::MSTATUS => self.mstatus | sd | (2 << 32) | (2 << 34), // +SXL/UXL
            csr::MISA => self.misa,
            csr::MEDELEG => self.medeleg,
            csr::MIDELEG => self.mideleg,
            csr::MIE => self.mie,
            csr::MTVEC => self.mtvec,
            csr::MSCRATCH => self.mscratch,
            csr::MEPC => self.mepc,
            csr::MCAUSE => self.mcause,
            csr::MTVAL => self.mtval,
            csr::MIP => (self.mip | ext_mip) & MIE_MASK,
            csr::SSTATUS => (self.mstatus & SSTATUS_MASK) | sd | (2 << 32), // +UXL
            csr::SIE => self.mie & self.sip_visible(),
            csr::STVEC => self.stvec,
            csr::SSCRATCH => self.sscratch,
            csr::SEPC => self.sepc,
            csr::SCAUSE => self.scause,
            csr::STVAL => self.stval,
            csr::SIP => (self.mip | ext_mip) & self.sip_visible(),
            csr::SATP => self.satp,
            csr::SCOUNTEREN => self.scounteren,
            csr::SENVCFG => self.senvcfg,
            csr::MCOUNTEREN => self.mcounteren,
            csr::MENVCFG => self.menvcfg,
            csr::MCOUNTINHIBIT => self.mcountinhibit,
            csr::MVENDORID | csr::MARCHID | csr::MIMPID | csr::MHARTID => 0,
            // U/S 态读计数器需要 mcounteren（U 还需 scounteren）对应位
            csr::MCYCLE | csr::MINSTRET | csr::CYCLE | csr::TIME | csr::INSTRET => {
                let bit = match addr {
                    csr::MCYCLE | csr::CYCLE => 1,
                    csr::TIME => 1 << 1,
                    _ => 1 << 2,
                };
                let s_ok = self.mcounteren & bit != 0;
                let u_ok = s_ok && self.scounteren & bit != 0;
                let val = match addr {
                    csr::MCYCLE | csr::CYCLE => self.mcycle,
                    csr::MINSTRET | csr::INSTRET => self.minstret,
                    _ => time,
                };
                match mode {
                    Privilege::M => val,
                    Privilege::S if s_ok => val,
                    Privilege::U if u_ok => val,
                    _ => return None,
                }
            }
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
        if matches!(addr, csr::FFLAGS | csr::FRM | csr::FCSR) {
            if !self.has_fp() || (self.mstatus >> 13) & 3 == 0 {
                return false;
            }
            match addr {
                csr::FFLAGS => self.fcsr = (self.fcsr & !0x1F) | (val as u32 & 0x1F),
                csr::FRM => self.fcsr = (self.fcsr & !(0x7 << 5)) | ((val as u32 & 0x7) << 5),
                _ => self.fcsr = (self.fcsr & !0xFF) | (val as u32 & 0xFF),
            }
            return true;
        }
        // TVM=1 时 S 态写 satp → illegal
        if addr == csr::SATP && mode == Privilege::S && self.mstatus & TVM != 0 {
            return false;
        }
        match addr {
            csr::MSTATUS => {
                self.mstatus = canonicalize_mstatus(
                    (self.mstatus & !MSTATUS_WRITE_MASK) | (val & MSTATUS_WRITE_MASK),
                );
            }
            csr::SSTATUS => self.mstatus = canonicalize_mstatus(
                (self.mstatus & !SSTATUS_MASK) | (val & SSTATUS_MASK),
            ),
            csr::MISA => {
                // 可写位 = 本实现支持的扩展（I M A F D C S U）；MXL 只读
                const SUPPORTED: u64 = (1 << 0)
                    | (1 << 2)
                    | (1 << 3)
                    | (1 << 5)
                    | (1 << 8)
                    | (1 << 12)
                    | (1 << 18)
                    | (1 << 20);
                self.misa = (2 << 62) | (val & SUPPORTED);
            }
            csr::MEDELEG => self.medeleg = val & MEDELEG_MASK,
            csr::MIDELEG => self.mideleg = val & MIP_WRITABLE, // 可委派 = S 态中断源
            csr::MIE => self.mie = val & MIE_MASK,
            csr::SIE => {
                let mask = self.sip_visible();
                self.mie = (self.mie & !mask) | (val & mask)
            }
            // mip：软件可写位 SSIP/STIP/SEIP；MSIP/MTIP/MEIP 只读
            csr::MIP => self.mip = (self.mip & !MIP_WRITABLE) | (val & MIP_WRITABLE),
            // sip：只能影响已委派位（子集窗口）
            csr::SIP => {
                let mask = self.sip_visible();
                self.mip = (self.mip & !mask) | (val & mask)
            }
            // WARL：BASE 4 字节对齐（位 1:0 即 MODE），MODE 仅支持
            // direct(0)/vectored(1)，保留编码 2/3 规范化为 direct
            csr::MTVEC => {
                let base = val & !3;
                self.mtvec = base | match val & 3 {
                    1 => 1,
                    _ => 0,
                };
            }
            csr::STVEC => {
                let base = val & !3;
                self.stvec = base | match val & 3 {
                    1 => 1,
                    _ => 0,
                };
            }
            csr::MSCRATCH => self.mscratch = val,
            // mepc[0] 恒 0（IALIGN=16，C 扩展支持 2 字节对齐）
            csr::MEPC => self.mepc = val & !1,
            csr::MCAUSE => self.mcause = val,
            csr::MTVAL => self.mtval = val,
            csr::SSCRATCH => self.sscratch = val,
            csr::SEPC => self.sepc = val & !1,
            csr::SCAUSE => self.scause = val,
            csr::STVAL => self.stval = val,
            // WARL：MODE 仅支持 Bare(0)/Sv39(8)，不支持的模式整个写入无效；
            // ASID 16 位（Sv39 ASIDMAX）+ PPN 44 位
            csr::SATP => {
                if let m @ (0 | 8) = (val >> 60) & 0xF {
                    self.satp = (m << 60) | (val & SATP_LOW_MASK);
                }
            },
            csr::SCOUNTEREN => self.scounteren = val & 0x7,
            csr::SENVCFG => self.senvcfg = val,
            csr::MCOUNTEREN => self.mcounteren = val & 0x7,
            csr::MENVCFG => self.menvcfg = val,
            // WARL：仅 CY/TM/IR（位 0-2）可写，HPM 未实现读回零
            csr::MCOUNTINHIBIT => self.mcountinhibit = val & 0x7,
            // 机器计数器：M 态可写；自增由 mcountinhibit 抑制（见 cpu.step）
            csr::MCYCLE => self.mcycle = val,
            csr::MINSTRET => self.minstret = val,
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

    fn r(csr: &Csrs, addr: u16, mode: Privilege) -> Option<u64> {
        csr.read(addr, mode, 0, 3)
    }

    #[test]
    fn permission() {
        let mut csr = Csrs::new();
        assert_eq!(r(&csr, csr::SSTATUS, Privilege::U), None);
        assert_eq!(r(&csr, csr::MSTATUS, Privilege::S), None);
        assert!(!csr.write(csr::MEPC, 1, Privilege::S));
        // 计数器访问权限：U 态默认无权，mcounteren/scounteren 使能后放开
        assert_eq!(r(&csr, csr::CYCLE, Privilege::U), None);
        assert_eq!(r(&csr, csr::TIME, Privilege::S), None);
        assert!(csr.write(csr::MCOUNTEREN, 0x7, Privilege::M));
        assert_eq!(r(&csr, csr::TIME, Privilege::S), Some(3));
        assert_eq!(r(&csr, csr::CYCLE, Privilege::U), None);
        assert!(csr.write(csr::SCOUNTEREN, 0x7, Privilege::S));
        assert_eq!(r(&csr, csr::CYCLE, Privilege::U), Some(0));
    }

    #[test]
    fn sstatus_is_view_of_mstatus() {
        let mut csr = Csrs::new();
        assert!(csr.write(csr::SSTATUS, !0, Privilege::S));
        assert_eq!(csr.mstatus & !SSTATUS_MASK, 0, "sstatus 只能影响 S 可见位");
        // M 态写 mstatus.MIE 不应被 sstatus 读出
        assert!(csr.write(csr::MSTATUS, MIE, Privilege::M));
        let view = r(&csr, csr::SSTATUS, Privilege::S).unwrap();
        assert_eq!(view & MIE, 0);
        assert_eq!(view & (2 << 32), 2 << 32, "UXL 应读出 64 位");
        // mstatus 读出 SXL/UXL
        let ms = r(&csr, csr::MSTATUS, Privilege::M).unwrap();
        assert_eq!(ms & (2 << 32) | (2 << 34), (2 << 32) | (2 << 34));
    }

    #[test]
    fn mip_write_semantics_split_from_sip() {
        let mut csr = Csrs::new();
        // mip：SSIP/STIP/SEIP 恒可写（M 态转发用），MSIP/MTIP/MEIP 只读
        assert!(csr.write(csr::MIP, SSIP | STIP | SEIP, Privilege::M));
        assert_eq!(csr.mip, SSIP | STIP | SEIP);
        csr.write(csr::MIP, MIE_MASK, Privilege::M);
        assert_eq!(
            csr.mip,
            SSIP | STIP | SEIP,
            "MSIP/MTIP/MEIP 只读（由 CLINT/PLIC 驱动）"
        );
        // sip：未委派时全部不可见/不可写
        assert!(csr.write(csr::SIP, MIE_MASK, Privilege::S));
        assert_eq!(csr.read(csr::SIP, Privilege::S, 0, 3), Some(0));
        // 委派 STIP 后：sip 只暴露 STIP；经 sip 写未委派的 SSIP 无效
        csr.mideleg = STIP;
        csr.mip = 0; // 清场后单独验证
        assert_eq!(csr.read(csr::SIP, Privilege::S, 0, 3).unwrap(), 0);
        csr.write(csr::SIP, SSIP, Privilege::S);
        assert_eq!(csr.mip & SSIP, 0, "未委派的 SSIP 经 sip 不可写");
        csr.write(csr::SIP, STIP, Privilege::S);
        assert_eq!(csr.mip & STIP, STIP, "已委派的 STIP 经 sip 可写");
        csr.write(csr::SIP, 0, Privilege::S);
        assert_eq!(csr.mip & STIP, 0, "已委派的 STIP 经 sip 可清零");
    }

    #[test]
    fn satp_warl_and_tvm() {
        let mut csr = Csrs::new();
        // 不支持 MODE=9：整个写入无效（保持原值 0）
        let v9 = (9u64 << 60) | 0x1234;
        assert!(csr.write(csr::SATP, v9, Privilege::M));
        assert_eq!(csr.satp, 0, "不支持的 MODE 整个写入无效");
        // Sv39：ASID 16 位 + PPN 44 位
        let v8 = (8u64 << 60) | (0x1234 << 44) | 0x8000_0000;
        assert!(csr.write(csr::SATP, v8, Privilege::M));
        assert_eq!(csr.satp, v8 & SATP_LOW_MASK | (8 << 60));
        assert_eq!(csr.satp >> 44 & 0xFFFF, 0x1234, "ASID 16 位");
        // TVM=1：S 态读/写 satp 都 illegal
        csr.mstatus |= TVM;
        assert_eq!(r(&csr, csr::SATP, Privilege::S), None);
        assert!(!csr.write(csr::SATP, 0, Privilege::S));
        csr.mstatus &= !TVM;
        assert_eq!(r(&csr, csr::SATP, Privilege::S), Some(csr.satp));
    }

    #[test]
    fn tvec_warl() {
        let mut csr = Csrs::new();
        // vectored 模式保留
        assert!(csr.write(csr::MTVEC, 0x1000 | 1, Privilege::M));
        assert_eq!(csr.mtvec, 0x1000 | 1);
        // 保留 MODE 2/3 → 规范化为 direct，BASE 对齐
        assert!(csr.write(csr::MTVEC, 0x1002, Privilege::M));
        assert_eq!(csr.mtvec, 0x1000);
        assert!(csr.write(csr::MTVEC, 0x1234, Privilege::M));
        assert_eq!(csr.mtvec, 0x1234, "BASE=0x1234 对齐，MODE=0");
        // stvec 同理
        assert!(csr.write(csr::STVEC, 0x2003, Privilege::S));
        assert_eq!(csr.stvec, 0x2000);
    }

    #[test]
    fn mpp_warl() {
        let mut csr = Csrs::new();
        // MPP=2（保留）→ 规范化为 0
        let v = 2u64 << 11;
        assert!(csr.write(csr::MSTATUS, v, Privilege::M));
        assert_eq!(csr.mstatus & MPP, 0, "MPP=2 保留 → 规范化为 U");
        // MPP=3 合法
        assert!(csr.write(csr::MSTATUS, 3u64 << 11, Privilege::M));
        assert_eq!(csr.mstatus & MPP, 3 << 11);
    }

    #[test]
    fn counters_rw_and_inhibit() {
        let mut csr = Csrs::new();
        // M 态可写（规范：mcycle/minstret 为 RW）
        assert!(csr.write(csr::MCYCLE, 100, Privilege::M));
        assert!(csr.write(csr::MINSTRET, 200, Privilege::M));
        assert_eq!(r(&csr, csr::MCYCLE, Privilege::M), Some(100));
        assert_eq!(r(&csr, csr::MINSTRET, Privilege::M), Some(200));
        // mcountinhibit：CY(bit0)/IR(bit2) 可写
        assert!(csr.write(csr::MCOUNTINHIBIT, 0x5, Privilege::M));
        assert_eq!(csr.mcountinhibit, 0x5);
        // 委派给 S 后可读（计数器继续走，由 cpu.step 控制）
        csr.mcounteren = 0x5;
        csr.mideleg = 0;
        assert_eq!(csr.read(csr::CYCLE, Privilege::S, 0, 3), Some(100));
    }

    #[test]
    fn medeleg_warl() {
        let mut csr = Csrs::new();
        assert!(csr.write(csr::MEDELEG, !0, Privilege::M));
        // ecall-from-M(11) 与保留位(10,14) 不可委派
        assert_eq!(csr.medeleg & (1 << 11), 0, "ecall-from-M 不可委派");
        assert_eq!(csr.medeleg & (1 << 10), 0, "保留位 10");
        assert_eq!(csr.medeleg & (1 << 14), 0, "保留位 14");
        assert_eq!(csr.medeleg & (1 << 8), 1 << 8, "ecall-from-U 可委派");
    }

    #[test]
    fn pmp_csr_roundtrip_and_lock() {
        let mut csr = Csrs::new();
        // pmpaddr 属 M 态：S 态读写都拒绝
        assert!(!csr.write(csr::PMPADDR0, 1, Privilege::S));
        assert_eq!(csr.read(csr::PMPADDR0, Privilege::S, 0, 3), None);
        assert!(csr.write(csr::PMPADDR0, 0x1234, Privilege::M));
        assert_eq!(csr.read(csr::PMPADDR0, Privilege::M, 0, 3), Some(0x1234));
        // pmpcfg0 字节 0 = NAPOT|RWX；pmpcfg1 在 RV64 不存在
        assert!(csr.write(csr::PMPCFG0, 0x1F, Privilege::M));
        assert_eq!(csr.read(csr::PMPCFG0, Privilege::M, 0, 3), Some(0x1F));
        assert_eq!(csr.pmp.num_rules(), 1);
        assert_eq!(csr.read(0x3A1, Privilege::M, 0, 3), None);
        // 写 L 后该项锁死：再写被忽略
        assert!(csr.write(csr::PMPCFG0, 0x80, Privilege::M));
        assert!(csr.write(csr::PMPCFG0, 0, Privilege::M));
        assert_eq!(csr.read(csr::PMPCFG0, Privilege::M, 0, 3), Some(0x80));
    }
}
