//! PLIC（RISC-V 平台级中断控制器，QEMU `virt` 布局在 0x0C00_0000）。
//!
//! 单 hart 两个 context：0 = M，1 = S。寄存器布局与 QEMU virt / Linux
//! `irq-sifive-plic` 一致：
//!   - 优先级：`0x0 + 4*src`（源 1..=95，3 位有效，对齐 virt 的 96 源/7 级）
//!   - 挂起位图：`0x1000`（只读）
//!   - 使能位图：`0x2000 + ctx*0x80`（每 context 3 个字）
//!   - 阈值/认领：`0x200000 + ctx*0x1000`（+0 阈值，+4 claim/complete）
//!
//! 语义对齐 QEMU `hw/intc/sifive_plic.c`：
//!   - 挂起位在设备线上沿时锁存置位，仅由 claim 清除（电平下沿不清除）；
//!   - claim 清挂起、置 in-service（网关占用），complete 清除；
//!   - 中断线电平 = 存在 pending & ~in-service & enable 且优先级 > 阈值的源。
//!
//! 认领规则：可通知源中优先级最高者，同优先级取小号。

pub const PLIC_SOURCES: usize = 96; // 源 1..=95（源 0 = 无）
pub const UART_IRQ: u32 = 10;

const WORDS: usize = (PLIC_SOURCES + 31).div_ceil(32);

pub struct Plic {
    /// 上一拍的线电平（sync 边沿检测用）
    prev_uart: bool,
    priority: [u32; PLIC_SOURCES],
    /// 挂起锁存（线上沿置位，claim 清除）
    pending: [u32; WORDS],
    /// claim → complete 之间的网关占用位图
    in_service: [u32; WORDS],
    /// [ctx][word]：ctx 0 = M，1 = S
    enable: [[u32; WORDS]; 2],
    threshold: [u32; 2],
}

impl Default for Plic {
    fn default() -> Self {
        Plic {
            prev_uart: false,
            priority: [0; PLIC_SOURCES],
            pending: [0; WORDS],
            in_service: [0; WORDS],
            enable: [[0; WORDS]; 2],
            threshold: [0; 2],
        }
    }
}

/// 设备中断线集合（Bus::plic_sync 的输入；源号对齐 virt 布局）
#[derive(Default, Clone, Copy)]
pub struct IrqLines {
    pub uart: bool,
}

impl Plic {
    /// 设备线电平扫描：仅在**上沿**（低→高）置挂起锁存。
    /// 设备在状态变化后主动重发请求（FIFO 仍有数据等）走 [`Plic::latch`]。
    pub fn sync(&mut self, lines: IrqLines) {
        if lines.uart && !self.prev_uart {
            self.pending[0] |= 1 << (UART_IRQ % 32);
        }
        self.prev_uart = lines.uart;
    }

    /// 设备重断言：线当前为高时锁存挂起位（对齐 QEMU 设备每次状态变化后
    /// 重发 qemu_set_irq 的行为，例如 RBR 弹出后 FIFO 仍非空）。
    pub fn latch(&mut self, src: u32) {
        if src > 0 && (src as usize) < PLIC_SOURCES {
            self.pending[(src / 32) as usize] |= 1 << (src % 32);
        }
    }

