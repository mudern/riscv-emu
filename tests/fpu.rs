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
fn overflow_window_and_inf_operands() {
    // 精确值超出 max 但 RNE 仍落在 max 的窗口：必须报 OF 且结果按模式
    let x = f64::MAX;
    let y = 2f64.powi(900); // 0 < y < max 的半 ulp
    let (r, f) = fpu::arith64(FArith::Add, x.to_bits(), y.to_bits(), Rm::RNE);
    assert_eq!(r, f64::INFINITY.to_bits(), "精确值 > max 应给出 inf（RN）");
    assert_eq!(f, fpu::FFLAGS_OF | fpu::FFLAGS_NX);

    // RTZ 溢出 → max finite
    let (r, f) = fpu::arith64(FArith::Add, x.to_bits(), y.to_bits(), Rm::RTZ);
    assert_eq!(r, f64::MAX.to_bits());
    assert_eq!(f, fpu::FFLAGS_OF | fpu::FFLAGS_NX);

    // 除法真溢出
    let (r, f) = fpu::arith64(FArith::Div, 1e308f64.to_bits(), 1e-308f64.to_bits(), Rm::RTZ);
    assert_eq!(r, f64::MAX.to_bits());
    assert_eq!(f, fpu::FFLAGS_OF | fpu::FFLAGS_NX);

    // 无穷操作数：精确结果就是 inf，无标志（曾误报 OF）
    let (r, f) = fpu::arith64(FArith::Add, f64::INFINITY.to_bits(), 1.0f64.to_bits(), Rm::RNE);
    assert_eq!(r, f64::INFINITY.to_bits());
    assert_eq!(f, 0);
    let (r, f) = fpu::arith64(FArith::Mul, f64::INFINITY.to_bits(), 2.0f64.to_bits(), Rm::RNE);
    assert_eq!(r, f64::INFINITY.to_bits());
    assert_eq!(f, 0);
}

#[test]
fn rmm_tie_side_at_power_of_two_boundary() {
    // exact = 1 + 2^-53：1.0 上方间距 2^-52（下方是 2^-24，不对称），
    // 2^-53 恰为上方间距之半 → 平局 {1.0, 1+2^-52}
    let (r, _) = fpu::arith64(FArith::Add, 1.0f64.to_bits(), 2f64.powi(-53).to_bits(), Rm::RMM);
    assert_eq!(r, (1.0f64 + 2f64.powi(-52)).to_bits(), "RMM 平局远离零（向上）");
    // 对照：RNE 平局取偶 → 1.0
    let (r, _) = fpu::arith64(FArith::Add, 1.0f64.to_bits(), 2f64.powi(-53).to_bits(), Rm::RNE);
    assert_eq!(r, 1.0f64.to_bits());
    // 下方平局：exact = 1 - 2^-54（下方间距 2^-53 之半），候选 {1-2^-53, 1.0}；
    // 正数侧远离零 = 向上 = 1.0（验证边界下方间距不对称也判定正确）
    let (r, _) = fpu::arith64(FArith::Add, 1.0f64.to_bits(), (-(2f64.powi(-54))).to_bits(), Rm::RMM);
    assert_eq!(r, 1.0f64.to_bits(), "边界下方平局 RMM 取绝对值更大者 1.0");
    // 1 - 2^-25 精确可表示（2^-25 >> 下方 ulp 2^-53）→ 原样返回
    let (r, f) = fpu::arith64(FArith::Add, 1.0f64.to_bits(), (-(2f64.powi(-25))).to_bits(), Rm::RMM);
    assert_eq!(r, (1.0f64 - 2f64.powi(-25)).to_bits());
    assert_eq!(f, 0);
}

#[test]
fn rtz_sign_of_representable_result() {
    // RTZ 符号：可表示结果原样带符号返回
    let (r, f) = fpu::arith64(FArith::Add, (-2f64.powi(-1074)).to_bits(), 0.0f64.to_bits(), Rm::RTZ);
    assert_eq!(r, (-2f64.powi(-1074)).to_bits());
    assert_eq!(f, 0);
    // 已知限制（见 fpu.rs 头注释）：精确结果低于最小次正规数时，
    // UF/NX 标志可能缺失（无误差残差自身下溢）；结果值仍正确。
}

