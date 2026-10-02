//! RV64IMAC + Zicsr 指令解码。
//! 16 位压缩指令直接解码为等价的规范形式（如 c.addi -> OpImm{Add}）。

use crate::exception::Exception;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Addw,
    Subw,
    Sllw,
    Srlw,
    Sraw,
    Mul,
    Mulh,
    Mulhsu,
    Mulhu,
    Div,
    Divu,
    Rem,
    Remu,
    Mulw,
    Divw,
    Divuw,
    Remw,
    Remuw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchOp {
    Eq,
    Ne,
    Lt,
    Ge,
    Ltu,
    Geu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOp {
    B,
    Bu,
    H,
    Hu,
    W,
    Wu,
    D,
}

impl LoadOp {
    pub fn size(self) -> u32 {
        match self {
            LoadOp::B | LoadOp::Bu => 1,
            LoadOp::H | LoadOp::Hu => 2,
            LoadOp::W | LoadOp::Wu => 4,
            LoadOp::D => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOp {
    B,
    H,
    W,
    D,
}

impl StoreOp {
    pub fn size(self) -> u32 {
        match self {
            StoreOp::B => 1,
            StoreOp::H => 2,
            StoreOp::W => 4,
            StoreOp::D => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmoOp {
    Lr,
    Sc,
    Swap,
    Add,
    Xor,
    And,
    Or,
    Min,
    Max,
    Minu,
    Maxu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsrKind {
    Rw,
    Rs,
    Rc,
}

/// 浮点格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    S,
    D,
}

impl Fmt {
    fn from_f3(funct3: u32) -> Result<Fmt, Exception> {
        match funct3 {
            0 => Ok(Fmt::S),
            1 => Ok(Fmt::D),
            _ => Err(Exception::IllegalInstruction),
        }
    }
}

/// R4 型（乘加）选择
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FMulAdd {
    Add,
    Sub,
    NAdd,
    NAddNeg, // fnmadd：-(a*b+c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FOp {
    /// FADD/FSUB/FMUL/FDIV/FSQRT（Sqrt 时 rs2=0）
    Arith {
        op: crate::fpu::FArith,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rs2: u8,
        rm: u64,
    },
    /// 乘加：rd = ±(rs1*rs2 ± rs3)（单舍入）
    MulAdd {
        kind: FMulAdd,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rs2: u8,
        rs3: u8,
        rm: u64,
    },
    /// 符号注入：neg = 取反、xor = 异或（FSGNJ/FSGNJN/FSGNJX）
    Sgnj {
        neg: bool,
        xor: bool,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    /// FMIN/FMAX
    MinMax {
        max: bool,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    /// 比较：0=FLE 1=FLT 2=FEQ（结果写整数 rd）
    Cmp {
        kind: u8,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    /// 浮点 → 整数（signed/W-L）
    Cvtf2i {
        signed: bool,
        is32: bool,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rm: u64,
    },
    /// 整数 → 浮点
    Cvti2f {
        signed: bool,
        src32: bool,
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        rm: u64,
    },
    /// 格式转换（S↔D）
    Cvtf2f {
        to64: bool,
        rd: u8,
        rs1: u8,
        rm: u64,
    },
    /// fmv.x.w / fmv.x.d（浮点位型直拷到整数）
    FmvXf {
        fmt: Fmt,
        rd: u8,
        rs1: u8,
    },
    /// fmv.w.x / fmv.d.x
    FmvFx {
        fmt: Fmt,
        rd: u8,
        rs1: u8,
    },
    Fclass {
        fmt: Fmt,
        rd: u8,
        rs1: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOp {
    Ecall,
    Ebreak,
    Mret,
    Sret,
    Wfi,
    SfenceVma,
    Csr {
        kind: CsrKind,
        /// true 表示 CSRRWI/CSRRSI/CSRRCI（rs1 字段是 zimm）
        imm: bool,
        csr: u16,
        rd: u8,
        rs1: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inst {
    Lui {
        rd: u8,
        imm: i64,
    },
    Auipc {
        rd: u8,
        imm: i64,
    },
    Jal {
        rd: u8,
        imm: i64,
    },
    Jalr {
        rd: u8,
        rs1: u8,
        imm: i64,
    },
    Branch {
        op: BranchOp,
        rs1: u8,
        rs2: u8,
        imm: i64,
    },
    Load {
        op: LoadOp,
        rd: u8,
        rs1: u8,
        imm: i64,
    },
    Store {
        op: StoreOp,
        rs1: u8,
        rs2: u8,
        imm: i64,
    },
    OpImm {
        op: AluOp,
        rd: u8,
        rs1: u8,
        imm: i64,
    },
    Op {
        op: AluOp,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    Amo {
        op: AmoOp,
        /// true: .w（32 位），false: .d（64 位）
        w: bool,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    /// 浮点加载 FLW/FLD（rd 为浮点寄存器）
    FLoad {
        fmt: Fmt,
        rd: u8,
        rs1: u8,
        imm: i64,
    },
    /// 浮点存储 FSW/FSD（rs2 为浮点寄存器）
    FStore {
        fmt: Fmt,
        rs1: u8,
        rs2: u8,
        imm: i64,
    },
    Fp(FOp),
    System(SystemOp),
    Fence,
    FenceI,
    Nop,
}

fn sext(v: u64, bits: u32) -> i64 {
    debug_assert!(bits > 0 && bits < 64);
    let shift = 64 - bits;
    ((v << shift) as i64) >> shift
}

fn i_imm(w: u32) -> i64 {
    sext((w >> 20) as u64, 12)
}

fn s_imm(w: u32) -> i64 {
    let v = (((w >> 25) & 0x7F) << 5) | ((w >> 7) & 0x1F);
    sext(v as u64, 12)
}

fn b_imm(w: u32) -> i64 {
    let v = (((w >> 31) & 1) << 12)
        | (((w >> 7) & 1) << 11)
        | (((w >> 25) & 0x3F) << 5)
        | (((w >> 8) & 0xF) << 1);
    sext(v as u64, 13)
}

fn j_imm(w: u32) -> i64 {
    let v = (((w >> 31) & 1) << 20)
        | (((w >> 12) & 0xFF) << 12)
        | (((w >> 20) & 1) << 11)
        | (((w >> 21) & 0x3FF) << 1);
    sext(v as u64, 21)
}

/// 解码 32 位指令。出错返回 IllegalInstruction（tval 由调用者填原始指令字）。
pub fn decode(w: u32) -> Result<Inst, Exception> {
    let opcode = w & 0x7F;
    let rd = ((w >> 7) & 0x1F) as u8;
    let funct3 = (w >> 12) & 0x7;
    let rs1 = ((w >> 15) & 0x1F) as u8;
    let rs2 = ((w >> 20) & 0x1F) as u8;
    let funct7 = (w >> 25) & 0x7F;

    match opcode {
        0x37 => Ok(Inst::Lui {
            rd,
            imm: sext((w & 0xFFFF_F000) as u64, 32),
        }),
        0x17 => Ok(Inst::Auipc {
            rd,
            imm: sext((w & 0xFFFF_F000) as u64, 32),
        }),
        0x6F => Ok(Inst::Jal { rd, imm: j_imm(w) }),
        0x67 => {
            if funct3 != 0 {
                return Err(Exception::IllegalInstruction);
            }
            Ok(Inst::Jalr { rd, rs1, imm: i_imm(w) })
        }
        0x63 => {
            let op = match funct3 {
                0 => BranchOp::Eq,
                1 => BranchOp::Ne,
                4 => BranchOp::Lt,
                5 => BranchOp::Ge,
                6 => BranchOp::Ltu,
                7 => BranchOp::Geu,
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::Branch {
                op,
                rs1,
                rs2,
                imm: b_imm(w),
            })
        }
        0x03 => {
            let op = match funct3 {
                0 => LoadOp::B,
                1 => LoadOp::H,
                2 => LoadOp::W,
                3 => LoadOp::D,
                4 => LoadOp::Bu,
                5 => LoadOp::Hu,
                6 => LoadOp::Wu,
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::Load {
                op,
                rd,
                rs1,
                imm: i_imm(w),
            })
        }
        0x23 => {
            let op = match funct3 {
                0 => StoreOp::B,
                1 => StoreOp::H,
                2 => StoreOp::W,
                3 => StoreOp::D,
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::Store {
                op,
                rs1,
                rs2,
                imm: s_imm(w),
            })
        }
        0x13 => {
            // OP-IMM
            let (op, imm) = match funct3 {
                0 => (AluOp::Add, i_imm(w)),
                2 => (AluOp::Slt, i_imm(w)),
                3 => (AluOp::Sltu, i_imm(w)),
                4 => (AluOp::Xor, i_imm(w)),
                6 => (AluOp::Or, i_imm(w)),
                7 => (AluOp::And, i_imm(w)),
                1 => {
                    if (w >> 26) != 0 {
                        return Err(Exception::IllegalInstruction);
                    }
                    (AluOp::Sll, ((w >> 20) & 0x3F) as i64)
                }
                _ => match w >> 26 {
                    0b000000 => (AluOp::Srl, ((w >> 20) & 0x3F) as i64),
                    0b010000 => (AluOp::Sra, ((w >> 20) & 0x3F) as i64),
                    _ => return Err(Exception::IllegalInstruction),
                },
            };
            Ok(Inst::OpImm { op, rd, rs1, imm })
        }
        0x1B => {
            // OP-IMM-32
            let (op, imm) = match funct3 {
                0 => (AluOp::Addw, i_imm(w)),
                1 if funct7 == 0 => (AluOp::Sllw, (rs2 as u64) as i64),
                5 if funct7 == 0 => (AluOp::Srlw, (rs2 as u64) as i64),
                5 if funct7 == 0x20 => (AluOp::Sraw, (rs2 as u64) as i64),
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::OpImm { op, rd, rs1, imm })
        }
        0x33 => {
            // OP
            let op = if funct7 == 0x01 {
                match funct3 {
                    0 => AluOp::Mul,
                    1 => AluOp::Mulh,
                    2 => AluOp::Mulhsu,
                    3 => AluOp::Mulhu,
                    4 => AluOp::Div,
                    5 => AluOp::Divu,
                    6 => AluOp::Rem,
                    7 => AluOp::Remu,
                    _ => unreachable!(),
                }
            } else {
                match (funct3, funct7) {
                    (0, 0x00) => AluOp::Add,
                    (0, 0x20) => AluOp::Sub,
                    (1, 0x00) => AluOp::Sll,
                    (2, 0x00) => AluOp::Slt,
                    (3, 0x00) => AluOp::Sltu,
                    (4, 0x00) => AluOp::Xor,
                    (5, 0x00) => AluOp::Srl,
                    (5, 0x20) => AluOp::Sra,
                    (6, 0x00) => AluOp::Or,
                    (7, 0x00) => AluOp::And,
                    _ => return Err(Exception::IllegalInstruction),
                }
            };
            Ok(Inst::Op { op, rd, rs1, rs2 })
        }
        0x3B => {
            // OP-32
            let op = if funct7 == 0x01 {
                match funct3 {
                    0 => AluOp::Mulw,
                    4 => AluOp::Divw,
                    5 => AluOp::Divuw,
                    6 => AluOp::Remw,
                    7 => AluOp::Remuw,
                    _ => return Err(Exception::IllegalInstruction),
                }
            } else {
                match (funct3, funct7) {
                    (0, 0x00) => AluOp::Addw,
                    (0, 0x20) => AluOp::Subw,
                    (1, 0x00) => AluOp::Sllw,
                    (5, 0x00) => AluOp::Srlw,
                    (5, 0x20) => AluOp::Sraw,
                    _ => return Err(Exception::IllegalInstruction),
                }
            };
            Ok(Inst::Op { op, rd, rs1, rs2 })
        }
        0x2F => {
            // AMO
            let is_w = funct3 == 2;
            if !is_w && funct3 != 3 {
                return Err(Exception::IllegalInstruction);
            }
            let op = match w >> 27 {
                0x00 => AmoOp::Add,
                0x01 => AmoOp::Swap,
                0x02 if rs2 == 0 => AmoOp::Lr,
                0x03 => AmoOp::Sc,
                0x04 => AmoOp::Xor,
                0x08 => AmoOp::Or,
                0x0C => AmoOp::And,
                0x10 => AmoOp::Min,
                0x14 => AmoOp::Max,
                0x18 => AmoOp::Minu,
                0x1C => AmoOp::Maxu,
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::Amo { op, w: is_w, rd, rs1, rs2 })
        }
        0x07 => {
            // LOAD-FP：FLW(2) / FLD(3)
            let fmt = match funct3 {
                2 => Fmt::S,
                3 => Fmt::D,
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::FLoad {
                fmt,
                rd,
                rs1,
                imm: i_imm(w),
            })
        }
        0x27 => {
            // STORE-FP：FSW(2) / FSD(3)
            let fmt = match funct3 {
                2 => Fmt::S,
                3 => Fmt::D,
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::FStore {
                fmt,
                rs1,
                rs2,
                imm: s_imm(w),
            })
        }
        0x43 | 0x47 | 0x4B | 0x4F => {
            // FMADD/FMSUB/FNMSUB/FNMADD（R4 型）
            let fmt = Fmt::from_f3(funct3)?;
            let kind = match opcode {
                0x43 => FMulAdd::Add,
                0x47 => FMulAdd::Sub,
                0x4B => FMulAdd::NAdd,
                _ => FMulAdd::NAddNeg,
            };
            let rs3 = ((w >> 27) & 0x1F) as u8;
            Ok(Inst::Fp(FOp::MulAdd {
                kind,
                fmt,
                rd,
                rs1,
                rs2,
                rs3,
                rm: (funct7 & 7) as u64,
            }))
        }
        0x53 => {
            // OP-FP：fmt 在 funct7 位 0（S=0/D=1），位 1 为 Q/H（未实现）
            let fmt = if funct7 & 1 == 0 { Fmt::S } else { Fmt::D };
            if funct7 & 2 != 0 {
                return Err(Exception::IllegalInstruction);
            }
            let rm = funct3 as u64;
            let op = match funct7 >> 2 {
                0x00 => FOp::Arith {
                    op: crate::fpu::FArith::Add,
                    fmt,
                    rd,
                    rs1,
                    rs2,
                    rm,
                },
                0x01 => FOp::Arith {
                    op: crate::fpu::FArith::Sub,
                    fmt,
                    rd,
                    rs1,
                    rs2,
                    rm,
                },
                0x02 => FOp::Arith {
                    op: crate::fpu::FArith::Mul,
                    fmt,
                    rd,
                    rs1,
                    rs2,
                    rm,
                },
                0x03 => FOp::Arith {
                    op: crate::fpu::FArith::Div,
                    fmt,
                    rd,
                    rs1,
                    rs2,
                    rm,
                },
                0x16 => {
                    if rs2 != 0 {
                        return Err(Exception::IllegalInstruction);
                    }
                    FOp::Arith {
                        op: crate::fpu::FArith::Sqrt,
                        fmt,
                        rd,
                        rs1,
                        rs2: 0,
                        rm,
                    }
                }
                0x04 => {
                    let (neg, xor) = match funct3 {
                        0 => (false, false),
                        1 => (true, false),
                        2 => (false, true),
                        _ => return Err(Exception::IllegalInstruction),
                    };
                    FOp::Sgnj { neg, xor, fmt, rd, rs1, rs2 }
                }
                0x05 => {
                    if funct3 > 1 {
                        return Err(Exception::IllegalInstruction);
                    }
                    FOp::MinMax {
                        max: funct3 == 1,
                        fmt,
                        rd,
                        rs1,
                        rs2,
                    }
                }
                0x28 => {
                    if funct3 > 2 {
                        return Err(Exception::IllegalInstruction);
                    }
                    FOp::Cmp {
                        kind: funct3 as u8,
                        fmt,
                        rd,
                        rs1,
                        rs2,
                    }
                }
                0x08 => {
                    // FCVT.S.D（fmt=S, rs2=1）/ FCVT.D.S（fmt=D, rs2=0）
                    let to64 = match (fmt, rs2) {
                        (Fmt::S, 1) => false,
                        (Fmt::D, 0) => true,
                        _ => return Err(Exception::IllegalInstruction),
                    };
                    FOp::Cvtf2f { to64, rd, rs1, rm }
                }
                0x10 => FOp::Cvtf2i {
                    signed: rs2 == 0 || rs2 == 2,
                    is32: rs2 == 0 || rs2 == 1,
                    fmt,
                    rd,
                    rs1,
                    rm,
                },
                0x1A => FOp::Cvti2f {
                    signed: rs2 == 0 || rs2 == 2,
                    src32: rs2 == 0 || rs2 == 1,
                    fmt,
                    rd,
                    rs1,
                    rm,
                },
                0x1C => {
                    if rs2 != 0 {
                        return Err(Exception::IllegalInstruction);
                    }
                    if funct3 == 0 {
                        FOp::FmvXf { fmt, rd, rs1 }
                    } else if funct3 == 1 {
                        FOp::Fclass { fmt, rd, rs1 }
                    } else {
                        return Err(Exception::IllegalInstruction);
                    }
                }
                0x1E => {
                    if rs2 != 0 || funct3 != 0 {
                        return Err(Exception::IllegalInstruction);
                    }
                    FOp::FmvFx { fmt, rd, rs1 }
                }
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::Fp(op))
        }
        0x73 => {
            // SYSTEM
            if funct3 == 0 {
                if (w >> 25) == 0x09 {
                    return Ok(Inst::System(SystemOp::SfenceVma)); // rs1/rs2 字段含义忽略（无 TLB）
                }
                if rd != 0 || rs1 != 0 {
                    return Err(Exception::IllegalInstruction);
                }
                return match w >> 20 {
                    0 => Ok(Inst::System(SystemOp::Ecall)),
                    1 => Ok(Inst::System(SystemOp::Ebreak)),
                    0x302 => Ok(Inst::System(SystemOp::Mret)),
                    0x102 => Ok(Inst::System(SystemOp::Sret)),
                    0x105 => Ok(Inst::System(SystemOp::Wfi)),
                    _ => Err(Exception::IllegalInstruction),
                };
            }
            let (kind, imm) = match funct3 {
                1 => (CsrKind::Rw, false),
                2 => (CsrKind::Rs, false),
                3 => (CsrKind::Rc, false),
                5 => (CsrKind::Rw, true),
                6 => (CsrKind::Rs, true),
                7 => (CsrKind::Rc, true),
                _ => return Err(Exception::IllegalInstruction),
            };
            Ok(Inst::System(SystemOp::Csr {
                kind,
                imm,
                csr: (w >> 20) as u16,
                rd,
                rs1,
            }))
        }
        0x0F => match funct3 {
            0 => Ok(Inst::Fence),
            1 => Ok(Inst::FenceI),
            _ => Err(Exception::IllegalInstruction),
        },
        _ => Err(Exception::IllegalInstruction),
    }
}

/// 6 位符号扩展（c.addi / c.li / c.andi 等）
fn c_sext6(h: u16) -> i64 {
    sext(((((h >> 12) & 1) << 5) | ((h >> 2) & 0x1F)) as u64, 6)
}

fn cb_imm(h: u16) -> i64 {
    let v = (((h >> 12) & 1) << 8)
        | (((h >> 10) & 3) << 3)
        | (((h >> 5) & 3) << 6)
        | (((h >> 3) & 3) << 1)
        | (((h >> 2) & 1) << 5);
    sext(v as u64, 9)
}

fn cj_imm(h: u16) -> i64 {
    let v = (((h >> 12) & 1) << 11)
        | (((h >> 11) & 1) << 4)
        | (((h >> 9) & 3) << 8)
        | (((h >> 8) & 1) << 10)
        | (((h >> 7) & 1) << 6)
        | (((h >> 6) & 1) << 7)
        | (((h >> 3) & 7) << 1)
        | (((h >> 2) & 1) << 5);
    sext(v as u64, 12)
}

/// 解码 16 位压缩指令。
pub fn decode_compressed(h: u16) -> Result<Inst, Exception> {
    let quad = h & 3;
    let funct3 = (h >> 13) & 7;
    match quad {
        0 => match funct3 {
            0 => {
                // C.ADDI4SPN
                let imm = (((h >> 7) & 0xF) << 6)
                    | (((h >> 11) & 3) << 4)
                    | (((h >> 5) & 1) << 3)
                    | (((h >> 6) & 1) << 2);
                if imm == 0 {
                    return Err(Exception::IllegalInstruction);
                }
                Ok(Inst::OpImm {
                    op: AluOp::Add,
                    rd: 8 + ((h >> 2) & 7) as u8,
                    rs1: 2,
                    imm: imm as i64,
                })
            }
            2 => {
                // C.LW
                let imm = (((h >> 10) & 7) << 3) | (((h >> 6) & 1) << 2) | (((h >> 5) & 1) << 6);
                Ok(Inst::Load {
                    op: LoadOp::W,
                    rd: 8 + ((h >> 2) & 7) as u8,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    imm: imm as i64,
                })
            }
            3 => {
                // C.LD
                let imm = (((h >> 10) & 7) << 3) | (((h >> 5) & 3) << 6);
                Ok(Inst::Load {
                    op: LoadOp::D,
                    rd: 8 + ((h >> 2) & 7) as u8,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    imm: imm as i64,
                })
            }
            6 => {
                // C.SW
                let imm = (((h >> 10) & 7) << 3) | (((h >> 6) & 1) << 2) | (((h >> 5) & 1) << 6);
                Ok(Inst::Store {
                    op: StoreOp::W,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    rs2: 8 + ((h >> 2) & 7) as u8,
                    imm: imm as i64,
                })
            }
            7 => {
                // C.SD
                let imm = (((h >> 10) & 7) << 3) | (((h >> 5) & 3) << 6);
                Ok(Inst::Store {
                    op: StoreOp::D,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    rs2: 8 + ((h >> 2) & 7) as u8,
                    imm: imm as i64,
                })
            }
            1 => {
                // C.FLD（CL 格式同 C.LD）
                let imm = (((h >> 10) & 7) << 3) | (((h >> 5) & 3) << 6);
                Ok(Inst::FLoad {
                    fmt: Fmt::D,
                    rd: 8 + ((h >> 2) & 7) as u8,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    imm: imm as i64,
                })
            }
            5 => {
                // C.FSD（CS 格式同 C.SD）
                let imm = (((h >> 10) & 7) << 3) | (((h >> 5) & 3) << 6);
                Ok(Inst::FStore {
                    fmt: Fmt::D,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    rs2: 8 + ((h >> 2) & 7) as u8,
                    imm: imm as i64,
                })
            }
            _ => Err(Exception::IllegalInstruction),
        },
        1 => {
            let rd = ((h >> 7) & 0x1F) as u8;
            match funct3 {
                0 => Ok(Inst::OpImm {
                    op: AluOp::Add,
                    rd,
                    rs1: rd,
                    imm: c_sext6(h),
                }), // C.ADDI（rd=x0 即 C.NOP/HINT）
                1 if rd != 0 => Ok(Inst::OpImm {
                    op: AluOp::Addw,
                    rd,
                    rs1: rd,
                    imm: c_sext6(h),
                }), // C.ADDIW
                2 => Ok(Inst::OpImm {
                    op: AluOp::Add,
                    rd,
                    rs1: 0,
                    imm: c_sext6(h),
                }), // C.LI
                3 => {
                    if rd == 2 {
                        // C.ADDI16SP
                        let imm = (((h >> 12) & 1) << 9)
                            | (((h >> 3) & 3) << 7)
                            | (((h >> 5) & 1) << 6)
                            | (((h >> 2) & 1) << 5)
                            | (((h >> 6) & 1) << 4);
                        Ok(Inst::OpImm {
                            op: AluOp::Add,
                            rd: 2,
                            rs1: 2,
                            imm: sext(imm as u64, 10),
                        })
                    } else if rd != 0 {
                        // C.LUI
                        let imm = sext(
                            ((((h >> 12) & 1) << 5) | ((h >> 2) & 0x1F)) as u64,
                            6,
                        ) << 12;
                        Ok(Inst::Lui { rd, imm })
                    } else {
                        Err(Exception::IllegalInstruction)
                    }
                }
                4 => {
                    let rd = 8 + ((h >> 7) & 7) as u8;
                    match (h >> 10) & 3 {
                        0 => Ok(Inst::OpImm {
                            op: AluOp::Srl,
                            rd,
                            rs1: rd,
                            imm: ((((h >> 12) & 1) << 5) | ((h >> 2) & 0x1F)) as i64,
                        }), // C.SRLI
                        1 => Ok(Inst::OpImm {
                            op: AluOp::Sra,
                            rd,
                            rs1: rd,
                            imm: ((((h >> 12) & 1) << 5) | ((h >> 2) & 0x1F)) as i64,
                        }), // C.SRAI
                        2 => Ok(Inst::OpImm {
                            op: AluOp::And,
                            rd,
                            rs1: rd,
                            imm: c_sext6(h),
                        }), // C.ANDI
                        _ => {
                            let rs2 = 8 + ((h >> 2) & 7) as u8;
                            if (h >> 12) & 1 == 0 {
                                let op = match (h >> 5) & 3 {
                                    0 => AluOp::Sub,
                                    1 => AluOp::Xor,
                                    2 => AluOp::Or,
                                    3 => AluOp::And,
                                    _ => unreachable!(),
                                };
                                Ok(Inst::Op { op, rd, rs1: rd, rs2 })
                            } else {
                                match (h >> 5) & 3 {
                                    0 => Ok(Inst::Op { op: AluOp::Subw, rd, rs1: rd, rs2 }),
                                    1 => Ok(Inst::Op { op: AluOp::Addw, rd, rs1: rd, rs2 }),
                                    _ => Err(Exception::IllegalInstruction),
                                }
                            }
                        }
                    }
                }
                5 => Ok(Inst::Jal { rd: 0, imm: cj_imm(h) }), // C.J
                6 => Ok(Inst::Branch {
                    op: BranchOp::Eq,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    rs2: 0,
                    imm: cb_imm(h),
                }), // C.BEQZ
                _ => Ok(Inst::Branch {
                    op: BranchOp::Ne,
                    rs1: 8 + ((h >> 7) & 7) as u8,
                    rs2: 0,
                    imm: cb_imm(h),
                }), // C.BNEZ
            }
        }
        _ => {
            let rd = ((h >> 7) & 0x1F) as u8;
            match funct3 {
                0 => Ok(Inst::OpImm {
                    op: AluOp::Sll,
                    rd,
                    rs1: rd,
                    imm: ((((h >> 12) & 1) << 5) | ((h >> 2) & 0x1F)) as i64,
                }), // C.SLLI
                1 => {
                    // C.FLDSP
                    let imm = (((h >> 2) & 7) << 6)
                        | (((h >> 5) & 3) << 3)
                        | (((h >> 12) & 1) << 5);
                    Ok(Inst::FLoad {
                        fmt: Fmt::D,
                        rd,
                        rs1: 2,
                        imm: imm as i64,
                    })
                }
                2 if rd != 0 => {
                    // C.LWSP
                    let imm = (((h >> 2) & 3) << 6)
                        | (((h >> 4) & 7) << 2)
                        | (((h >> 12) & 1) << 5);
                    Ok(Inst::Load {
                        op: LoadOp::W,
                        rd,
                        rs1: 2,
                        imm: imm as i64,
                    })
                }
                3 if rd != 0 => {
                    // C.LDSP
                    let imm = (((h >> 2) & 7) << 6) | (((h >> 5) & 3) << 3) | (((h >> 12) & 1) << 5);
                    Ok(Inst::Load {
                        op: LoadOp::D,
                        rd,
                        rs1: 2,
                        imm: imm as i64,
                    })
                }
                4 => {
                    let rs2 = ((h >> 2) & 0x1F) as u8;
                    if (h >> 12) & 1 == 0 {
                        // funct4=1000
                        if rs2 == 0 {
                            if rd == 0 {
                                Err(Exception::IllegalInstruction)
                            } else {
                                Ok(Inst::Jalr { rd: 0, rs1: rd, imm: 0 }) // C.JR
                            }
                        } else {
                            Ok(Inst::Op {
                                op: AluOp::Add,
                                rd,
                                rs1: 0,
                                rs2,
                            }) // C.MV（rd=x0 时天然为 HINT）
                        }
                    } else {
                        // funct4=1001
                        if rs2 == 0 {
                            if rd == 0 {
                                Ok(Inst::System(SystemOp::Ebreak)) // C.EBREAK
                            } else {
                                Ok(Inst::Jalr { rd: 1, rs1: rd, imm: 0 }) // C.JALR
                            }
                        } else {
                            Ok(Inst::Op {
                                op: AluOp::Add,
                                rd,
                                rs1: rd,
                                rs2,
                            }) // C.ADD
                        }
                    }
                }
                5 => {
                    // C.FSDSP
                    let imm = (((h >> 7) & 7) << 6) | (((h >> 10) & 7) << 3);
                    Ok(Inst::FStore {
                        fmt: Fmt::D,
                        rs1: 2,
                        rs2: ((h >> 2) & 0x1F) as u8,
                        imm: imm as i64,
                    })
                }
                6 => {
                    // C.SWSP（CSS：uimm[5:2]=inst[12:9], uimm[7:6]=inst[8:7], rs2=inst[6:2]）
                    let imm = (((h >> 9) & 0xF) << 2) | (((h >> 7) & 3) << 6);
                    Ok(Inst::Store {
                        op: StoreOp::W,
                        rs1: 2,
                        rs2: ((h >> 2) & 0x1F) as u8,
                        imm: imm as i64,
                    })
                }
                7 => {
                    // C.SDSP（CSS：uimm[5:3]=inst[12:10], uimm[8:6]=inst[9:7], rs2=inst[6:2]）
                    let imm = (((h >> 7) & 7) << 6) | (((h >> 10) & 7) << 3);
                    Ok(Inst::Store {
                        op: StoreOp::D,
                        rs1: 2,
                        rs2: ((h >> 2) & 0x1F) as u8,
                        imm: imm as i64,
                    })
                }
                _ => Err(Exception::IllegalInstruction),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_basic() {
        // addi x1, x0, -1
        assert_eq!(
            decode(0xFFF00093),
            Ok(Inst::OpImm { op: AluOp::Add, rd: 1, rs1: 0, imm: -1 })
        );
        // srai x1, x2, 5
        assert_eq!(
            decode(0x40515093),
            Ok(Inst::OpImm { op: AluOp::Sra, rd: 1, rs1: 2, imm: 5 })
        );
        // mulw x3, x4, x5
        assert_eq!(
            decode(0x025201BB),
            Ok(Inst::Op { op: AluOp::Mulw, rd: 3, rs1: 4, rs2: 5 })
        );
        // csrrw x3, 0x341, x5
        assert_eq!(
            decode(0x341291F3),
            Ok(Inst::System(SystemOp::Csr {
                kind: CsrKind::Rw,
                imm: false,
                csr: 0x341,
                rd: 3,
                rs1: 5
            }))
        );
        // amoswap.w x6, x7, (x8)
        assert_eq!(
            decode(0x0874232F),
            Ok(Inst::Amo { op: AmoOp::Swap, w: true, rd: 6, rs1: 8, rs2: 7 })
        );
    }

    #[test]
    fn decode_compressed_basic() {
        // c.addi16sp sp, 16  (0x6141)
        assert_eq!(
            decode_compressed(0x6141),
            Ok(Inst::OpImm { op: AluOp::Add, rd: 2, rs1: 2, imm: 16 })
        );
        // c.addi a5, -1  (rd=15, imm=-1): funct3=000 quad=01, imm[5]=1, imm[4:0]=11111
        // = 0b000_1_11111_01111_01 = 0x17FD
        assert_eq!(
            decode_compressed(0x17FD),
            Ok(Inst::OpImm { op: AluOp::Add, rd: 15, rs1: 15, imm: -1 })
        );
        // c.add a3, a0: funct4=1001 rd=a3 rs2=a0 -> add a3, a3, a0 (0x96AA)
        assert_eq!(
            decode_compressed(0x96AA),
            Ok(Inst::Op { op: AluOp::Add, rd: 13, rs1: 13, rs2: 10 })
        );
        // c.mv a3, a0: funct4=1000 rd=a3 rs2=a0 -> add a3, x0, a0 (0x86AA)
        assert_eq!(
            decode_compressed(0x86AA),
            Ok(Inst::Op { op: AluOp::Add, rd: 13, rs1: 0, rs2: 10 })
        );
    }

    #[test]
    fn decode_compressed_llvm_reference() {
        // 以下编码均由 llvm-mc --triple=riscv64 -mattr=+c 生成
        assert_eq!(
            decode_compressed(0xE42A), // c.sdsp a0, 8(sp)
            Ok(Inst::Store { op: StoreOp::D, rs1: 2, rs2: 10, imm: 8 })
        );
        assert_eq!(
            decode_compressed(0x6522), // c.ldsp a0, 8(sp)
            Ok(Inst::Load { op: LoadOp::D, rd: 10, rs1: 2, imm: 8 })
        );
        assert_eq!(
            decode_compressed(0x4522), // c.lwsp a0, 8(sp)
            Ok(Inst::Load { op: LoadOp::W, rd: 10, rs1: 2, imm: 8 })
        );
        assert_eq!(
            decode_compressed(0xC42A), // c.swsp a0, 8(sp)
            Ok(Inst::Store { op: StoreOp::W, rs1: 2, rs2: 10, imm: 8 })
        );
        assert_eq!(
            decode_compressed(0x6588), // c.ld a0, 8(a1)
            Ok(Inst::Load { op: LoadOp::D, rd: 10, rs1: 11, imm: 8 })
        );
        assert_eq!(
            decode_compressed(0xE588), // c.sd a0, 8(a1)
            Ok(Inst::Store { op: StoreOp::D, rs1: 11, rs2: 10, imm: 8 })
        );
        assert_eq!(
            decode_compressed(0xD97D), // c.beqz a0, -10
            Ok(Inst::Branch { op: BranchOp::Eq, rs1: 10, rs2: 0, imm: -10 })
        );
        assert_eq!(
            decode_compressed(0xA095), // c.j 100
            Ok(Inst::Jal { rd: 0, imm: 100 })
        );
        assert_eq!(
            decode_compressed(0x7139), // c.addi16sp sp, -64
            Ok(Inst::OpImm { op: AluOp::Add, rd: 2, rs1: 2, imm: -64 })
        );
        assert_eq!(
            decode_compressed(0x357D), // c.addiw a0, -1
            Ok(Inst::OpImm { op: AluOp::Addw, rd: 10, rs1: 10, imm: -1 })
        );
        assert_eq!(
            decode_compressed(0x9D0D), // c.subw a0, a1
            Ok(Inst::Op { op: AluOp::Subw, rd: 10, rs1: 10, rs2: 11 })
        );
        assert_eq!(
            decode_compressed(0x6511), // c.lui a0, 4
            Ok(Inst::Lui { rd: 10, imm: 0x4000 })
        );
        assert_eq!(
            decode_compressed(0x852E), // c.mv a0, a1
            Ok(Inst::Op { op: AluOp::Add, rd: 10, rs1: 0, rs2: 11 })
        );
        assert_eq!(
            decode_compressed(0x952E), // c.add a0, a1
            Ok(Inst::Op { op: AluOp::Add, rd: 10, rs1: 10, rs2: 11 })
        );
        assert_eq!(
            decode_compressed(0x9002), // c.ebreak
            Ok(Inst::System(SystemOp::Ebreak))
        );
        assert_eq!(
            decode_compressed(0x8031), // c.srli s0, 12
            Ok(Inst::OpImm { op: AluOp::Srl, rd: 8, rs1: 8, imm: 12 })
        );
        assert_eq!(
            decode_compressed(0x0516), // c.slli a0, 5
            Ok(Inst::OpImm { op: AluOp::Sll, rd: 10, rs1: 10, imm: 5 })
        );
        assert_eq!(
            decode_compressed(0x8515), // c.srai a0, 5
            Ok(Inst::OpImm { op: AluOp::Sra, rd: 10, rs1: 10, imm: 5 })
        );
    }
}
