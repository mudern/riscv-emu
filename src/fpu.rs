//! IEEE-754 F/D 执行核心。
//!
//! 数值语义用 Rust 原生 `f32`/`f64`（IEEE，RNE）实现；inexact 判定与非默认
//! 舍入模式用无误差变换：two-sum（加减）/ FMA 残差（乘除平方根）给出精确
//! 余量 e（RNE 结果 + e == 精确值），再按目标舍入模式从 (RNE 结果, e) 导出。
//! NaN 一律规范化为规范 quiet NaN（确定性优先于载荷传播）。
//!
//! 已知限制：精确结果低于 f64/f32 最小次正规数（|exact| < 2^-1074 / 2^-149）
//! 时，残差自身下溢，UF/NX 标志可能缺失（结果值与符号仍正确）。
//! FMADD 的标志为 two-sum 链近似（除三重舍入角落外精确）。

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
    if rm_field == 7 {
        rm_of(((fcsr >> 5) & 7) as u64)
    } else {
        rm_of(rm_field)
    }
}

type Flags = u32;

fn flags_of(nx: bool, of: bool, uf: bool) -> Flags {
    (nx as u32) | ((of as u32) << 2) | ((uf as u32) << 1)
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

/// 溢出时的结果位型（IEEE：RN/RMM → ±inf，RZ → ±max，RD → +max/-inf，
/// RU → +inf/-max）
fn overflow64(rm: Rm, pos: bool) -> u64 {
    match rm {
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
    }
}

fn overflow32(rm: Rm, pos: bool) -> u32 {
    match rm {
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
    }
}

/// 从 (RNE 结果, 精确残差) 按舍入模式导出最终结果与标志。
/// s + e == 精确值，|e| ≤ 0.5 ulp(s)。
///
/// 约定：
/// - e == NaN：无误差变换在无穷操作数下失效，此时精确结果就是 s（无标志）；
/// - e == ±∞ 或 s == ±∞：精确值溢出 → OF（含 RNE 舍回 max 的窗口：
///   s == ±max 且 e 朝外）。
fn round64(s: f64, e: f64, rm: Rm) -> (f64, Flags) {
    if e == 0.0 {
        return (s, 0);
    }
    if e.is_nan() {
        return (s, 0);
    }
    let max = f64::MAX;
    if s.is_infinite() || e.is_infinite() || (s == max && e > 0.0) || (s == -max && e < 0.0) {
        let pos = if s == 0.0 { e > 0.0 } else { s > 0.0 };
        return (f64::from_bits(overflow64(rm, pos)), FFLAGS_OF | FFLAGS_NX);
    }
    let nx = true;
    let v = match rm {
        Rm::RNE => s,
        Rm::RTZ => {
            // e 的符号给出精确值在 s 的哪一侧；向零 = 朝减小幅值方向
            let toward = (e < 0.0) == (s > 0.0);
            if toward {
                if s > 0.0 {
                    s.next_down()
                } else if s < 0.0 {
                    s.next_up()
                } else {
                    s
                }
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
            // 平局判定用 e 所在一侧的 ulp 间距（2 的幂边界两侧间距不同）。
            // 平局时取绝对值更大的候选：精确值在 s 下方（e<0）时候选为
            // {next_down, s}，在上方（e>0）时为 {s, next_up}——RNE 可能
            // 已选远离零侧，此时保持 s 本身。
            let side_gap = if e > 0.0 { s.next_up() - s } else { s - s.next_down() };
            if e.abs() * 2.0 == side_gap {
                if e < 0.0 {
                    // 候选 {next_down, s}
                    if s < 0.0 { s.next_down() } else { s }
                } else {
                    // 候选 {s, next_up}
                    if s > 0.0 { s.next_up() } else if s < 0.0 { s } else { s.next_up() }
                }
            } else {
                s
            }
        }
    };
    // 舍入到 ±0 时保留精确值的符号
    let mut v = v;
    if v == 0.0 {
        let exact_neg = if s == 0.0 { e < 0.0 } else { s < 0.0 };
        v = if exact_neg { -0.0 } else { 0.0 };
    }
    let uf = nx && (v == 0.0 || v.is_subnormal());
    (v, flags_of(nx, false, uf))
}

fn round32(s: f32, e: f32, rm: Rm) -> (f32, Flags) {
    if e == 0.0 {
        return (s, 0);
    }
    if e.is_nan() {
        return (s, 0);
    }
    let max = f32::MAX;
    if s.is_infinite() || e.is_infinite() || (s == max && e > 0.0) || (s == -max && e < 0.0) {
        let pos = if s == 0.0 { e > 0.0 } else { s > 0.0 };
        return (f32::from_bits(overflow32(rm, pos)), FFLAGS_OF | FFLAGS_NX);
    }
    let nx = true;
    let v = match rm {
        Rm::RNE => s,
        Rm::RTZ => {
            let toward = (e < 0.0) == (s > 0.0);
            if toward {
                if s > 0.0 {
                    s.next_down()
                } else if s < 0.0 {
                    s.next_up()
                } else {
                    s
                }
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
            let side_gap = if e > 0.0 { s.next_up() - s } else { s - s.next_down() };
            if e.abs() * 2.0 == side_gap {
                if e < 0.0 {
                    if s < 0.0 { s.next_down() } else { s }
                } else {
                    if s > 0.0 { s.next_up() } else if s < 0.0 { s } else { s.next_up() }
                }
            } else {
                s
            }
        }
    };
    let mut v = v;
    if v == 0.0 {
        let exact_neg = if s == 0.0 { e < 0.0 } else { s < 0.0 };
        v = if exact_neg { -0.0 } else { 0.0 };
    }
    let uf = nx && (v == 0.0 || v.is_subnormal());
    (v, flags_of(nx, false, uf))
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
            if q.is_infinite() {
                // 真溢出（x、y 有限）：IEEE 溢出结果表
                let pos = (a ^ b) & 0x8000_0000_0000_0000 == 0;
                return (overflow64(rm, pos), FFLAGS_OF | FFLAGS_NX);
            }
            let e = (-q).mul_add(y, x); // 精确残差
            let (v, flags) = round64(q, e, rm);
            (v.to_bits(), flags)
        }
        FArith::Sqrt => {
            if x < 0.0 {
                return (CANON_F64, FFLAGS_NV);
            }
            let r = x.sqrt();
            if r.is_infinite() {
                return (r.to_bits(), 0); // sqrt(+inf) 精确
            }
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
            if q.is_infinite() {
                let pos = (a ^ b) & 0x8000_0000 == 0;
                return (overflow32(rm, pos), FFLAGS_OF | FFLAGS_NX);
            }
            let e = (-q).mul_add(y, x);
            let (v, flags) = round32(q, e, rm);
            (v.to_bits(), flags)
        }
        FArith::Sqrt => {
            if x < 0.0 {
                return (CANON_F32 as u32, FFLAGS_NV);
            }
            let r = x.sqrt();
            if r.is_infinite() {
                return (r.to_bits(), 0);
            }
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

/// 乘加（单舍入）：±(a*b ± c)。结果值精确（Rust fma 正确舍入）；
/// 标志用 two-sum 链近似（除三重舍入角落外精确）。
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
    if r.is_infinite() {
        // 真溢出
        let pos = !r.is_sign_negative();
        return (overflow64(rm, pos), FFLAGS_OF | FFLAGS_NX);
    }
    // 残差：exact = (p + pe) + z，p = fl(x*y)，pe = x*y - p 精确
    let (p, pe) = {
        let p = x * y;
        (p, x.mul_add(y, -p))
    };
    let z_eff = match kind {
        K::NAddNeg => -z,
        _ => z,
    };
    let (s1, e1) = two_sum64(p, z_eff);
    let r_eff = match kind {
        K::NAddNeg => -r,
        _ => r,
    };
    let (b, _be) = two_sum64(s1 - r_eff, e1);
    let (c, ce) = two_sum64(b, pe);
    let e_total = c + ce;
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
    if r.is_infinite() {
        let pos = !r.is_sign_negative();
        return (overflow32(rm, pos) as u64, FFLAGS_OF | FFLAGS_NX);
    }
    let (p, pe) = {
        let p = x * y;
        (p, x.mul_add(y, -p))
    };
    let z_eff = match kind {
        K::NAddNeg => -z,
        _ => z,
    };
    let (s1, e1) = two_sum32(p, z_eff);
    let r_eff = match kind {
        K::NAddNeg => -r,
        _ => r,
    };
    let (b, _be) = two_sum32(s1 - r_eff, e1);
    let (c, ce) = two_sum32(b, pe);
    let e_total = c + ce;
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
    // 范围预判（f64 边界精确；i64 上界 2^63 开区间）
    let out_hi = if signed && !is32 { x >= hi as f64 } else { x > hi as f64 };
    let out_lo = x < lo as f64;
    if out_hi || out_lo {
        let sat: u64 = if out_lo {
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
            if af > 0.5 || (af == 0.5 && (t as i64) % 2 != 0) {
                (t + af.signum()) as i128 // 超半取远，平局取偶
            } else {
                t as i128
            }
        }
        Rm::RMM => {
            let af = frac.abs();
            if af >= 0.5 {
                (t + af.signum()) as i128
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

/// FCVT：整数 → 浮点。幅值 + 符号分离法：高位整数部分 + 低位余量，
/// 五种舍入模式全部精确（平局判定无 f64 表示误差）。
pub fn cvt_i_to_f(v: u64, signed: bool, src32: bool, to64: bool, rm: Rm) -> (u64, Flags) {
    let (neg, mag): (bool, u64) = if src32 {
        if signed {
            let x = v as u32 as i32;
            (x < 0, x.unsigned_abs() as u64)
        } else {
            (false, v as u32 as u64)
        }
    } else if signed {
        let x = v as i64;
        (x < 0, x.unsigned_abs())
    } else {
        (false, v)
    };
    let mant = if to64 { 53 } else { 24 };
    let bits = 64 - mag.leading_zeros(); // mag == 0 → 0
    let (result, nx): (f64, bool) = if bits <= mant {
        let m = mag as f64;
        (if neg { -m } else { m }, false)
    } else {
        let shift = bits - mant;
        let half = 1u64 << (shift - 1);
        let low = mag & ((1u64 << shift) - 1);
        let mut high = mag >> shift;
        let inexact = low != 0;
        match rm {
            Rm::RTZ => {}
            Rm::RNE => {
                if low > half || (low == half && high & 1 == 1) {
                    high += 1;
                }
            }
            Rm::RDN => {
                // 负数向下 = 绝对值向上
                if neg && inexact {
                    high += 1;
                }
            }
            Rm::RUP => {
                if !neg && inexact {
                    high += 1;
                }
            }
            Rm::RMM => {
                if low >= half {
                    high += 1; // 平局远离零
                }
            }
        }
        // high ≤ 2^mant，乘 2 的幂精确（结果不超过目标格式范围：
        // 2^64 < f32::MAX）
        let scaled = (high as f64) * (2f64).powi(shift as i32);
        (if neg { -scaled } else { scaled }, inexact)
    };
    if to64 {
        (result.to_bits(), nx as u32)
    } else {
        ((result as f32).to_bits() as u64, nx as u32)
    }
}

/// 格式转换 S↔D（S→D 恒精确；D→S 按 rm 舍入，邻居比较在 f64 中精确）
pub fn cvt_f_to_f(a: u64, to64: bool, rm: Rm) -> (u64, Flags) {
    if to64 {
        (f64::from(f32::from_bits(a as u32)).to_bits(), 0)
    } else {
        let x = f64::from_bits(a);
        let r = x as f32; // RNE
        let nx = (r as f64) != x;
        if !nx {
            return (r.to_bits() as u64, 0);
        }
        let v = match rm {
            Rm::RNE => r,
            Rm::RTZ => {
                if (r as f64) > x {
                    // r 在精确值上方：向零 = 减小幅值（正数向下、负数即 r）
                    if x > 0.0 { r.next_down() } else { r }
                } else if x > 0.0 {
                    r
                } else {
                    r.next_up()
                }
            }
            Rm::RDN => {
                if (r as f64) > x { r.next_down() } else { r }
            }
            Rm::RUP => {
                if (r as f64) < x { r.next_up() } else { r }
            }
            Rm::RMM => {
                let du = (r.next_up() as f64 - x).abs();
                let dd = (x - r.next_down() as f64).abs();
                if du < dd {
                    r.next_up()
                } else if dd < du {
                    r.next_down()
                } else if x > 0.0 {
                    r.next_up() // 平局远离零
                } else {
                    r.next_down()
                }
            }
        };
        let uf = nx && (v == 0.0 || v.is_subnormal());
        (v.to_bits() as u64, flags_of(nx, false, uf))
    }
}
