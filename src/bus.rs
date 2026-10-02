//! 系统总线：把物理地址路由到 RAM 或 MMIO 设备。
//! 布局对齐 QEMU `virt` 机器，方便将来直接启动 OpenSBI/Linux。

use crate::devices::{Clint, IrqLines, Plic, TestDevice, Uart, UART_IRQ};
use crate::exception::Exception;

pub const DRAM_BASE: u64 = 0x8000_0000;
pub const TEST_BASE: u64 = 0x0010_0000;
pub const TEST_END: u64 = TEST_BASE + 0x1000;
pub const CLINT_BASE: u64 = 0x0200_0000;
pub const CLINT_END: u64 = CLINT_BASE + 0x0001_0000;
pub const PLIC_BASE: u64 = 0x0C00_0000;
pub const PLIC_END: u64 = PLIC_BASE + 0x0040_0000;
pub const UART_BASE: u64 = 0x1000_0000;
pub const UART_END: u64 = UART_BASE + 0x100;

pub struct Bus {
    pub dram: Vec<u8>,
    pub uart: Uart,
    pub clint: Clint,
    pub plic: Plic,
    pub test: TestDevice,
}

impl Bus {
    pub fn new(dram_size: usize) -> Self {
        Bus {
            dram: vec![0; dram_size],
            uart: Uart::new(),
            clint: Clint::new(),
            plic: Plic::default(),
            test: TestDevice::new(),
        }
    }

    pub fn dram_end(&self) -> u64 {
        DRAM_BASE + self.dram.len() as u64
    }

    /// 把一段数据写入 RAM（ELF 加载用），越界返回 false。
    pub fn write_dram(&mut self, addr: u64, data: &[u8]) -> bool {
        let Some(off) = addr.checked_sub(DRAM_BASE) else {
            return false;
        };
        let end = match off.checked_add(data.len() as u64) {
            Some(e) => e as usize,
            None => return false,
        };
        if end > self.dram.len() {
            return false;
        }
        self.dram[off as usize..end].copy_from_slice(data);
        true
    }

    /// 刷新 PLIC 挂起锁存（设备线电平 → 上沿置位）。由运行循环每步调用。
    pub fn plic_sync(&mut self) {
        let lines = IrqLines {
            uart: self.uart.irq_line(),
        };
        self.plic.sync(lines);
    }

    pub fn load(&mut self, addr: u64, size: u32) -> Result<u64, Exception> {
        if let Some(off) = addr.checked_sub(DRAM_BASE) {
            let end = off as usize + size as usize;
            if end <= self.dram.len() {
                return Ok(self.dram_read(off as usize, size));
            }
        }
        match addr {
            CLINT_BASE..CLINT_END => Ok(self.clint.load(addr - CLINT_BASE, size)),
            PLIC_BASE..PLIC_END => {
                let rel = addr - PLIC_BASE;
                // claim 寄存器（context + 4）读取即认领，需网关副作用
                let claim =
                    (0x20_0000..0x20_2000).contains(&rel) && rel % 0x1000 == 4 && size == 4;
                Ok(self.plic.load(rel, size, claim))
            }
            UART_BASE..UART_END => {
                let v = self.uart.read(addr - UART_BASE) as u64;
                // 设备状态变化后的重断言：RBR 弹出后 FIFO 仍非空等场景
                // （对齐 QEMU 串口每次状态变化后重发 qemu_set_irq）
                if self.uart.irq_line() {
                    self.plic.latch(UART_IRQ);
                }
                Ok(v)
            }
            TEST_BASE..TEST_END => Ok(0),
            _ => Err(Exception::LoadAccessFault),
        }
    }

    pub fn store(&mut self, addr: u64, size: u32, val: u64) -> Result<(), Exception> {
        if let Some(off) = addr.checked_sub(DRAM_BASE) {
            let end = off as usize + size as usize;
            if end <= self.dram.len() {
                self.dram_write(off as usize, size, val);
                return Ok(());
            }
        }
        match addr {
            CLINT_BASE..CLINT_END => self.clint.store(addr - CLINT_BASE, size, val),
            PLIC_BASE..PLIC_END => self.plic.store(addr - PLIC_BASE, val),
            UART_BASE..UART_END => {
                self.uart.write(addr - UART_BASE, val as u8);
                if self.uart.irq_line() {
                    self.plic.latch(UART_IRQ);
                }
            }
            TEST_BASE..TEST_END => self.test.store(val),
            _ => return Err(Exception::StoreAccessFault),
        }
        Ok(())
    }

    fn dram_read(&self, off: usize, size: u32) -> u64 {
        match size {
            1 => self.dram[off] as u64,
            2 if off.is_multiple_of(2) => u16::from_le_bytes(self.dram[off..off + 2].try_into().unwrap()) as u64,
            4 if off.is_multiple_of(4) => u32::from_le_bytes(self.dram[off..off + 4].try_into().unwrap()) as u64,
            8 if off.is_multiple_of(8) => u64::from_le_bytes(self.dram[off..off + 8].try_into().unwrap()),
            _ => {
                let mut v = 0u64;
                for i in 0..size as usize {
                    v |= (self.dram[off + i] as u64) << (i * 8);
                }
                v
            }
        }
    }

    fn dram_write(&mut self, off: usize, size: u32, val: u64) {
        match size {
            1 => self.dram[off] = val as u8,
            2 if off.is_multiple_of(2) => self.dram[off..off + 2].copy_from_slice(&(val as u16).to_le_bytes()),
            4 if off.is_multiple_of(4) => self.dram[off..off + 4].copy_from_slice(&(val as u32).to_le_bytes()),
            8 if off.is_multiple_of(8) => self.dram[off..off + 8].copy_from_slice(&val.to_le_bytes()),
            _ => {
                for i in 0..size as usize {
                    self.dram[off + i] = (val >> (i * 8)) as u8;
                }
            }
        }
    }
}
