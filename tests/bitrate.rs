//! Bitrate modes without the oracle: streaming and parallel invariance,
//! the `Bitrate` API and its errors, and that the modes do what they say.
//! Byte parity with libvorbis is in `parity_bitrate.rs`.

mod common;

use common::{Rng, Sig, signal_i16};
use oxivorbis::{
    Bitrate, BitrateError, BitrateMode, BitrateTuning, Channels, Encoder, OggWriter, Options,
    Quality, S16LeWriter, SampleRate,
};

const CBR: BitrateMode = BitrateMode::Average {
    kbps: 96,
    min_kbps: Some(96),
    max_kbps: Some(96),
};

/// One-shot encode in a bitrate mode that must be valid here.
fn encode_br(pcm: &[i16], rate: SampleRate, ch: Channels, o: &Options, b: Bitrate) -> Vec<u8> {
    oxivorbis::encode_with_bitrate(pcm, rate, ch, o, &b)
        .unwrap_or_else(|e| panic!("{rate} {ch} {b:?}: {e}"))
}

/// Streaming encoder in a bitrate mode that must be valid here.
fn encoder_br(rate: SampleRate, ch: Channels, o: &Options, b: Bitrate) -> Encoder {
    Encoder::with_bitrate(rate, ch, o, &b).unwrap_or_else(|e| panic!("{rate} {ch} {b:?}: {e}"))
}

/// Whether `Encoder::with_bitrate` accepts the request.
fn check(rate: SampleRate, ch: Channels, o: &Options, b: Bitrate) -> Result<(), BitrateError> {
    Encoder::with_bitrate(rate, ch, o, &b).map(|_| ())
}

/// Tone, noise and transient stretches.
fn music(secs: usize, ch: usize, rate: u32) -> Vec<i16> {
    let mut out = Vec::new();
    for (i, s) in [Sig::Noise, Sig::Sine, Sig::Transients, Sig::Silence]
        .iter()
        .cycle()
        .take(secs)
        .enumerate()
    {
        out.extend(signal_i16(*s, rate as usize, ch, rate, i as u64));
    }
    out
}

/// Total audio bytes (pages after the three header packets' pages) of an
/// Ogg Vorbis stream and the ID header's [upper, nominal, lower].
fn audio_bytes_and_header(ogg: &[u8]) -> (usize, [i32; 3]) {
    let mut pos = 0;
    let mut page = 0;
    let mut audio = 0;
    let mut hdr = [0; 3];
    while pos < ogg.len() {
        let segs = ogg[pos + 26] as usize;
        let body: usize = ogg[pos + 27..pos + 27 + segs]
            .iter()
            .map(|&s| s as usize)
            .sum();
        let start = pos + 27 + segs;
        if page == 0 {
            let id = &ogg[start..start + body];
            for (k, h) in hdr.iter_mut().enumerate() {
                *h = i32::from_le_bytes(id[16 + 4 * k..20 + 4 * k].try_into().unwrap());
            }
        } else if page >= 2 {
            audio += body;
        }
        page += 1;
        pos = start + body;
    }
    (audio, hdr)
}

#[test]
fn chunking_invariance_managed() {
    let mut rng = Rng(0xB17);
    for (hz, ch, mode) in [
        (44100, Channels::STEREO, CBR),
        (22050, Channels::MONO, BitrateMode::average(40)),
        (48000, Channels::SURROUND_5_1, BitrateMode::average(256)),
        (
            32000,
            Channels::new(4).unwrap(),
            BitrateMode::Nominal { kbps: 200 },
        ),
        (
            48000,
            Channels::STEREO,
            BitrateMode::QualityLimits {
                min_kbps: Some(64),
                max_kbps: Some(160),
            },
        ),
    ] {
        let rate = SampleRate::new(hz).unwrap();
        let pcm = music(6, ch.count() as usize, hz);
        let o = Options::default();
        let want = encode_br(&pcm, rate, ch, &o, mode.into());
        for _ in 0..3 {
            let mut enc = encoder_br(rate, ch, &o, mode.into());
            let mut out = Vec::new();
            let mut rest = &pcm[..];
            while !rest.is_empty() {
                let n = match rng.below(3) {
                    0 => 1 + rng.below(3),
                    1 => rng.below(700),
                    _ => rng.below(20_000),
                }
                .min(rest.len());
                if rng.below(2) == 0 {
                    out.extend_from_slice(enc.write(&rest[..n]));
                } else {
                    let f: Vec<f32> = rest[..n].iter().map(|&s| s as f32 / 32768.0).collect();
                    out.extend_from_slice(enc.write_f32(&f));
                }
                rest = &rest[n..];
            }
            out.extend_from_slice(&enc.finish());
            assert!(out == want, "{hz} {ch:?} {mode:?}");
        }
    }
}

