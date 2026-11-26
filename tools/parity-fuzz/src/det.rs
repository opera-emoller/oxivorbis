//! Platform-independent transcendental functions for the signal generators.
//!
//! `f64::sin`, `exp`, `ln` and `powf` call the host libm, whose last-bit
//! results differ between glibc, macOS and MSVC, so the same seed used to
//! render slightly different PCM on each platform. These versions use only
//! IEEE-754 `+ - * /`, `round` and `sqrt` (all correctly rounded and
//! deterministic everywhere; Rust never contracts them into FMAs), so a
//! seed renders bit-identical PCM on every host. Accuracy is a few ulp,
//! which is plenty for test signals.

use std::f64::consts::{FRAC_PI_2, LN_2, SQRT_2};

// Cody-Waite split of pi/2 and ln 2: the high parts have trailing zero
// bits so `k * HI` is exact for the |k| we see.
const PIO2_HI: f64 = 1.570_796_326_734_125_6; // 0x3FF921FB54400000
const PIO2_LO: f64 = 6.077_100_506_506_192e-11; // pi/2 - PIO2_HI
const LN2_HI: f64 = 6.931_471_803_691_238e-1; // 0x3FE62E42FEE00000
const LN2_LO: f64 = 1.908_214_929_270_587_7e-10; // ln2 - LN2_HI

/// Taylor coefficients: SIN_C[k] = (-1)^k / (2k+1)!, COS_C[k] = (-1)^k / (2k)!.
const SIN_C: [f64; 9] = taylor(1);
const COS_C: [f64; 9] = taylor(0);
/// EXP_C[n] = 1 / n!.
const EXP_C: [f64; 15] = {
    let mut c = [0.0; 15];
    let mut f = 1.0;
    let mut n = 0;
    while n < 15 {
        if n > 0 {
            f *= n as f64;
        }
        c[n] = 1.0 / f;
        n += 1;
    }
    c
};

const fn taylor(first: u32) -> [f64; 9] {
    let mut c = [0.0; 9];
    let mut k = 0;
    while k < 9 {
        let n = 2 * k as u32 + first;
        let mut f = 1.0;
        let mut i = 2;
        while i <= n {
            f *= i as f64;
            i += 1;
        }
        c[k] = if k % 2 == 0 { 1.0 / f } else { -1.0 / f };
        k += 1;
    }
    c
}

/// sin and cos of `r` for |r| <= pi/4.
fn sincos_kernel(r: f64) -> (f64, f64) {
    let r2 = r * r;
    let (mut s, mut c) = (0.0, 0.0);
    for k in (0..9).rev() {
        s = s * r2 + SIN_C[k];
        c = c * r2 + COS_C[k];
    }
    (s * r, c)
}

/// (sin x, cos x).
pub fn sin_cos(x: f64) -> (f64, f64) {
    if !x.is_finite() {
        return (f64::NAN, f64::NAN);
    }
    let q = (x / FRAC_PI_2).round();
    let r = (x - q * PIO2_HI) - q * PIO2_LO;
    let (s, c) = sincos_kernel(r);
    // q mod 4, also for negative q and |q| beyond i64 (where it is even).
    let quadrant = (q - 4.0 * (q / 4.0).floor()) as i64;
    match quadrant {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

pub fn sin(x: f64) -> f64 {
    sin_cos(x).0
}

pub fn cos(x: f64) -> f64 {
    sin_cos(x).1
}

/// 2^k for integer k, built from the exponent bits.
fn pow2i(k: i64) -> f64 {
    if k > 1023 {
        f64::INFINITY
    } else if k >= -1022 {
        f64::from_bits(((k + 1023) as u64) << 52)
    } else if k >= -1074 {
        f64::from_bits(1u64 << (k + 1074))
    } else {
        0.0
    }
}

pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.8 {
        return f64::INFINITY;
    }
    if x < -745.2 {
        return 0.0;
    }
    let k = (x / LN_2).round();
    let r = (x - k * LN2_HI) - k * LN2_LO; // |r| <= ~0.35
    let mut p = 0.0;
    for c in EXP_C.iter().rev() {
        p = p * r + c;
    }
    // Split the scaling so subnormal results don't lose the whole mantissa.
    let k = k as i64;
    let h = k / 2;
    p * pow2i(h) * pow2i(k - h)
}

pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return x;
    }
    let (mut x, mut e) = (x, 0i64);
    if x < f64::MIN_POSITIVE {
        x *= pow2i(54);
        e -= 54;
    }
    let bits = x.to_bits();
    e += ((bits >> 52) & 0x7ff) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > SQRT_2 {
        m /= 2.0;
        e += 1;
    }
    // ln m = 2 atanh(f), f = (m-1)/(m+1), |f| <= 0.172.
    let f = (m - 1.0) / (m + 1.0);
    let f2 = f * f;
    let mut s = 0.0;
    for n in (0..12).rev() {
        s = s * f2 + 1.0 / (2 * n + 1) as f64;
    }
    let ef = e as f64;
    ef * LN2_HI + (2.0 * f * s + ef * LN2_LO)
}

/// a^b for a > 0.
pub fn powf(a: f64, b: f64) -> f64 {
    if a == 1.0 || b == 0.0 {
        return 1.0;
    }
    exp(b * ln(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_to_libm() {
        let mut x = -50.0;
        while x < 50.0 {
            assert!((sin(x) - x.sin()).abs() < 1e-14, "sin {x}");
            assert!((cos(x) - x.cos()).abs() < 1e-14, "cos {x}");
            x += 0.0137;
        }
        for &x in &[-700.0, -20.0, -1.0, -1e-9, 0.0, 0.3, 1.0, 7.5, 300.0, 709.0] {
            let rel = (exp(x) - x.exp()).abs() / x.exp();
            assert!(rel < 1e-14, "exp {x}: {rel}");
        }
        for &x in &[1e-310, 1e-20, 0.5, 1.0, 1.5, 2.0, 10.0, 1e100] {
            assert!((ln(x) - x.ln()).abs() < 1e-13, "ln {x}");
        }
        assert!((powf(10.0, -4.5) - 10f64.powf(-4.5)).abs() < 1e-18);
    }
}
