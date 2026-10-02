//! F/D 浮点单元测试（fpu.rs 数值语义 + CPU 集成）

use riscv_emu::fpu::{self, FArith, Rm};

#[test]
fn arith64_basic() {
    let one = 1.0f64.to_bits();
    let two = 2.0f64.to_bits();
    let (r, f) = fpu::arith64(FArith::Add, one, two, Rm::RNE);
    assert_eq!(r, 3.0f64.to_bits());
    assert_eq!(f, 0);

    let (r, f) = fpu::arith64(FArith::Div, one, two, Rm::RNE);
    assert_eq!(r, 0.5f64.to_bits());
    assert_eq!(f, 0);

    // 1/3 不精确 → NX
    let (r, f) = fpu::arith64(FArith::Div, one, 3.0f64.to_bits(), Rm::RNE);
    assert_eq!(r, (1.0f64 / 3.0f64).to_bits());
    assert_eq!(f, fpu::FFLAGS_NX);

    // 除零 → DZ + 带符号无穷
    let (r, f) = fpu::arith64(FArith::Div, one, 0, Rm::RNE);
    assert_eq!(r, f64::INFINITY.to_bits());
    assert_eq!(f, fpu::FFLAGS_DZ);
    let (r, _) = fpu::arith64(FArith::Div, (-1.0f64).to_bits(), 0, Rm::RNE);
    assert_eq!(r, f64::NEG_INFINITY.to_bits());

    // 0/0 → NV
    let (_, f) = fpu::arith64(FArith::Div, 0, 0, Rm::RNE);
    assert_eq!(f, fpu::FFLAGS_NV);

    // inf - inf → NV
    let (_, f) = fpu::arith64(
        FArith::Sub,
        f64::INFINITY.to_bits(),
        f64::INFINITY.to_bits(),
        Rm::RNE,
    );
    assert_eq!(f, fpu::FFLAGS_NV);

    // sqrt(-1) → NV；sqrt(4) = 2 精确
    let (_, f) = fpu::arith64(FArith::Sqrt, (-1.0f64).to_bits(), 0, Rm::RNE);
    assert_eq!(f, fpu::FFLAGS_NV);
    let (r, f) = fpu::arith64(FArith::Sqrt, 4.0f64.to_bits(), 0, Rm::RNE);
    assert_eq!(r, 2.0f64.to_bits());
    assert_eq!(f, 0);

    // 舍入模式：1 + 2^-60（亚 ulp）向不同方向
    let tiny = 1.0f64 / (1u64 << 60) as f64;
    let (r, f) = fpu::arith64(FArith::Add, 1.0f64.to_bits(), tiny.to_bits(), Rm::RNE);
    assert_eq!(r, 1.0f64.to_bits(), "1+2^-60 RNE 舍回 1");
    assert_eq!(f, fpu::FFLAGS_NX);
    let (r, _) = fpu::arith64(FArith::Add, 1.0f64.to_bits(), tiny.to_bits(), Rm::RUP);
    assert!(r > 1.0f64.to_bits(), "RUP 应向上");
    let (r, _) = fpu::arith64(FArith::Add, (-1.0f64).to_bits(), (-tiny).to_bits(), Rm::RTZ);
    assert_eq!(r, (-1.0f64).to_bits(), "RTZ 向零截断");
}

#[test]
fn arith32_nan_boxing_semantics() {
    // f32 加法在 fpu::arith32 上独立工作
    let (r, f) = fpu::arith32(FArith::Add, 1.0f32.to_bits(), 2.0f32.to_bits(), Rm::RNE);
    assert_eq!(r, 3.0f32.to_bits());
    assert_eq!(f, 0);
    // 溢出 → OF
    let (_, f) = fpu::arith32(
        FArith::Mul,
        3.0e38f32.to_bits(),
        3.0e38f32.to_bits(),
        Rm::RNE,
    );
    assert_eq!(f, fpu::FFLAGS_OF | fpu::FFLAGS_NX);
}