/// The parallel pipeline chooses packetblobs serially, in stream order,
/// so the batch size can't change the bytes.
#[cfg(feature = "parallel")]
#[test]
fn parallel_batch_sizes_give_identical_bytes_managed() {
    let rate = SampleRate::HZ_44100;
    let pcm = music(8, 2, 44100);
    for mode in [CBR, BitrateMode::Nominal { kbps: 128 }] {
        let o = Options::default();
        let want = encode_br(&pcm, rate, Channels::STEREO, &o, mode.into());
        for batch in [1usize, 5, 256] {
            let mut enc = encoder_br(rate, Channels::STEREO, &o, mode.into());
            enc.set_parallel_batch(batch);
            let mut out = Vec::new();
            for c in pcm.chunks(9999) {
                out.extend_from_slice(enc.write(c));
            }
            out.extend_from_slice(&enc.finish());
            assert!(out == want, "{mode:?} batch {batch}");
        }
    }
}

/// The managed modes land near their targets and announce them.
#[test]
fn bitrates_hit_their_targets() {
    let rate = SampleRate::HZ_44100;
    let secs = 20;
    let pcm = music(secs, 2, 44100);
    let kbps = |o: &Options, b: Option<BitrateMode>| {
        let ogg = match b {
            Some(b) => encode_br(&pcm, rate, Channels::STEREO, o, b.into()),
            None => oxivorbis::encode(&pcm, rate, Channels::STEREO, o),
        };
        let (bytes, hdr) = audio_bytes_and_header(&ogg);
        (bytes as f64 * 8.0 / secs as f64 / 1000.0, hdr)
    };
    // CBR-like: within the 2 s reservoir of 96 kb/s.
    let (k, hdr) = kbps(&Options::default(), Some(CBR));
    assert!((k - 96.0).abs() < 96.0 * 2.0 / secs as f64 + 1.0, "{k}");
    assert_eq!(hdr, [96_000, 96_000, 96_000]);
    // ABR
    let (k, hdr) = kbps(&Options::default(), Some(BitrateMode::average(160)));
    assert!((k - 160.0).abs() < 16.0, "{k}");
    assert_eq!(hdr, [-1, 160_000, -1]);
    // A hard maximum well below what quality 8 wants.
    let q8 = Options::default().with_quality(Quality::new(8.0).unwrap());
    let (vbr, _) = kbps(&q8, None);
    let capped = BitrateMode::QualityLimits {
        min_kbps: None,
        max_kbps: Some(128),
    };
    let (k, hdr) = kbps(&q8, Some(capped));
    // The reservoir is twice q8's nominal bitrate (2 x 256 kb/s).
    let slack = 2.0 * hdr[1] as f64 / 1000.0 / secs as f64;
    assert!(
        vbr > 150.0 && k < vbr && k <= 128.0 + slack,
        "q8 {vbr} capped {k}"
    );
    assert_eq!(hdr[0], 128_000);
}

#[test]
fn bitrate_api() {
    let b = Bitrate::new(CBR);
    assert_eq!(b.mode, CBR);
    assert_eq!(b.tuning, BitrateTuning::default());
    assert_eq!(Bitrate::from(CBR), b);
    let t = BitrateTuning::default().with_reservoir_bias(0.5);
    assert_eq!(b.with_tuning(t).tuning, t);
    let (r, c) = (SampleRate::HZ_44100, Channels::STEREO);
    let pcm = music(2, 2, 44100);
    let o = Options::default();
    // Streaming, one-shot i16 and f32, and the writers agree.
    let want = encode_br(&pcm, r, c, &o, b);
    let mut enc = encoder_br(r, c, &o, b);
    let mut streamed = enc.write(&pcm).to_vec();
    streamed.extend_from_slice(&enc.finish());
    assert!(streamed == want);
    let f: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
    assert!(oxivorbis::encode_f32_with_bitrate(&f, r, c, &o, &b).unwrap() == want);
    let mut w = OggWriter::with_bitrate(Vec::new(), r, c, &o, &b).unwrap();
    w.write_i16(&pcm).unwrap();
    assert!(w.finish().unwrap() == want);
    let mut w = S16LeWriter::with_bitrate(Vec::new(), r, c, &o, &b).unwrap();
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::io::Write::write_all(&mut w, &bytes).unwrap();
    assert!(w.finish().unwrap() == want);
    // VBR is untouched by the bitrate API.
    assert!(oxivorbis::encode(&pcm, r, c, &o) != want);
}

