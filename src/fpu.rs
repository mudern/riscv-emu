//! IEEE-754 F/D 执行核心。
//!
//! 数值语义用 Rust 原生 `f32`/`f64`（IEEE，RNE）实现；inexact 判定与非默认
//! 舍入模式用无误差变换：two-sum（加减）/ FMA 残差（乘除平方根）给出精确
//! 余量 e（RNE 结果 + e == 精确值），再按目标舍入模式从 (RNE 结果, e) 导出。
//! NaN 一律规范化为规范 quiet NaN（确定性优先于载荷传播）。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FArith {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rm {
    RNE,
    RTZ,
    RDN,
    RUP,
    RMM,
}

pub const FFLAGS_NV: u32 = 1 << 4;
pub const FFLAGS_DZ: u32 = 1 << 3;
pub const FFLAGS_OF: u32 = 1 << 2;
pub const FFLAGS_UF: u32 = 1 << 1;
pub const FFLAGS_NX: u32 = 1 << 0;

pub const CANON_F32: u64 = 0x7FC0_0000;
pub const CANON_F64: u64 = 0x7FF8_0000_0000_0000;

pub fn rm_of(rm: u64) -> Option<Rm> {
    match rm {
        0 => Some(Rm::RNE),
        1 => Some(Rm::RTZ),
        2 => Some(Rm::RDN),
        3 => Some(Rm::RUP),
        4 => Some(Rm::RMM),
        _ => None,
    }
}

/// rm 字段 → 有效舍入模式（7 = DYN，取 fcsr.frm）
pub fn effective_rm(rm_field: u64, fcsr: u32) -> Option<Rm> {
    if rm_field == 7 { rm_of(((fcsr >> 5) & 7) as u64) } else { rm_of(rm_field) }
}

type Flags = u32;

fn flags_of(nx: bool, of: bool, uf: bool) -> Flags {
    (nx as u32) | ((of as u32) << 2) | ((uf as u32) << 1)
}

/// 从 (RNE 结果, 精确残差) 按舍入模式导出最终结果与标志。
/// s + e == 精确值，|e| ≤ 0.5 ulp(s)。s 为有限值（溢出由调用方处理）。
fn round64(s: f64, e: f64, rm: Rm) -> (f64, Flags) {
    if e == 0.0 {
        return (s, 0);
    }
    let nx = true;
    if s.is_infinite() {
        let pos = s > 0.0;
        let bits: u64 = match rm {
            Rm::RNE | Rm::RMM => {
                if pos { 0x7FF0_0000_0000_0000 } else { 0xFFF0_0000_0000_0000 }
            }
            Rm::RTZ => {
                if pos { 0x7FEF_FFFF_FFFF_FFFF } else { 0xFFEF_FFFF_FFFF_FFFF }
            }
            Rm::RDN => {
                if pos { 0x7FEF_FFFF_FFFF_FFFF } else { 0xFFF0_0000_0000_0000 }
            }
            Rm::RUP => {
                if pos { 0x7FF0_0000_0000_0000 } else { 0xFFEF_FFFF_FFFF_FFFF }
            }
        };
        return (f64::from_bits(bits), FFLAGS_OF | FFLAGS_NX);
    }
    let v = match rm {
        Rm::RNE => s,
        Rm::RTZ => {
            let toward_zero = (e < 0.0) == (s > 0.0);
            if toward_zero {
                if s > 0.0 { s.next_down() } else { s.next_up() }
            } else {
                s
            }
        }
        Rm::RDN => {
            if e < 0.0 { s.next_down() } else { s }
        }
        Rm::RUP => {
            if e > 0.0 { s.next_up() } else { s }
        }
        Rm::RMM => {
            let gap = (s.next_up() - s).abs();
            if e.abs() * 2.0 == gap {
                if s > 0.0 { s.next_up() } else { s.next_down() }
            } else {
                s
            }
        }
    };
    finish64(v, nx)
}

fn finish64(v: f64, nx: bool) -> (f64, Flags) {
    // 舍入到 ±0 保留符号；tiny（±0 或 subnormal）且不精确 → UF
    let uf = nx && (v == 0.0 || v.is_subnormal());
    (v, flags_of(nx, false, uf))
}

/// two-sum：s + e == a + b（无误差）
fn two_sum64(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    let aa = s - bb;
    let eb = b - bb;
    let ea = a - aa;
    (s, ea + eb)
}

fn two_sum32(a: f32, b: f32) -> (f32, f32) {
    let s = a + b;
    let bb = s - a;
    let aa = s - bb;
    let eb = b - bb;
    let ea = a - aa;
    (s, ea + eb)
}

