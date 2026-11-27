//! Sample formats and their conversion to what the encoder takes.
//!
//! | Format | Encoder path | Conversion |
//! |---|---|---|
//! | `s16` | `write` (i16) | none; the library scales by 1/32768 |
//! | `u8`  | `write_f32` | `(x - 128) / 128`, exact |
//! | `s24` | `write_f32` | `x / 2^23`, exact (24 bits fit an f32 mantissa) |
//! | `s32` | `write_f32` | `x / 2^31`, computed exactly in f64, rounded once to f32 |
//! | `f32` | `write_f32` | passed through unchanged |
//! | `f64` | `write_f32` | rounded once to the nearest f32 |
//!
//! The 8-bit conversion gives exactly the floats the i16 path would for
//! `(x - 128) << 8`, and the 24- and 32-bit ones exactly what it would
//! for their top 16 bits when the low bits are zero.

/// A little-endian interleaved PCM sample format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    U8,
    S16,
    S24,
    S32,
    F32,
    F64,
}

impl SampleFormat {
    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::S16 => 2,
            SampleFormat::S24 => 3,
            SampleFormat::S32 | SampleFormat::F32 => 4,
            SampleFormat::F64 => 8,
        }
    }

    /// Short description for the summary line.
    pub fn describe(self) -> &'static str {
        match self {
            SampleFormat::U8 => "8-bit",
            SampleFormat::S16 => "16-bit",
            SampleFormat::S24 => "24-bit",
            SampleFormat::S32 => "32-bit",
            SampleFormat::F32 => "float32",
            SampleFormat::F64 => "float64",
        }
    }
}

/// Decode little-endian s16 samples. `bytes.len()` must be even.
pub fn to_i16(bytes: &[u8], out: &mut Vec<i16>) {
    out.clear();
    out.extend(
        bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]])),
    );
}

/// Decode samples of `fmt` to the f32 values handed to `write_f32`.
/// `bytes.len()` must be a multiple of `fmt.bytes()`.
pub fn to_f32(fmt: SampleFormat, bytes: &[u8], out: &mut Vec<f32>) {
    out.clear();
    match fmt {
        SampleFormat::U8 => out.extend(bytes.iter().map(|&b| (b as i32 - 128) as f32 / 128.0)),
        SampleFormat::S16 => out.extend(
            bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0),
        ),
        SampleFormat::S24 => out.extend(bytes.chunks_exact(3).map(|c| {
            // Sign-extend by placing the 24 bits at the top of an i32.
            let x = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
            x as f32 / 8_388_608.0
        })),
        SampleFormat::S32 => out.extend(bytes.chunks_exact(4).map(|c| {
            let x = i32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            (x as f64 / 2_147_483_648.0) as f32
        })),
        SampleFormat::F32 => out.extend(
            bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        ),
        SampleFormat::F64 => {
            out.extend(bytes.chunks_exact(8).map(|c| {
                f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) as f32
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(fmt: SampleFormat, bytes: &[u8]) -> Vec<f32> {
        let mut v = Vec::new();
        to_f32(fmt, bytes, &mut v);
        v
    }

    #[test]
    fn integer_extremes() {
        assert_eq!(
            conv(SampleFormat::U8, &[0, 128, 255]),
            [-1.0, 0.0, 127.0 / 128.0]
        );
        assert_eq!(
            conv(SampleFormat::S24, &[0, 0, 0x80, 0xFF, 0xFF, 0x7F, 1, 0, 0]),
            [-1.0, 8_388_607.0 / 8_388_608.0, 1.0 / 8_388_608.0]
        );
        let s32: Vec<u8> = [i32::MIN, i32::MAX, -1]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        // i32::MAX / 2^31 rounds up to 1.0 in f32.
        assert_eq!(
            conv(SampleFormat::S32, &s32),
            [-1.0, 1.0, -1.0 / 2_147_483_648.0]
        );
    }

    #[test]
    fn s32_rounds_once() {
        for x in [i32::MAX, 0x0123_4567, -0x7654_3211, 16_777_217, -33_554_435] {
            let got = conv(SampleFormat::S32, &x.to_le_bytes())[0];
            assert_eq!(got, x as f32 / 2_147_483_648.0, "{x}");
        }
    }

    #[test]
    fn narrower_formats_match_the_i16_path() {
        // 8-bit and zero-padded 24/32-bit give the i16 path's floats.
        for s in [i16::MIN, -12345, -1, 0, 1, 255, 32767] {
            let want = s as f32 / 32768.0;
            let b = s.to_le_bytes();
            assert_eq!(conv(SampleFormat::S24, &[0, b[0], b[1]])[0], want);
            assert_eq!(conv(SampleFormat::S32, &[0, 0, b[0], b[1]])[0], want);
            assert_eq!(conv(SampleFormat::S16, &b)[0], want);
        }
        for u in 0..=255u8 {
            let s = ((u as i16) - 128) << 8;
            assert_eq!(conv(SampleFormat::U8, &[u])[0], s as f32 / 32768.0);
        }
    }

    #[test]
    fn floats() {
        let v = [0.25f32, -1.5, f32::MIN_POSITIVE];
        let b: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(conv(SampleFormat::F32, &b), v);
        let d = [0.1f64, -2.0];
        let b: Vec<u8> = d.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(conv(SampleFormat::F64, &b), [0.1f32, -2.0]);
    }
}