/// One `Options` and one `Bitrate`, reused at a rate where the request
/// is valid and at one where it isn't: `Err`, never a panic, from every
/// bitrate constructor.
#[test]
fn reuse_across_rates_returns_errors() {
    let q8 = Options::default().with_quality(Quality::new(8.0).unwrap());
    let cases = [
        (Bitrate::new(CBR), Channels::STEREO),
        (BitrateMode::average(320).into(), Channels::SURROUND_5_1),
        (
            BitrateMode::QualityLimits {
                min_kbps: None,
                max_kbps: Some(200),
            }
            .into(),
            Channels::STEREO,
        ),
    ];
    for (b, ch) in cases {
        let n = ch.count() as usize;
        let ok = SampleRate::HZ_44100;
        let pcm = music(1, n, 44100);
        let f: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        assert!(check(ok, ch, &q8, b).is_ok(), "{b:?}");
        assert!(oxivorbis::encode_with_bitrate(&pcm, ok, ch, &q8, &b).is_ok());
        for bad in [SampleRate::HZ_96000, SampleRate::new(7999).unwrap()] {
            let e = check(bad, ch, &q8, b).unwrap_err();
            assert!(
                matches!(e, BitrateError::OutOfRange { rate, .. } if rate == bad),
                "{e:?}"
            );
            let e2 = oxivorbis::encode_with_bitrate(&pcm, bad, ch, &q8, &b).unwrap_err();
            assert_eq!(e2, e);
            let e3 = oxivorbis::encode_f32_with_bitrate(&f, bad, ch, &q8, &b).unwrap_err();
            assert_eq!(e3, e);
            let io = OggWriter::with_bitrate(Vec::new(), bad, ch, &q8, &b)
                .err()
                .expect("rejected");
            assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput);
            let inner = io.get_ref().unwrap().downcast_ref::<BitrateError>();
            assert_eq!(inner, Some(&e));
            assert!(S16LeWriter::with_bitrate(Vec::new(), bad, ch, &q8, &b).is_err());
            // ... while VBR with the same options works at that rate.
            let ogg = oxivorbis::encode(&pcm, bad, ch, &q8);
            assert_eq!(&ogg[..4], b"OggS");
        }
        // And still fine at the first rate afterwards.
        assert!(oxivorbis::encode_with_bitrate(&pcm, ok, ch, &q8, &b).is_ok());
    }
    // The channel count matters too: 1440 kb/s is in range for 5.1 at
    // 48 kHz, not for stereo.
    let b = BitrateMode::average(1440).into();
    let r = SampleRate::HZ_48000;
    assert!(check(r, Channels::SURROUND_5_1, &q8, b).is_ok());
    assert!(matches!(
        check(r, Channels::STEREO, &q8, b),
        Err(BitrateError::OutOfRange { .. })
    ));
}

