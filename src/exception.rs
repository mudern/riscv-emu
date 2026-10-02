//! RISC-V 异常（cause 编码见特权规范 mcause/scause 表）。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    InstructionMisaligned,
    InstructionAccessFault,
    IllegalInstruction,
    Breakpoint,
    LoadMisaligned,
    LoadAccessFault,
    StoreMisaligned,
    StoreAccessFault,
    EcallFromU,
    EcallFromS,
    EcallFromM,
    InstructionPageFault,
    LoadPageFault,
    StorePageFault,
}

impl Exception {
    pub fn code(self) -> u64 {
        match self {
            Exception::InstructionMisaligned => 0,
            Exception::InstructionAccessFault => 1,
            Exception::IllegalInstruction => 2,
            Exception::Breakpoint => 3,
            Exception::LoadMisaligned => 4,
            Exception::LoadAccessFault => 5,
            Exception::StoreMisaligned => 6,
            Exception::StoreAccessFault => 7,
            Exception::EcallFromU => 8,
            Exception::EcallFromS => 9,
            Exception::EcallFromM => 11,
            Exception::InstructionPageFault => 12,
            Exception::LoadPageFault => 13,
            Exception::StorePageFault => 15,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Exception::InstructionMisaligned => "instruction-address-misaligned",
            Exception::InstructionAccessFault => "instruction-access-fault",
            Exception::IllegalInstruction => "illegal-instruction",
            Exception::Breakpoint => "breakpoint",
            Exception::LoadMisaligned => "load-address-misaligned",
            Exception::LoadAccessFault => "load-access-fault",
            Exception::StoreMisaligned => "store/amo-address-misaligned",
            Exception::StoreAccessFault => "store/amo-access-fault",
            Exception::EcallFromU => "ecall-from-u-mode",
            Exception::EcallFromS => "ecall-from-s-mode",
            Exception::EcallFromM => "ecall-from-m-mode",
            Exception::InstructionPageFault => "instruction-page-fault",
            Exception::LoadPageFault => "load-page-fault",
            Exception::StorePageFault => "store/amo-page-fault",
        }
    }
}

/// 未能交付给客户机 trap handler 的异常（guest 没有对应的 mtvec/stvec）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapInfo {
    pub pc: u64,
    pub cause: Exception,
    pub val: u64,
}