/// f64 算术（位型输入输出）
pub fn arith64(op: FArith, a: u64, b: u64, rm: Rm) -> (u64, Flags) {
    let (x, y) = (f64::from_bits(a), f64::from_bits(b));
    if x.is_nan() || y.is_nan() {
        return (CANON_F64, FFLAGS_NV);
    }
    match op {
        FArith::Div => {
            if y == 0.0 {
                if x == 0.0 {
                    return (CANON_F64, FFLAGS_NV); // 0/0
                }
                // 非零/0：带符号无穷
                let sign = (a ^ b) & 0x8000_0000_0000_0000;
                return (sign | 0x7FF0_0000_0000_0000, FFLAGS_DZ);
            }
            if x.is_infinite() && y.is_infinite() {
                return (CANON_F64, FFLAGS_NV); // inf/inf
            }
            let q = x / y;
            let e = (-q).mul_add(y, x); // 精确残差
            let (v, flags) = round64(q, e, rm);
            (v.to_bits(), flags)
        }
        FArith::Sqrt => {
            if x < 0.0 {
                return (CANON_F64, FFLAGS_NV);
            }
            let r = x.sqrt();
            let e = (-r).mul_add(r, x);
            let (v, flags) = round64(r, e, rm);
            (v.to_bits(), flags)
        }
        _ => {
            let (s, e) = match op {
                FArith::Add => two_sum64(x, y),
                FArith::Sub => two_sum64(x, -y),
                FArith::Mul => {
                    let p = x * y;
                    let e = x.mul_add(y, -p);
                    (p, e)
                }
                _ => unreachable!(),
            };
            if s.is_nan() {
                // inf - inf、0 * inf 等无效操作
                return (CANON_F64, FFLAGS_NV);
            }
            let (v, flags) = round64(s, e, rm);
            (v.to_bits(), flags)
        }
    }
}

/// f32 算术（位型输入输出）
pub fn arith32(op: FArith, a: u32, b: u32, rm: Rm) -> (u32, Flags) {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if x.is_nan() || y.is_nan() {
        return (CANON_F32 as u32, FFLAGS_NV);
    }
    match op {
        FArith::Div => {
            if y == 0.0 {
                if x == 0.0 {
                    return (CANON_F32 as u32, FFLAGS_NV);
                }
                let sign = (a ^ b) & 0x8000_0000;
                return (sign | 0x7F80_0000, FFLAGS_DZ);
            }
            if x.is_infinite() && y.is_infinite() {
                return (CANON_F32 as u32, FFLAGS_NV);
            }
            let q = x / y;
            let e = (-q).mul_add(y, x);
            let (v, flags) = round32(q, e, rm);
            (v.to_bits(), flags)
        }
        FArith::Sqrt => {
            if x < 0.0 {
                return (CANON_F32 as u32, FFLAGS_NV);
            }
            let r = x.sqrt();
            let e = (-r).mul_add(r, x);
            let (v, flags) = round32(r, e, rm);
            (v.to_bits(), flags)
        }
        _ => {
            let (s, e) = match op {
                FArith::Add => two_sum32(x, y),
                FArith::Sub => two_sum32(x, -y),
                FArith::Mul => {
                    let p = x * y;
                    let e = x.mul_add(y, -p);
                    (p, e)
                }
                _ => unreachable!(),
            };
            if s.is_nan() {
                return (CANON_F32 as u32, FFLAGS_NV);
            }
            let (v, flags) = round32(s, e, rm);
            (v.to_bits(), flags)
        }
    }
}

fn round32(s: f32, e: f32, rm: Rm) -> (f32, Flags) {
    if e == 0.0 {
        return (s, 0);
    }
    let nx = true;
    if s.is_infinite() {
        let pos = s > 0.0;
        let bits: u32 = match rm {
            Rm::RNE | Rm::RMM => {
                if pos { 0x7F80_0000 } else { 0xFF80_0000 }
            }
            Rm::RTZ => {
                if pos { 0x7F7F_FFFF } else { 0xFF7F_FFFF }
            }
            Rm::RDN => {
                if pos { 0x7F7F_FFFF } else { 0xFF80_0000 }
            }
            Rm::RUP => {
                if pos { 0x7F80_0000 } else { 0xFF7F_FFFF }
            }
        };
        return (f32::from_bits(bits), FFLAGS_OF | FFLAGS_NX);
    }
    let v = match rm {
        Rm::RNE => s,
        Rm::RTZ => {
            let toward_zero = (e < 0.0) == (s > 0.0);
            if toward_zero {
                if s > 0.0 { s.next_down() } else { s.next_up() }
            } else {
                s
            }
        }
        Rm::RDN => {
            if e < 0.0 { s.next_down() } else { s }
        }
        Rm::RUP => {
            if e > 0.0 { s.next_up() } else { s }
        }
        Rm::RMM => {
            let gap = (s.next_up() - s).abs();
            if e.abs() * 2.0 == gap {
                if s > 0.0 { s.next_up() } else { s.next_down() }
            } else {
                s
            }
        }
    };
    let uf = nx && (v == 0.0 || v.is_subnormal());
    (v, flags_of(nx, false, uf))
}