#[test]
fn errors() {
    let r = SampleRate::HZ_44100;
    let o = Options::default();
    let e = check(r, Channels::STEREO, &o, BitrateMode::average(600).into()).unwrap_err();
    assert_eq!(
        e,
        BitrateError::OutOfRange {
            bps: 600_000,
            rate: r,
            channels: Channels::STEREO,
            supported: Some(45..=500),
        }
    );
    assert_eq!(
        e.to_string(),
        "no libvorbis setup for 600000 b/s at 44100 Hz stereo (supported: 45..=500 kb/s)"
    );
    let e = check(
        SampleRate::HZ_192000,
        Channels::MONO,
        &o,
        BitrateMode::average(128).into(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("8000..=50000 Hz"), "{e}");
    let e = check(
        SampleRate::HZ_48000,
        Channels::SURROUND_5_1,
        &o,
        BitrateMode::average(2000).into(),
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "no libvorbis setup for 2000000 b/s at 48000 Hz 6 channels (supported: 84..=1440 kb/s)"
    );
    for m in [
        BitrateMode::Average {
            kbps: 128,
            min_kbps: Some(160),
            max_kbps: None,
        },
        BitrateMode::Average {
            kbps: 128,
            min_kbps: None,
            max_kbps: Some(96),
        },
        BitrateMode::Limits {
            min_kbps: Some(160),
            max_kbps: Some(96),
        },
        BitrateMode::Limits {
            min_kbps: None,
            max_kbps: None,
        },
        BitrateMode::Nominal { kbps: 0 },
        BitrateMode::average(u32::MAX),
    ] {
        let e = check(r, Channels::STEREO, &o, m.into());
        assert!(matches!(e, Err(BitrateError::Invalid(_))), "{m:?}: {e:?}");
    }
    for t in [
        BitrateTuning::default().with_reservoir_bias(-0.1),
        BitrateTuning::default().with_reservoir_bias(f64::NAN),
        BitrateTuning::default().with_average_damping(0.0),
        BitrateTuning::default().with_average_damping(-1.0),
    ] {
        let e = check(r, Channels::STEREO, &o, Bitrate::new(CBR).with_tuning(t));
        assert!(matches!(e, Err(BitrateError::Invalid(_))), "{t:?}: {e:?}");
    }
}

/// Every mode at every rate that has bitrate setups, at the ends of the
/// range, on short and empty input: no panics, valid-looking streams.
#[test]
fn edges_do_not_panic() {
    for hz in [
        8000, 8999, 9000, 14999, 15000, 18999, 19000, 25999, 26000, 39999, 40000, 50000, 50001,
        70000,
    ] {
        let rate = SampleRate::new(hz).unwrap();
        for n in [1, 2, 3, 6, 8] {
            let ch = Channels::new(n).unwrap();
            if hz > 50000 && n != 6 {
                assert_eq!(BitrateMode::supported_kbps(rate, ch), None);
                continue;
            }
            let r = BitrateMode::supported_kbps(rate, ch).unwrap();
            let (lo, hi) = (*r.start(), *r.end());
            for mode in [
                BitrateMode::Nominal { kbps: lo },
                BitrateMode::average(hi),
                BitrateMode::Average {
                    kbps: lo,
                    min_kbps: Some(lo),
                    max_kbps: Some(lo),
                },
                BitrateMode::Limits {
                    min_kbps: Some(hi),
                    max_kbps: None,
                },
                BitrateMode::QualityLimits {
                    min_kbps: Some(1),
                    max_kbps: Some(1),
                },
            ] {
                let o = Options::default();
                for frames in [0usize, 1, 3000] {
                    let pcm = signal_i16(Sig::Noise, frames, ch.count() as usize, hz, 9);
                    let ogg = encode_br(&pcm, rate, ch, &o, mode.into());
                    assert_eq!(&ogg[..4], b"OggS", "{hz} {ch:?} {mode:?} {frames}");
                }
            }
        }
    }
}

/// `EncoderTuning`: the default changes nothing, NaN is ignored, and the
/// values libvorbis clamps encode like the clamped value.
#[test]
fn encoder_tuning_defaults_nan_and_clamps() {
    use oxivorbis::EncoderTuning;
    let (r, c) = (SampleRate::HZ_44100, Channels::STEREO);
    let pcm = music(3, 2, 44100);
    let enc = |t: EncoderTuning| {
        let o = Options::default().with_encoder_tuning(t);
        assert_eq!(format!("{:?}", o.encoder_tuning()), format!("{t:?}"));
        oxivorbis::encode(&pcm, r, c, &o)
    };
    let plain = oxivorbis::encode(&pcm, r, c, &Options::default());
    assert!(enc(EncoderTuning::default()) == plain);
    let nan = EncoderTuning::default()
        .with_lowpass_khz(f64::NAN)
        .with_impulse_noisetune(f64::NAN);
    assert!(enc(nan) == plain);
    let lp = |k| enc(EncoderTuning::default().with_lowpass_khz(k));
    assert!(lp(150.0) == lp(99.0) && lp(0.1) == lp(2.0) && lp(12.0) != plain);
    let it = |d| enc(EncoderTuning::default().with_impulse_noisetune(d));
    assert!(it(5.0) == plain && it(-99.0) == it(-15.0));
    assert!(enc(EncoderTuning::default().with_coupling_disabled()) != plain);
}