#[test]
fn cvt_semantics() {
    // FCVT.L.D：2.7 RNE → 3（偶数侧）
    let (v, f) = fpu::cvt_f_to_i(2.7, Rm::RNE, true, false);
    assert_eq!((v as i64, f), (3, 0));
    // 2.5 平局取偶 → 2
    let (v, _) = fpu::cvt_f_to_i(2.5, Rm::RNE, true, false);
    assert_eq!(v as i64, 2);
    // 3.5 平局取偶 → 4
    let (v, _) = fpu::cvt_f_to_i(3.5, Rm::RNE, true, false);
    assert_eq!(v as i64, 4);
    // RTZ：-2.7 → -2
    let (v, _) = fpu::cvt_f_to_i(-2.7, Rm::RTZ, true, false);
    assert_eq!(v as i64, -2);
    // NaN → i64::MIN + NV
    let (v, f) = fpu::cvt_f_to_i(f64::NAN, Rm::RNE, true, false);
    assert_eq!((v as i64, f), (i64::MIN, fpu::FFLAGS_NV));
    // 越界 → 饱和 + NV
    let (v, f) = fpu::cvt_f_to_i(1e30, Rm::RNE, true, false);
    assert_eq!((v, f), (i64::MAX as u64, fpu::FFLAGS_NV));
    let (v, f) = fpu::cvt_f_to_i(-1e30, Rm::RNE, false, false);
    assert_eq!((v, f), (0, fpu::FFLAGS_NV));

    // 整数 → 浮点：2^60 精确；2^63-1 不精确
    let (r, f) = fpu::cvt_i_to_f(1u64 << 60, true, false, true, Rm::RNE);
    assert_eq!(r, ((1u64 << 60) as f64).to_bits());
    assert_eq!(f, 0);
    let (_, f) = fpu::cvt_i_to_f(i64::MAX as u64, true, false, true, Rm::RNE);
    assert_eq!(f, fpu::FFLAGS_NX);
}

#[test]
fn cmp_class_minmax() {
    // fmin(-0, +0) = -0；fmax(-0, +0) = +0
    let n0 = (-0.0f64).to_bits();
    let p0 = 0.0f64.to_bits();
    assert_eq!(fpu::fmin64(n0, p0), n0);
    assert_eq!(fpu::fmax64(n0, p0), p0);
    // NaN 输入 → 规范 qNaN
    assert_eq!(fpu::fmax64(f64::NAN.to_bits(), p0), fpu::CANON_F64);
    // fle NaN → false + NV；feq NaN → false 无标志
    let (_, f) = fpu::fcmp64(0, f64::NAN.to_bits(), 1.0f64.to_bits());
    assert_eq!(f, fpu::FFLAGS_NV);
    let (r, f) = fpu::fcmp64(2, f64::NAN.to_bits(), 1.0f64.to_bits());
    assert!(!r);
    assert_eq!(f, 0);
    // fclass
    assert_eq!(fpu::fclass64(f64::NEG_INFINITY.to_bits()), 1);
    assert_eq!(fpu::fclass64(0.0f64.to_bits()), 1 << 4);
    assert_eq!(fpu::fclass64(f64::NAN.to_bits()), 1 << 9);
}

#[test]
fn muladd_single_rounding() {
    use riscv_emu::decode::FMulAdd;
    // fma 与 Rust 原生 mul_add 一致（同为正确单舍入）
    let (a, b, c) = (1.0000000001f64, 2.0000000003f64, -2.0000000004f64);
    let (r, _) = fpu::muladd64(FMulAdd::Add, a.to_bits(), b.to_bits(), c.to_bits(), Rm::RNE);
    assert_eq!(r, a.mul_add(b, c).to_bits());
    // 经典分歧：(1+ε)(1-ε)-1 —— 精确值 = -ε²（次正规）
    // 两步：乘积舍到 1.0，加 -1 得 0；单舍入得 -ε²
    let x = f64::from_bits(0x3FF0_0000_0000_0001); // 1 + 2^-52
    let y = f64::from_bits(0x3FEF_FFFF_FFFF_FFFF); // 1 - 2^-52
    let z = -1.0f64;
    let (fused, _) = fpu::muladd64(FMulAdd::Add, x.to_bits(), y.to_bits(), z.to_bits(), Rm::RNE);
    let (p, _) = fpu::arith64(FArith::Mul, x.to_bits(), y.to_bits(), Rm::RNE);
    let (two_step, _) = fpu::arith64(FArith::Add, p, z.to_bits(), Rm::RNE);
    assert_ne!(fused, two_step, "fma 的单舍入应与两步计算不同");
    assert_eq!(fused, f64::mul_add(x, y, z).to_bits());
}
