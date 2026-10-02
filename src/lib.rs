//! riscv-emu：RISC-V 全系统模拟器（目标是逐步发展到能启动 Linux + busybox）。
//!
//! 当前（阶段 1）：RV64IMAC + Zicsr，仅 M-mode，内存布局对齐 QEMU `virt` 机器。
//! 运行裸机 ELF：M-mode `ecall` 按阶段 1 约定实现 write/exit，UART 输出可直接打印。

pub mod bus;
pub mod cpu;
pub mod csr;
pub mod decode;
pub mod devices;
pub mod elf;
pub mod exception;
pub mod fpu;
pub mod machine;
pub mod mmu;
pub mod pmp;

pub use bus::{Bus, DRAM_BASE};
pub use cpu::{Cpu, Privilege};
pub use csr::Csrs;
pub use devices::UART_IRQ;
pub use exception::{Exception, TrapInfo};
pub use machine::{Halt, Machine};