/// 乘加（单舍入）：±(a*b ± c)
pub fn muladd64(kind: crate::decode::FMulAdd, a: u64, b: u64, c: u64, rm: Rm) -> (u64, Flags) {
    let (x, y, z) = (f64::from_bits(a), f64::from_bits(b), f64::from_bits(c));
    if x.is_nan() || y.is_nan() || z.is_nan() {
        return (CANON_F64, FFLAGS_NV);
    }
    use crate::decode::FMulAdd as K;
    let addend = match kind {
        K::Add | K::NAddNeg => z,
        K::Sub | K::NAdd => -z,
    };
    let mut r = x.mul_add(y, addend);
    if matches!(kind, K::NAddNeg) {
        r = -r;
    }
    // 残差（近似，一阶）：exact = x*y + addend_signed
    let (p, pe) = {
        let p = x * y;
        let e = x.mul_add(y, -p);
        (p, e)
    };
    let (s, e) = two_sum64(
        match kind {
            K::NAddNeg => -p,
            _ => p,
        },
        match kind {
            K::NAddNeg => -z,
            _ => z,
        },
    );
    let _ = pe;
    // s + e 为精确乘加结果；RNE 已在 r
    let _ = s;
    let e2 = s - r; // 近似修正量
    let e_total = e + e2;
    let (v, flags) = round64(r, e_total, rm);
    (v.to_bits(), flags)
}

pub fn muladd32(kind: crate::decode::FMulAdd, a: u32, b: u32, c: u32, rm: Rm) -> (u64, Flags) {
    let (x, y, z) = (f32::from_bits(a), f32::from_bits(b), f32::from_bits(c));
    if x.is_nan() || y.is_nan() || z.is_nan() {
        return (CANON_F32, FFLAGS_NV);
    }
    use crate::decode::FMulAdd as K;
    let addend = match kind {
        K::Add | K::NAddNeg => z,
        K::Sub | K::NAdd => -z,
    };
    let mut r = x.mul_add(y, addend);
    if matches!(kind, K::NAddNeg) {
        r = -r;
    }
    let (p, _pe) = {
        let p = x * y;
        let e = x.mul_add(y, -p);
        (p, e)
    };
    let (s, e) = two_sum32(
        match kind {
            K::NAddNeg => -p,
            _ => p,
        },
        match kind {
            K::NAddNeg => -z,
            _ => z,
        },
    );
    let e_total = e + (s - r);
    let (v, flags) = round32(r, e_total, rm);
    (v.to_bits() as u64, flags)
}

fn minmax32(a: u32, b: u32, max: bool) -> u32 {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if x.is_nan() || y.is_nan() {
        return CANON_F32 as u32;
    }
    let n0x = a == 0x8000_0000;
    let n0y = b == 0x8000_0000;
    if x == y && (n0x || n0y) {
        // ±0：min 取 -0，max 取 +0
        return if max == n0x { b } else { a };
    }
    if max {
        if x > y { a } else { b }
    } else if x < y {
        a
    } else {
        b
    }
}
fn minmax64(a: u64, b: u64, max: bool) -> u64 {
    let (x, y) = (f64::from_bits(a), f64::from_bits(b));
    if x.is_nan() || y.is_nan() {
        return CANON_F64;
    }
    let n0x = a == 0x8000_0000_0000_0000;
    let n0y = b == 0x8000_0000_0000_0000;
    if x == y && (n0x || n0y) {
        return if max == n0x { b } else { a };
    }
    if max {
        if x > y { a } else { b }
    } else if x < y {
        a
    } else {
        b
    }
}

pub fn fmin32(a: u32, b: u32) -> u32 {
    minmax32(a, b, false)
}
pub fn fmax32(a: u32, b: u32) -> u32 {
    minmax32(a, b, true)
}
pub fn fmin64(a: u64, b: u64) -> u64 {
    minmax64(a, b, false)
}
pub fn fmax64(a: u64, b: u64) -> u64 {
    minmax64(a, b, true)
}

