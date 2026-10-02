//! 整机：CPU + 总线 + 控制台。

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::csr;
use crate::exception::TrapInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// guest 主动退出（ecall exit 或 test 设备）
    Exit(i32),
    /// 无法交付的异常
    Trap(TrapInfo),
    /// 超过最大指令数（防死循环）
    Timeout,
}

pub struct Machine {
    pub cpu: Cpu,
    pub bus: Bus,
    /// ecall write 的输出记录（同时也会打印）
    pub console: Vec<u8>,
}

impl Machine {
    pub fn new(dram_mb: usize) -> Self {
        let bus = Bus::new(dram_mb * 1024 * 1024);
        let mut machine = Machine {
            cpu: Cpu::new(crate::bus::DRAM_BASE),
            bus,
            console: Vec::new(),
        };
        // 给未初始化 sp 的程序一个兜底栈顶（指向 RAM 顶端，向低地址增长）
        machine.cpu.regs[2] = machine.bus.dram_end();
        machine
    }

    pub fn run(&mut self, max_insts: u64) -> Halt {
        let mut remaining = max_insts;
        while remaining > 0 {
            // 同步外部中断线到 mip（软件可写位不受影响）：
            //   CLINT msip → MSIP，CLINT mtime/mtimecmp → MTIP
            //   PLIC M context → MEIP，S context → ext_mip.SEIP（与软件位合并）
            let mip = &mut self.cpu.csr.mip;
            *mip &= !(csr::MSIP | csr::MTIP | csr::MEIP);
            if self.bus.clint.msip {
                *mip |= csr::MSIP;
            }
            if self.bus.clint.timer_pending() {
                *mip |= csr::MTIP;
            }
            self.bus.plic_sync();
            let mut ext_mip = 0;
            if self.bus.plic.irq_level(0) {
                ext_mip |= csr::MEIP;
            }
            if self.bus.plic.irq_level(1) {
                ext_mip |= csr::SEIP;
            }
            self.cpu.ext_mip = ext_mip;
            // 中断投递也计入预算，防止无法清除的挂起中断导致死循环
            if self.cpu.take_pending_interrupt() {
                remaining -= 1;
                continue;
            }
            if let Some(code) = self.bus.test.exit {
                return Halt::Exit(code);
            }
            if let Some(code) = self.cpu.exit {
                return Halt::Exit(code);
            }
            match self.cpu.step(&mut self.bus, &mut self.console) {
                Ok(()) => {}
                Err(t) => return Halt::Trap(t),
            }
            remaining -= 1;
        }
        Halt::Timeout
    }
}
