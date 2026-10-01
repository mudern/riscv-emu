use std::io::{self, Write};

/// NS16550 UART（virt 机器布局在 0x1000_0000）。
/// 阶段 1 只实现发送：写 THR 输出一个字节；LSR 恒报告可发送、无输入。
#[derive(Default)]
pub struct Uart {
    pub output: Vec<u8>,
    /// 为 false 时只记录不打印（测试用）
    pub print: bool,
}

impl Uart {
    pub fn new() -> Self {
        Uart {
            output: Vec::new(),
            print: true,
        }
    }

    pub fn read(&self, off: u64) -> u8 {
        match off {
            5 => 0x60, // LSR: THRE | TEMT，可继续写
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u64, val: u8) {
        if off == 0 {
            self.output.push(val);
            if self.print {
                let mut out = io::stdout().lock();
                let _ = out.write_all(&[val]);
                let _ = out.flush();
            }
        }
        // IER/FCR/LCR/MCR/MSR 等忽略
    }
}
