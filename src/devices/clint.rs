use std::time::Instant;

/// CLINT（virt 机器布局在 0x0200_0000）。
/// 阶段 1 只提供 mtime 读取（频率 10MHz，与 virt 机器一致）；
/// mtimecmp 可写但暂不产生定时器中断（后续特权级支持时补上）。
pub struct Clint {
    pub mtimecmp: u64,
    /// msip hart 0（CLINT 偏移 0x0000）：软件中断，运行循环同步到 mip.MSIP
    pub msip: bool,
    start: Instant,
}

pub const TIMEBASE_FREQ: u64 = 10_000_000;

impl Default for Clint {
    fn default() -> Self {
        Self::new()
    }
}

impl Clint {
    pub fn new() -> Self {
        Clint {
            mtimecmp: 0,
            msip: false,
            start: Instant::now(),
        }
    }

    pub fn mtime(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64 * TIMEBASE_FREQ / 1_000_000_000
    }

    /// 定时器中断是否挂起（mtime >= mtimecmp，符合硬件语义：复位后即挂起）
    pub fn timer_pending(&self) -> bool {
        self.mtime() >= self.mtimecmp
    }

    /// 取消挂起（写一个很远的到期时间）
    pub fn clear_timer(&mut self) {
        self.mtimecmp = u64::MAX;
    }

    /// size 字节的读取，offset 以 CLINT 基址为原点。
    pub fn load(&self, off: u64, size: u32) -> u64 {
        let qword = match off & !7 {
            0x0000 => self.msip as u64, // msip hart0：bit0 有效
            0x4000 => self.mtimecmp,
            0xBFF8 => self.mtime(),
            _ => 0,
        };
        let shift = (off & 7) * 8;
        let mask = if size >= 8 { u64::MAX } else { (1u64 << (size * 8)) - 1 };
        (qword >> shift) & mask
    }

    pub fn store(&mut self, off: u64, size: u32, val: u64) {
        match off & !7 {
            0x0000 => self.msip = val & 1 != 0,
            0x4000 => {
                let shift = (off & 7) * 8;
                let mask = if size >= 8 {
                    u64::MAX
                } else {
                    (1u64 << (size * 8)) - 1
                } << shift;
                self.mtimecmp = (self.mtimecmp & !mask) | ((val << shift) & mask);
            }
            _ => {}
        }
    }
}