    /// context 下最高优先级的可通知源（pending & !in_service & enable 且
    /// 优先级 > 阈值）。无则 None。
    fn best(&self, ctx: usize) -> Option<(u32, u32)> {
        let mut best: Option<(u32, u32)> = None; // (priority, src)
        for (w, pend) in self.pending.iter().enumerate() {
            let mut bits = pend & !self.in_service[w] & self.enable[ctx][w];
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                let src = (w * 32 + bit as usize) as u32;
                if src == 0 {
                    continue;
                }
                let prio = self.priority[src as usize];
                if prio > self.threshold[ctx] && best.is_none_or(|b| prio > b.0) {
                    best = Some((prio, src));
                }
            }
        }
        best
    }

    /// context 的中断线电平（→ CPU 的 MEIP / SEIP）
    pub fn irq_level(&self, ctx: usize) -> bool {
        self.best(ctx).is_some()
    }

    /// 认领（读 claim 寄存器）：清挂起、置网关占用，返回源号（无则 0）。
    pub fn claim(&mut self, ctx: usize) -> u32 {
        match self.best(ctx) {
            Some((_, src)) => {
                self.pending[(src / 32) as usize] &= !(1 << (src % 32));
                self.in_service[(src / 32) as usize] |= 1 << (src % 32);
                src
            }
            None => 0,
        }
    }

    /// 完成（写 claim 寄存器）：清网关占用。之后若线仍高（重新上沿）
    /// 会再次通知。规范：对未认领源的 complete 写被忽略；源 0 忽略。
    pub fn complete(&mut self, src: u32) {
        if src > 0 && (src as usize) < PLIC_SOURCES {
            let bit = 1 << (src % 32);
            if self.in_service[(src / 32) as usize] & bit != 0 {
                self.in_service[(src / 32) as usize] &= !bit;
            }
        }
    }

    /// 读 PLIC 寄存器。off 相对 PLIC 基址；`claim` 为 true 表示本次读取
    /// 命中 claim 寄存器（需要网关副作用），由总线层判定后调用。
    pub fn load(&mut self, off: u64, size: u32, claim: bool) -> u64 {
        let val: u32 = if off < 0x1000 {
            // 源优先级（源 0 保留）
            let src = (off / 4) as usize;
            if off.is_multiple_of(4) && src < PLIC_SOURCES {
                self.priority[src]
            } else {
                0
            }
        } else if (0x1000..0x2000).contains(&off) {
            // 挂起位图（只读）
            let word = ((off - 0x1000) / 4) as usize;
            self.pending.get(word).copied().unwrap_or(0)
        } else if (0x2000..0x20_0000).contains(&off) {
            // 使能位图：ctx = (off - 0x2000) / 0x80
            let rel = off - 0x2000;
            let ctx = (rel / 0x80) as usize;
            let word = ((rel % 0x80) / 4) as usize;
            if ctx < 2 { self.enable[ctx].get(word).copied().unwrap_or(0) } else { 0 }
        } else if (0x20_0000..0x20_2000).contains(&off) {
            // context 寄存器：+0 阈值，+4 claim
            let rel = off - 0x20_0000;
            let ctx = (rel / 0x1000) as usize;
            match (ctx < 2, rel % 0x1000, claim) {
                (true, 0, _) => self.threshold[ctx],
                (true, 4, true) => self.claim(ctx),
                _ => 0,
            }
        } else {
            0
        };
        match size {
            4 => val as u64,
            1 => (val & 0xFF) as u64,
            2 => (val & 0xFFFF) as u64,
            _ => 0,
        }
    }

    pub fn store(&mut self, off: u64, val: u64) {
        let val = val as u32;
        if off < 0x1000 {
            let src = (off / 4) as usize;
            if off.is_multiple_of(4) && src > 0 && src < PLIC_SOURCES {
                // 3 位优先级 WARL（QEMU：value % 8）
                self.priority[src] = val & 0x7;
            }
        } else if (0x2000..0x20_0000).contains(&off) {
            let rel = off - 0x2000;
            let ctx = (rel / 0x80) as usize;
            let word = ((rel % 0x80) / 4) as usize;
            if ctx < 2
                && rel.is_multiple_of(4)
                && let Some(w) = self.enable[ctx].get_mut(word)
            {
                *w = val;
            }
        } else if (0x20_0000..0x20_2000).contains(&off) {
            let rel = off - 0x20_0000;
            let ctx = (rel / 0x1000) as usize;
            let reg = rel % 0x1000;
            if ctx < 2 && reg == 0 {
                self.threshold[ctx] = val & 0x7;
            } else if ctx < 2 && reg == 4 {
                self.complete(val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latch_and_gateway() {
        let mut p = Plic::default();
        p.enable[0][0] = 1 << UART_IRQ;
        p.priority[UART_IRQ as usize] = 1;
        // 上沿锁存 → 通知
        p.sync(IrqLines { uart: true });
        assert!(p.irq_level(0));
        // claim → 网关占用，不再通知（即使线保持高：无新上沿不重锁存）
        assert_eq!(p.claim(0), UART_IRQ);
        p.sync(IrqLines { uart: true });
        assert!(!p.irq_level(0), "claim 后 complete 前不应再通知");
        // 设备重断言（如 RBR 弹出后 FIFO 仍非空）→ complete 后再次通知
        p.complete(UART_IRQ);
        assert!(!p.irq_level(0), "线保持高且无新事件不应重新通知");
        p.latch(UART_IRQ);
        assert!(p.irq_level(0));
        // 设备撤线再上沿 → 重新通知；仅撤线则通知撤销
        assert_eq!(p.claim(0), UART_IRQ);
        p.sync(IrqLines { uart: false });
        p.complete(UART_IRQ);
        assert!(!p.irq_level(0));
        assert_eq!(p.claim(0), 0);
        p.sync(IrqLines { uart: true });
        assert!(p.irq_level(0), "撤线后的新上沿应重新通知");
    }

    #[test]
    fn threshold_and_priority() {
        let mut p = Plic::default();
        p.enable[0][0] = 1 << UART_IRQ;
        p.priority[UART_IRQ as usize] = 1;
        p.sync(IrqLines { uart: true });
        // 阈值 7 = 最高优先级也被屏蔽（Linux 的 PLIC_DISABLE_THRESHOLD）
        p.threshold[0] = 7;
        assert!(!p.irq_level(0));
        p.threshold[0] = 6;
        p.priority[UART_IRQ as usize] = 7;
        assert!(p.irq_level(0));
        // 同优先级取小号（阈值回落到 0）
        p.threshold[0] = 0;
        p.priority[UART_IRQ as usize] = 3;
        p.enable[0][0] |= 1 << 5;
        p.pending[0] |= 1 << 5;
        p.priority[5] = 3;
        assert_eq!(p.claim(0), 5);
    }

    #[test]
    fn contexts_are_independent() {
        let mut p = Plic::default();
        p.enable[1][0] = 1 << UART_IRQ; // 只开 S context
        p.priority[UART_IRQ as usize] = 1;
        p.sync(IrqLines { uart: true });
        assert!(p.irq_level(1));
        assert!(!p.irq_level(0));
        assert_eq!(p.claim(1), UART_IRQ);
        assert_eq!(p.claim(0), 0, "M context 未使能");
        p.complete(UART_IRQ);
    }

    #[test]
    fn complete_of_unclaimed_is_ignored() {
        let mut p = Plic::default();
        p.enable[0][0] = 1 << UART_IRQ;
        p.priority[UART_IRQ as usize] = 1;
        // 未 claim 直接 complete：忽略（不影响网关）
        p.complete(UART_IRQ);
        // 正常流程：锁存 → claim → complete → 线撤销后不再通知
        p.sync(IrqLines { uart: true });
        assert_eq!(p.claim(0), UART_IRQ);
        assert!(!p.irq_level(0));
        p.complete(UART_IRQ);
        p.sync(IrqLines { uart: false });
        assert!(!p.irq_level(0));
    }

    #[test]
    fn priority_masked_to_3_bits() {
        let mut p = Plic::default();
        p.store(4 * UART_IRQ as u64, 0xFF);
        assert_eq!(p.priority[UART_IRQ as usize], 0x7);
    }
}
