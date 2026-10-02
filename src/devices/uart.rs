//! NS16550 UART（virt 机器布局在 0x1000_0000，时钟 3.6864MHz）。
//!
//! 发送：写 THR 输出。接收：`receive()` 投递字节进 FIFO，LSR.DR 置位，
//! 配合 IER.RX 可产生 PLIC 中断（源 10）。寄存器子集覆盖 8250 驱动
//! autoconfig 所需：IER/IIR/FCR/LCR/MCR/LSR/MSR/SCR + 波特率除数。
//!
//! 中断线（`irq_line`）：RX 数据可用（IER bit0）或 THRE 挂起（IER bit1）。
//! THRE 中断读 IIR 即清除（16550 语义）。

#[derive(Default)]
pub struct Uart {
    pub output: Vec<u8>,
    /// 为 false 时只记录不打印（测试用）
    pub print: bool,

    ier: u8,
    /// scratch（autoconfig 探测用）
    scr: u8,
    lcr: u8,
    dll: u8,
    dlm: u8,
    /// FCR bit0：FIFO 使能（仅影响 IIR 报告）
    fifo: bool,
    /// RX FIFO：16550 标称 16 字节
    rx: std::collections::VecDeque<u8>,
    /// overrun 标记（一次读 LSR 后清除）
    overrun: bool,
    /// THRE 中断挂起（读 IIR 清除；写 THR 置位）。复位时 THR 恒空，
    /// 因此使能 IER.1 即挂起，与真实 16550 一致。
    thre_ip: bool,
}

const RX_FIFO_CAP: usize = 16;

impl Uart {
    pub fn new() -> Self {
        Uart {
            output: Vec::new(),
            print: true,
            thre_ip: true,
            ..Uart::default()
        }
    }

    /// 接收一个字节（宿主 stdin / 测试注入）。FIFO 满时丢弃并置 overrun。
    pub fn receive(&mut self, b: u8) {
        if self.rx.len() >= RX_FIFO_CAP {
            self.overrun = true;
        } else {
            self.rx.push_back(b);
        }
    }

    /// 中断线电平（→ PLIC 源 10）
    pub fn irq_line(&self) -> bool {
        (self.ier & 0x01 != 0 && !self.rx.is_empty()) || (self.ier & 0x02 != 0 && self.thre_ip)
    }

    fn dlab(&self) -> bool {
        self.lcr & 0x80 != 0
    }

    pub fn read(&mut self, off: u64) -> u8 {
        match off {
            0 => {
                if self.dlab() {
                    self.dll
                } else {
                    let b = self.rx.pop_front().unwrap_or(0);
                    // FIFO 空时 THRE 侧也置位（发送器恒空）
                    if self.rx.is_empty() {
                        self.thre_ip = true;
                    }
                    b
                }
            }
            1 => {
                if self.dlab() {
                    self.dlm
                } else {
                    self.ier
                }
            }
            2 => {
                // IIR：bit0=0 表示有挂起中断；原因优先级 RX > THRE
                let rx = self.ier & 0x01 != 0 && !self.rx.is_empty();
                if rx {
                    0x04 | self.fifo_ctrl() << 6 // received data available
                } else if self.ier & 0x02 != 0 && self.thre_ip {
                    self.thre_ip = false; // 读 IIR 清除 THRE 中断
                    0x02 | self.fifo_ctrl() << 6 // THR empty
                } else {
                    0x01 | self.fifo_ctrl() << 6 // 无挂起
                }
            }
            3 => self.lcr,
            4 => 0, // MCR
            5 => {
                let mut lsr = 0x60u8; // THRE | TEMT（发送器恒空）
                if !self.rx.is_empty() {
                    lsr |= 0x01; // DR
                }
                if std::mem::take(&mut self.overrun) {
                    lsr |= 0x02; // OE
                }
                lsr
            }
            7 => self.scr,
            _ => 0,
        }
    }

    fn fifo_ctrl(&self) -> u8 {
        self.fifo as u8 & 0x01
    }

    pub fn write(&mut self, off: u64, val: u8) {
        match off {
            0 => {
                if self.dlab() {
                    self.dll = val;
                } else {
                    self.output.push(val);
                    self.thre_ip = true;
                    if self.print {
                        use std::io::Write;
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(&[val]);
                        let _ = out.flush();
                    }
                }
            }
            1 => {
                if self.dlab() {
                    self.dlm = val;
                } else {
                    self.ier = val & 0x0F;
                }
            }
            2 => {
                self.fifo = val & 0x01 != 0;
            }
            3 => self.lcr = val,
            4 => {} // MCR 忽略
            7 => self.scr = val,
            _ => {}
        }
    }
}