/// 比较：kind 0=FLE 1=FLT 2=FEQ
pub fn fcmp32(kind: u8, a: u32, b: u32) -> (bool, Flags) {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if x.is_nan() || y.is_nan() {
        return (false, if kind != 2 { FFLAGS_NV } else { 0 });
    }
    (
        match kind {
            0 => x <= y,
            1 => x < y,
            _ => x == y,
        },
        0,
    )
}
pub fn fcmp64(kind: u8, a: u64, b: u64) -> (bool, Flags) {
    let (x, y) = (f64::from_bits(a), f64::from_bits(b));
    if x.is_nan() || y.is_nan() {
        return (false, if kind != 2 { FFLAGS_NV } else { 0 });
    }
    (
        match kind {
            0 => x <= y,
            1 => x < y,
            _ => x == y,
        },
        0,
    )
}

/// fclass：10 位掩码
pub fn fclass32(a: u32) -> u64 {
    let x = f32::from_bits(a);
    if x.is_nan() {
        return if x.is_sign_negative() { 1 << 8 } else { 1 << 9 };
    }
    if x.is_infinite() {
        return if x.is_sign_negative() { 1 } else { 1 << 7 };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { 1 << 3 } else { 1 << 4 };
    }
    if x.is_subnormal() {
        return if x.is_sign_negative() { 1 << 2 } else { 1 << 5 };
    }
    if x < 0.0 {
        1 << 1
    } else {
        1 << 6
    }
}
pub fn fclass64(a: u64) -> u64 {
    let x = f64::from_bits(a);
    if x.is_nan() {
        return if x.is_sign_negative() { 1 << 8 } else { 1 << 9 };
    }
    if x.is_infinite() {
        return if x.is_sign_negative() { 1 } else { 1 << 7 };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { 1 << 3 } else { 1 << 4 };
    }
    if x.is_subnormal() {
        return if x.is_sign_negative() { 1 << 2 } else { 1 << 5 };
    }
    if x < 0.0 {
        1 << 1
    } else {
        1 << 6
    }
}

/// FCVT：浮点 → 整数（按 rm 舍入，越界/NaN 饱和 + NV）。
/// 返回 (64 位结果, flags)；32 位取低 32 位（已符号/零扩展到 64）。
pub fn cvt_f_to_i(x: f64, rm: Rm, signed: bool, is32: bool) -> (u64, Flags) {
    if x.is_nan() {
        return (
            match (signed, is32) {
                (true, true) => i32::MIN as i64 as u64,
                (true, false) => i64::MIN as u64,
                (false, _) => 0,
            },
            FFLAGS_NV,
        );
    }
    let (lo, hi): (i128, i128) = if is32 {
        if signed {
            (i32::MIN as i128, i32::MAX as i128)
        } else {
            (0, u32::MAX as i128)
        }
    } else if signed {
        (i64::MIN as i128, i64::MAX as i128)
    } else {
        (0, u64::MAX as i128)
    };
    // 先做范围预判（f64 边界精确）
    let ulo = if signed && !is32 { hi + 1 } else { lo }; // signed i64 上界开区间 2^63
    let _ = ulo;
    let out_of_range_hi = if signed && !is32 {
        x >= hi as f64 // x >= 2^63 必越界（2^63 可精确表示）
    } else {
        x > hi as f64 && !(hi as f64 == x && !signed)
    };
    let out_of_range_lo = x < lo as f64;
    if out_of_range_hi || out_of_range_lo {
        let sat: u64 = if out_of_range_lo {
            if signed { i64::MIN as u64 } else { 0 }
        } else if signed {
            if is32 { i32::MAX as i64 as u64 } else { i64::MAX as u64 }
        } else if is32 {
            u32::MAX as u64
        } else {
            u64::MAX
        };
        return (sat, FFLAGS_NV);
    }
    // 范围内：安全地舍入到整数
    let t = x.trunc();
    let frac = x - t;
    let r: i128 = match rm {
        Rm::RTZ => t as i128,
        Rm::RDN => {
            if frac < 0.0 {
                (t - 1.0) as i128
            } else {
                t as i128
            }
        }
        Rm::RUP => {
            if frac > 0.0 {
                (t + 1.0) as i128
            } else {
                t as i128
            }
        }
        Rm::RNE => {
            let af = frac.abs();
            if af > 0.5 {
                (t + af.signum() * 1.0) as i128
            } else if af < 0.5 {
                t as i128
            } else {
                // 平局取偶
                let even = (t as i64) % 2 == 0;
                if even {
                    t as i128
                } else {
                    (t + af.signum() * 1.0) as i128
                }
            }
        }
        Rm::RMM => {
            let af = frac.abs();
            if af >= 0.5 {
                (t + af.signum() * 1.0) as i128
            } else {
                t as i128
            }
        }
    };
    let val: u64 = if signed {
        if is32 {
            (r as i32) as i64 as u64
        } else {
            (r as i64) as u64
        }
    } else if is32 {
        (r as u32) as u64
    } else {
        r as u64
    };
    (val, 0)
}

