//! 整机：CPU + 总线 + 控制台。

use crate::bus::Bus;
use crate::cpu::Cpu;
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
        for _ in 0..max_insts {
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
        }
        Halt::Timeout
    }
}