#[test]
fn cvt_i2f_all_modes_exact() {
    // 2^63 + 2^10：f64 在 2^63 处 ulp = 2^11，半 ulp → 平局 {2^63, 2^63+2^11}
    let v = (1u64 << 63) + (1 << 10);
    let want_rne = ((1u64 << 63) as f64).to_bits();
    let want_up = ((1u64 << 63) as f64 + (1u64 << 11) as f64).to_bits();
    let (r, f) = fpu::cvt_i_to_f(v, false, false, true, Rm::RNE);
    assert_eq!(r, want_rne, "RNE 平局取偶 → 2^63");
    assert_eq!(f, fpu::FFLAGS_NX);
    let (r, _) = fpu::cvt_i_to_f(v, false, false, true, Rm::RMM);
    assert_eq!(r, want_up, "RMM 平局远离零");
    let (r, _) = fpu::cvt_i_to_f(v, false, false, true, Rm::RTZ);
    assert_eq!(r, want_rne, "RTZ 向零（已是最小候选）");
    let (r, _) = fpu::cvt_i_to_f(v, false, false, true, Rm::RUP);
    assert_eq!(r, want_up, "RUP 远离零");

    // u64 → f32 尾数 24 位：2^31 + 2^7 平局
    let v32 = (1u64 << 31) + (1 << 7);
    let want32 = ((1u32 << 31) as f32).to_bits();
    let want32_up = ((1u32 << 31) as f32 + (1u32 << 8) as f32).to_bits();
    let (r, _) = fpu::cvt_i_to_f(v32, false, false, false, Rm::RNE);
    assert_eq!(r as u32, want32, "f32 RNE 平局取偶");
    let (r, _) = fpu::cvt_i_to_f(v32, false, false, false, Rm::RMM);
    assert_eq!(r as u32, want32_up, "f32 RMM 远离零");
}

#[test]
fn cvt_f2f_modes() {
    // D→S：x = 1 + 2^-25 → f32 平局 {1.0, 1+2^-24}
    let x = 1.0 + 2f64.powi(-25);
    let one = 1.0f32.to_bits() as u64;
    let one_up = 0x3F80_0001u64; // 1 + 2^-24（不可用 f32 加法构造：会先舍入）
    let one_neg = 0x8000_0000u64 | one;
    let one_up_neg = 0xBF80_0001u64; // -(1+2^-24)
    let (r, _) = fpu::cvt_f_to_f(x.to_bits(), false, Rm::RNE);
    assert_eq!(r, one, "f32 RNE 平局取偶 → 1.0");
    let (r, _) = fpu::cvt_f_to_f(x.to_bits(), false, Rm::RMM);
    assert_eq!(r, one_up, "f32 RMM 远离零");
    // 负数 RTZ：向零（曾因符号盲判向下多退一步）
    let (r, _) = fpu::cvt_f_to_f((-x).to_bits(), false, Rm::RTZ);
    assert_eq!(r, one_neg, "负数 RTZ = -1.0");
    let (r, _) = fpu::cvt_f_to_f((-x).to_bits(), false, Rm::RDN);
    assert_eq!(r, one_up_neg, "负数 RDN 远离零");
    let (r, _) = fpu::cvt_f_to_f((-x).to_bits(), false, Rm::RUP);
    assert_eq!(r, one_neg, "负数 RUP 向零");
    // 正数 RDN / RUP
    let (r, _) = fpu::cvt_f_to_f(x.to_bits(), false, Rm::RDN);
    assert_eq!(r, one, "正数 RDN 向下");
    let (r, _) = fpu::cvt_f_to_f(x.to_bits(), false, Rm::RUP);
    assert_eq!(r, one_up, "正数 RUP 向上");
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