/// FCVT：整数 → 浮点
pub fn cvt_i_to_f(v: u64, signed: bool, src32: bool, to64: bool, rm: Rm) -> (u64, Flags) {
    let exact: f64 = if src32 {
        if signed {
            (v as u32) as i32 as f64
        } else {
            (v as u32) as f64
        }
    } else if signed {
        (v as i64) as f64
    } else {
        v as f64
    };
    // 精确性：整数有效位超出目标格式尾数（f32=24 / f64=53）时舍入
    let mantissa_bits = if to64 { 53 } else { 24 };
    let nx = if src32 {
        // i32/u32 最多 32 位有效位：u32 大数 → f32 可能舍入
        let bits = 32 - (v as u32).leading_zeros();
        bits > mantissa_bits && (v & ((1u64 << (bits - mantissa_bits)) - 1)) != 0
    } else {
        let mag = if signed {
            (v as i64).unsigned_abs()
        } else {
            v
        };
        if mag == 0 {
            false
        } else {
            let bits = 64 - mag.leading_zeros();
            bits > mantissa_bits && (mag & ((1u64 << (bits - mantissa_bits)) - 1)) != 0
        }
    };
    let (r32, r64): (f32, f64) = match rm {
        Rm::RNE => (exact as f32, exact),
        Rm::RTZ => {
            if nx {
                // 向零：先 RNE 再修正（exact 为正时 RNE 可能偏大）
                let s = exact as f32;
                let s = if (s as f64) > exact {
                    s.next_down()
                } else if (s as f64) < exact {
                    s.next_up()
                } else {
                    s
                };
                let d = if (exact as i64) as f64 > exact && v != 0 && signed && v as i64 > 0 {
                    exact
                } else {
                    s as f64
                };
                let _ = d;
                // f64 目标：i64→f64 RTZ
                let d = if !src32 && to64 {
                    if v == 0 {
                        exact
                    } else if signed && (v as i64) < 0 {
                        // 负数向零 = 向上
                        if exact < (v as i64) as f64 {
                            exact.next_up()
                        } else {
                            exact
                        }
                    } else if exact > v as f64 {
                        exact.next_down()
                    } else {
                        exact
                    }
                } else {
                    s as f64
                };
                (s, d)
            } else {
                (exact as f32, exact)
            }
        }
        Rm::RDN => {
            let s = if nx && exact > 0.0 { (exact as f32).next_down() } else { exact as f32 };
            let d = if nx && exact > 0.0 { exact.next_down() } else { exact };
            (s, d)
        }
        Rm::RUP => {
            let s = if nx && exact < 0.0 { (exact as f32).next_up() } else { exact as f32 };
            let d = if nx && exact < 0.0 { exact.next_up() } else { exact };
            (s, d)
        }
        Rm::RMM => (exact as f32, exact),
    };
    if to64 {
        (r64.to_bits(), nx as u32)
    } else {
        (r32.to_bits() as u64, nx as u32)
    }
}

/// 格式转换 S↔D（S→D 恒精确；D→S 按 rm 舍入）
pub fn cvt_f_to_f(a: u64, to64: bool, rm: Rm) -> (u64, Flags) {
    if to64 {
        (f64::from(f32::from_bits(a as u32)).to_bits(), 0)
    } else {
        let x = f64::from_bits(a);
        let rne = x as f32;
        let nx = (rne as f64) != x;
        let v = match rm {
            Rm::RNE => rne,
            _ => {
                let mut r = rne;
                if nx {
                    match rm {
                        Rm::RNE => {}
                        Rm::RTZ => {
                            if (r as f64) > x {
                                r = r.next_down();
                            } else if (r as f64) < x {
                                r = r.next_up();
                            }
                        }
                        Rm::RDN => {
                            if (r as f64) > x {
                                r = r.next_down();
                            }
                        }
                        Rm::RUP => {
                            if (r as f64) < x {
                                r = r.next_up();
                            }
                        }
                        Rm::RMM => unreachable!(),
                    }
                }
                r
            }
        };
        let uf = nx && (v == 0.0 || v.is_subnormal());
        (v.to_bits() as u64, flags_of(nx, false, uf))
    }
}
