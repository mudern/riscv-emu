//! RISC-V 异常（cause 编码见特权规范 Table "mcause"）。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    InstructionMisaligned,
    InstructionAccessFault,
    IllegalInstruction,
    Breakpoint,
    LoadAccessFault,
    StoreAccessFault,
}

impl Exception {
    pub fn code(self) -> u64 {
        match self {
            Exception::InstructionMisaligned => 0,
            Exception::InstructionAccessFault => 1,
            Exception::IllegalInstruction => 2,
            Exception::Breakpoint => 3,
            Exception::LoadAccessFault => 5,
            Exception::StoreAccessFault => 7,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Exception::InstructionMisaligned => "instruction-address-misaligned",
            Exception::InstructionAccessFault => "instruction-access-fault",
            Exception::IllegalInstruction => "illegal-instruction",
            Exception::Breakpoint => "breakpoint",
            Exception::LoadAccessFault => "load-access-fault",
            Exception::StoreAccessFault => "store-access-fault",
        }
    }
}

/// 未能交付给客户机 trap handler 的异常（通常意味着 guest 没有 mtvec）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapInfo {
    pub pc: u64,
    pub cause: Exception,
    pub val: u64,
}
