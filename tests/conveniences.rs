//! `OggWriter`, `S16LeWriter` and planar input: byte-identical to the
//! one-shot `encode` / `encode_f32` (themselves oracle-tested), across
//! chunkings, sinks and sink failures.

mod common;

use common::{ALL_SIGS, Rng, Sig, ch_count, rate_hz, signal_f32, signal_i16};
use oxivorbis::{Channels, Encoder, OggWriter, Options, Quality, S16LeWriter, SampleRate};
use std::io::{self, Write};

const CONFIGS: [(SampleRate, Channels); 4] = [
    (SampleRate::HZ_44100, Channels::MONO),
    (SampleRate::HZ_44100, Channels::STEREO),
    (SampleRate::HZ_48000, Channels::MONO),
    (SampleRate::HZ_48000, Channels::STEREO),
];

fn opts(q: f32) -> Options {
    Options::default().with_quality(Quality::new(q).unwrap())
}

/// Random chunk lengths summing to `total`: mostly small and odd, with
/// some large ones and some empty ones.
fn chunking(total: usize, seed: u64) -> Vec<usize> {
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    let mut left = total;
    while left > 0 {
        let n = match rng.below(6) {
            0 => 0,
            1 => 1 + rng.below(7),
            2 | 3 => 1 + rng.below(3000),
            4 => 1 + rng.below(20_000),
            _ => 1 + rng.below(100_000),
        }
        .min(left);
        out.push(n);
        left -= n;
    }
    out
}

fn split<'a, T>(data: &'a [T], sizes: &[usize]) -> Vec<&'a [T]> {
    let mut pos = 0;
    sizes
        .iter()
        .map(|&n| {
            let s = &data[pos..pos + n];
            pos += n;
            s
        })
        .collect()
}

/// Interleaved -> planar (whole frames only).
fn deinterleave<T: Copy>(pcm: &[T], ch: usize) -> Vec<Vec<T>> {
    (0..ch)
        .map(|c| pcm.chunks_exact(ch).map(|f| f[c]).collect())
        .collect()
}

fn interleave<T: Copy>(planar: &[Vec<T>], frames: usize) -> Vec<T> {
    (0..frames)
        .flat_map(|f| planar.iter().map(move |c| c[f]))
        .collect()
}

fn decodes(ogg: &[u8]) -> usize {
    let mut r = lewton::inside_ogg::OggStreamReader::new(io::Cursor::new(ogg)).expect("headers");
    let mut n = 0;
    while let Some(p) = r.read_dec_packet_itl().expect("audio packet") {
        n += p.len();
    }
    n
}

// ---------------------------------------------------------------------------
// OggWriter
// ---------------------------------------------------------------------------

#[test]
fn ogg_writer_vec_matches_encode_across_chunkings() {
    for (k, &(rate, channels)) in CONFIGS.iter().enumerate() {
        let ch = ch_count(channels);
        for (s, &sig) in ALL_SIGS.iter().enumerate() {
            let seed = (k * 10 + s) as u64;
            let frames = 3 * rate_hz(rate) as usize + 17;
            let o = opts(if s % 2 == 0 { 5.0 } else { -1.0 });

            let pcm = signal_i16(sig, frames, ch, rate_hz(rate), seed);
            let want = oxivorbis::encode(&pcm, rate, channels, &o);
            let mut w = OggWriter::new(Vec::new(), rate, channels, &o).unwrap();
            for c in split(&pcm, &chunking(pcm.len(), seed)) {
                w.write_i16(c).unwrap();
            }
            assert_eq!(
                w.finish().unwrap(),
                want,
                "i16 {rate:?} {channels:?} {sig:?}"
            );

            let pcm = signal_f32(sig, frames, ch, rate_hz(rate), seed);
            let want = oxivorbis::encode_f32(&pcm, rate, channels, &o);
            let mut w = OggWriter::new(Vec::new(), rate, channels, &o).unwrap();
            for c in split(&pcm, &chunking(pcm.len(), seed + 1000)) {
                w.write_f32(c).unwrap();
            }
            assert_eq!(
                w.finish().unwrap(),
                want,
                "f32 {rate:?} {channels:?} {sig:?}"
            );
        }
    }
}

#[test]
fn ogg_writer_empty_and_tiny_inputs() {
    for &(rate, channels) in &CONFIGS {
        for len in [0usize, 1, 2, 3, 63, 64, 65] {
            let pcm = vec![123i16; len];
            let want = oxivorbis::encode(&pcm, rate, channels, &Options::default());
            let mut w = OggWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
            for &s in &pcm {
                w.write_i16(&[s]).unwrap();
            }
            assert_eq!(w.finish().unwrap(), want, "{rate:?} {channels:?} {len}");
        }
    }
}

/// Unique path in the temp dir, removed on drop.
struct TempPath(std::path::PathBuf);

impl TempPath {
    fn new(tag: &str) -> Self {
        let name = format!("oxivorbis-conv-{}-{tag}.ogg", std::process::id());
        TempPath(std::env::temp_dir().join(name))
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn ogg_writer_file_sinks_match_encode() {
    let (rate, channels) = (SampleRate::HZ_44100, Channels::STEREO);
    let pcm = signal_i16(Sig::Transients, 5 * 44_100, 2, 44_100, 7);
    let want = oxivorbis::encode(&pcm, rate, channels, &Options::default());

    // Plain File: every page goes straight to the file.
    let path = TempPath::new("plain");
    let file = std::fs::File::create(&path.0).unwrap();
    let mut w = OggWriter::new(file, rate, channels, &Options::default()).unwrap();
    for c in split(&pcm, &chunking(pcm.len(), 3)) {
        w.write_i16(c).unwrap();
    }
    drop(w.finish().unwrap());
    assert_eq!(std::fs::read(&path.0).unwrap(), want);

    // BufWriter<File>: finish must flush it.
    let path = TempPath::new("buffered");
    let file = io::BufWriter::new(std::fs::File::create(&path.0).unwrap());
    let mut w = OggWriter::new(file, rate, channels, &Options::default()).unwrap();
    let planar = deinterleave(&pcm, 2);
    for (l, r) in planar[0].chunks(777).zip(planar[1].chunks(777)) {
        w.write_planar_i16(&[l, r]).unwrap();
    }
    let buffered = w.finish().unwrap();
    assert!(buffered.buffer().is_empty(), "finish flushed the BufWriter");
    drop(buffered);
    assert_eq!(std::fs::read(&path.0).unwrap(), want);
}

#[test]
fn ogg_writer_forwards_pages_before_finish() {
    let (rate, channels) = (SampleRate::HZ_48000, Channels::STEREO);
    let pcm = signal_f32(Sig::Noise, 20 * 48_000, 2, 48_000, 1);
    let mut w = OggWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
    let mut sizes = Vec::new();
    for c in pcm.chunks(48_000 * 2) {
        w.write_f32(c).unwrap();
        sizes.push(w.get_ref().len());
    }
    // Pages reach the sink while streaming (the parallel build lags up to ~16 s).
    assert!(*sizes.last().unwrap() > 10_000, "{sizes:?}");
    let before = w.get_ref().len();
    let ogg = w.finish().unwrap();
    assert!(ogg.len() > before);
    assert_eq!(
        ogg,
        oxivorbis::encode_f32(&pcm, rate, channels, &Options::default())
    );
}

#[test]
fn ogg_writer_from_encoder_and_get_mut() {
    let pcm = signal_i16(Sig::Sine, 44_100, 1, 44_100, 2);
    let o = opts(3.0).with_serial(99).with_comment("TITLE", "t");
    let enc = Encoder::new(SampleRate::HZ_44100, Channels::MONO, &o);
    let mut w = OggWriter::from_encoder(enc, Vec::new());
    w.write_i16(&pcm[..1000]).unwrap();
    w.get_mut().flush().unwrap();
    w.write_i16(&pcm[1000..]).unwrap();
    assert_eq!(
        w.finish().unwrap(),
        oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::MONO, &o)
    );
}

// ---------------------------------------------------------------------------
// Failing sinks
// ---------------------------------------------------------------------------

/// Accepts `budget` bytes, then fails every write (or returns `Ok(0)`);
/// optionally fails `flush`; injects `Interrupted` every few writes.
struct FailingSink {
    data: Vec<u8>,
    budget: usize,
    zero_instead_of_err: bool,
    fail_flush: bool,
    calls: usize,
}

impl FailingSink {
    fn new(budget: usize) -> Self {
        FailingSink {
            data: Vec::new(),
            budget,
            zero_instead_of_err: false,
            fail_flush: false,
            calls: 0,
        }
    }
}

impl Write for FailingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls % 3 == 0 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.budget == 0 {
            if self.zero_instead_of_err {
                return Ok(0);
            }
            return Err(io::Error::other("sink full"));
        }
        // Short writes: at most 1000 bytes per call.
        let n = buf.len().min(self.budget).min(1000);
        self.data.extend_from_slice(&buf[..n]);
        self.budget -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other("flush failed"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn sink_errors_propagate_without_panicking() {
    let (rate, channels) = (SampleRate::HZ_44100, Channels::STEREO);
    let pcm = signal_i16(Sig::Noise, 10 * 44_100, 2, 44_100, 5);
    let want = oxivorbis::encode(&pcm, rate, channels, &Options::default());

    // Enough budget: short writes and Interrupted are retried by
    // write_all, so the output is exact.
    let mut w = OggWriter::new(
        FailingSink::new(usize::MAX),
        rate,
        channels,
        &Options::default(),
    )
    .unwrap();
    for c in pcm.chunks(5000) {
        w.write_i16(c).unwrap();
    }
    assert_eq!(w.finish().unwrap().data, want);

    for budget in [0usize, 1, 57, 4000, 30_000, want.len() - 1] {
        for zero in [false, true] {
            let mut sink = FailingSink::new(budget);
            sink.zero_instead_of_err = zero;
            let mut w = OggWriter::new(sink, rate, channels, &Options::default()).unwrap();
            let mut first_err = None;
            for c in pcm.chunks(5000) {
                // Keep going after errors: must not panic.
                if let Err(e) = w.write_i16(c) {
                    first_err.get_or_insert(e.kind());
                }
            }
            let fin = w.finish();
            let kind = first_err.or(fin.as_ref().err().map(|e| e.kind()));
            let want_kind = if zero {
                io::ErrorKind::WriteZero
            } else {
                io::ErrorKind::Other
            };
            assert_eq!(kind, Some(want_kind), "budget {budget} zero {zero}");
        }
    }

    // A failing flush surfaces from finish.
    let mut sink = FailingSink::new(usize::MAX);
    sink.fail_flush = true;
    let mut w = OggWriter::new(sink, rate, channels, &Options::default()).unwrap();
    w.write_i16(&pcm).unwrap();
    assert!(w.finish().is_err());

    // Same through S16LeWriter and io::copy (the header pages alone
    // exceed the budget, so this fails with and without `parallel`).
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut w =
        S16LeWriter::new(FailingSink::new(1000), rate, channels, &Options::default()).unwrap();
    let err = io::copy(&mut &bytes[..], &mut w).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Other);
    let _ = w.write(&bytes[..3]);
    assert!(w.finish().is_err());
}

// ---------------------------------------------------------------------------
// S16LeWriter
// ---------------------------------------------------------------------------

#[test]
fn s16le_writer_matches_encode_across_byte_chunkings() {
    for (k, &(rate, channels)) in CONFIGS.iter().enumerate() {
        let ch = ch_count(channels);
        let pcm = signal_i16(
            Sig::Transients,
            2 * rate_hz(rate) as usize,
            ch,
            rate_hz(rate),
            9,
        );
        let want = oxivorbis::encode(&pcm, rate, channels, &Options::default());
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        for seed in 0..3u64 {
            let mut w = S16LeWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
            // Odd byte counts split samples as well as frames.
            for c in split(&bytes, &chunking(bytes.len(), seed * 31 + k as u64)) {
                w.write_all(c).unwrap();
            }
            w.flush().unwrap();
            assert_eq!(
                w.finish().unwrap(),
                want,
                "{rate:?} {channels:?} seed {seed}"
            );
        }
        // One byte at a time, plus a dangling odd byte (dropped).
        let mut w = S16LeWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
        for &b in &bytes[..20_001] {
            assert_eq!(w.write(&[b]).unwrap(), 1);
        }
        assert_eq!(w.write(&[]).unwrap(), 0);
        assert_eq!(
            w.finish().unwrap(),
            oxivorbis::encode(&pcm[..10_000], rate, channels, &Options::default())
        );
    }
}

#[test]
fn s16le_writer_from_ogg_writer_and_io_copy() {
    let (rate, channels) = (SampleRate::HZ_48000, Channels::MONO);
    let pcm = signal_i16(Sig::Sine, 48_000, 1, 48_000, 4);
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut ogg = OggWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
    ogg.write_i16(&pcm[..100]).unwrap();
    let mut w = S16LeWriter::from(ogg);
    io::copy(&mut &bytes[200..], &mut w).unwrap();
    assert!(!w.get_ref().is_empty());
    let _: &mut Vec<u8> = w.get_mut();
    let mut joined = pcm[..100].to_vec();
    joined.extend_from_slice(&pcm[100..]);
    assert_eq!(
        w.finish().unwrap(),
        oxivorbis::encode(&joined, rate, channels, &Options::default())
    );
}

// ---------------------------------------------------------------------------
// Planar
// ---------------------------------------------------------------------------

#[test]
fn planar_matches_interleaved_across_chunkings() {
    for (k, &(rate, channels)) in CONFIGS.iter().enumerate() {
        let ch = ch_count(channels);
        for (s, &sig) in ALL_SIGS.iter().enumerate() {
            let seed = (k * 10 + s) as u64 + 500;
            let frames = 3 * rate_hz(rate) as usize + 5;
            let o = opts(if s % 2 == 0 { 5.0 } else { -1.0 });

            let pcm = signal_f32(sig, frames, ch, rate_hz(rate), seed);
            let want = oxivorbis::encode_f32(&pcm, rate, channels, &o);
            let planar = deinterleave(&pcm, ch);
            let mut enc = Encoder::new(rate, channels, &o);
            let mut got = Vec::new();
            let mut pos = 0;
            for n in chunking(frames, seed) {
                let chunk: Vec<&[f32]> = planar.iter().map(|c| &c[pos..pos + n]).collect();
                got.extend_from_slice(enc.write_planar_f32(&chunk));
                pos += n;
            }
            got.extend_from_slice(&enc.finish());
            assert_eq!(got, want, "f32 {rate:?} {channels:?} {sig:?}");

            let pcm = signal_i16(sig, frames, ch, rate_hz(rate), seed);
            let want = oxivorbis::encode(&pcm, rate, channels, &o);
            let planar = deinterleave(&pcm, ch);
            let mut w = OggWriter::new(Vec::new(), rate, channels, &o).unwrap();
            let mut pos = 0;
            for n in chunking(frames, seed + 1) {
                let chunk: Vec<&[i16]> = planar.iter().map(|c| &c[pos..pos + n]).collect();
                w.write_planar_i16(&chunk).unwrap();
                pos += n;
            }
            assert_eq!(
                w.finish().unwrap(),
                want,
                "i16 {rate:?} {channels:?} {sig:?}"
            );
        }
    }
}

/// Planar input with more than two channels: 5.1 (coupled and not), 3, 8
/// and 255 channels, across chunkings, plus `OggWriter` and
/// `S16LeWriter`.
#[test]
fn planar_multichannel_matches_interleaved() {
    for (k, (hz, n, frames)) in [
        (44_100u32, 6u16, 3 * 44_100 + 5),
        (48_000, 6, 2 * 48_000 + 1),
        (32_000, 6, 40_000),
        (44_100, 3, 50_000),
        (22_050, 8, 30_001),
        (44_100, 255, 5_000),
    ]
    .into_iter()
    .enumerate()
    {
        let rate = SampleRate::new(hz).unwrap();
        let channels = Channels::new(n).unwrap();
        let ch = n as usize;
        let sig = ALL_SIGS[k % ALL_SIGS.len()];
        let seed = 900 + k as u64;
        let o = opts(if k % 2 == 0 { 5.0 } else { -1.0 });

        let pcm = signal_f32(sig, frames, ch, hz, seed);
        let want = oxivorbis::encode_f32(&pcm, rate, channels, &o);
        let planar = deinterleave(&pcm, ch);
        let mut enc = Encoder::new(rate, channels, &o);
        let mut got = Vec::new();
        let mut pos = 0;
        for len in chunking(frames, seed) {
            let chunk: Vec<&[f32]> = planar.iter().map(|c| &c[pos..pos + len]).collect();
            got.extend_from_slice(enc.write_planar_f32(&chunk));
            pos += len;
        }
        got.extend_from_slice(&enc.finish());
        assert_eq!(got, want, "f32 {hz} Hz {n} ch {sig:?}");

        let pcm = signal_i16(sig, frames, ch, hz, seed);
        let want = oxivorbis::encode(&pcm, rate, channels, &o);
        let planar = deinterleave(&pcm, ch);
        let mut w = OggWriter::new(Vec::new(), rate, channels, &o).unwrap();
        let mut pos = 0;
        for len in chunking(frames, seed + 1) {
            let chunk: Vec<&[i16]> = planar.iter().map(|c| &c[pos..pos + len]).collect();
            w.write_planar_i16(&chunk).unwrap();
            pos += len;
        }
        assert_eq!(w.finish().unwrap(), want, "i16 {hz} Hz {n} ch {sig:?}");

        // Raw s16le bytes in odd-sized writes (splitting samples and frames).
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut w = S16LeWriter::new(Vec::new(), rate, channels, &o).unwrap();
        for c in bytes.chunks(4099) {
            w.write_all(c).unwrap();
        }
        assert_eq!(w.finish().unwrap(), want, "s16le {hz} Hz {n} ch {sig:?}");
    }
}

#[test]
fn planar_mixes_with_interleaved_writes() {
    // Including an interleaved write that leaves a stereo frame split: the
    // next planar left sample completes it, exactly as interleaving would.
    let (rate, channels) = (SampleRate::HZ_44100, Channels::STEREO);
    let pcm = signal_f32(Sig::Sine, 44_100, 2, 44_100, 8);
    let planar = deinterleave(&pcm, 2);
    let mut enc = Encoder::new(rate, channels, &Options::default());
    let mut got = enc.write_f32(&pcm[..2001]).to_vec(); // 1000.5 frames
    // Interleaving L[1001..], R[1001..] after the dangling L[1000] sample
    // puts L[1001] in R[1000]'s place: build the same stream by hand.
    let tail: Vec<Vec<f32>> = planar.iter().map(|c| c[1001..].to_vec()).collect();
    got.extend_from_slice(enc.write_planar_f32(&tail));
    got.extend_from_slice(&enc.finish());
    let mut manual = pcm[..2001].to_vec();
    manual.extend(interleave(&tail, tail[0].len()));
    assert_eq!(
        got,
        oxivorbis::encode_f32(&manual, rate, channels, &Options::default())
    );

    // Whole-frame alternation of all four write kinds.
    let pcm16 = signal_i16(Sig::Noise, 44_100, 2, 44_100, 9);
    let planar16 = deinterleave(&pcm16, 2);
    let mut w = OggWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
    let as_f32 = |s: i16| s as f32 / 32768.0;
    let mut f = 0;
    let mut step = 0;
    while f < 44_100 {
        let n = (1 + step * 397 % 3000).min(44_100 - f);
        match step % 4 {
            0 => w.write_i16(&pcm16[2 * f..2 * (f + n)]).unwrap(),
            1 => w
                .write_planar_i16(&[&planar16[0][f..f + n], &planar16[1][f..f + n]])
                .unwrap(),
            2 => {
                let fl: Vec<f32> = pcm16[2 * f..2 * (f + n)]
                    .iter()
                    .map(|&s| as_f32(s))
                    .collect();
                w.write_f32(&fl).unwrap()
            }
            _ => {
                let l: Vec<f32> = planar16[0][f..f + n].iter().map(|&s| as_f32(s)).collect();
                let r: Vec<f32> = planar16[1][f..f + n].iter().map(|&s| as_f32(s)).collect();
                w.write_planar_f32(&[l, r]).unwrap()
            }
        }
        f += n;
        step += 1;
    }
    assert_eq!(
        w.finish().unwrap(),
        oxivorbis::encode(&pcm16, rate, channels, &Options::default())
    );
}

#[test]
fn planar_mismatched_lengths_truncate_to_shortest() {
    let (rate, channels) = (SampleRate::HZ_48000, Channels::STEREO);
    let pcm = signal_f32(Sig::Transients, 2 * 48_000, 2, 48_000, 3);
    let planar = deinterleave(&pcm, 2);
    let mut rng = Rng(77);
    let mut enc = Encoder::new(rate, channels, &Options::default());
    let mut got = Vec::new();
    let mut used: Vec<Vec<f32>> = vec![Vec::new(), Vec::new()];
    let mut pos = 0;
    while pos < 48_000 {
        let n = (1 + rng.below(5000)).min(48_000 - pos);
        let extra = rng.below(300);
        let (l, r) = (&planar[0][pos..pos + n], &planar[1][pos..pos + n + extra]);
        // Longer channel alternates; the excess is dropped, not carried.
        let chunk = if rng.below(2) == 0 { [l, r] } else { [r, l] };
        got.extend_from_slice(enc.write_planar_f32(&chunk));
        let (a, b) = (&chunk[0][..n], &chunk[1][..n]);
        used[0].extend_from_slice(a);
        used[1].extend_from_slice(b);
        pos += n + extra;
    }
    // An empty channel truncates the call to nothing.
    got.extend_from_slice(enc.write_planar_f32(&[&planar[0][..], &[]]));
    got.extend_from_slice(&enc.finish());
    let manual = interleave(&used, used[0].len());
    assert_eq!(
        got,
        oxivorbis::encode_f32(&manual, rate, channels, &Options::default())
    );

    // i16 through OggWriter.
    let pcm16 = signal_i16(Sig::Sine, 48_000, 2, 48_000, 3);
    let p16 = deinterleave(&pcm16, 2);
    let mut w = OggWriter::new(Vec::new(), rate, channels, &Options::default()).unwrap();
    w.write_planar_i16(&[&p16[0][..], &p16[1][..30_000]])
        .unwrap();
    assert_eq!(
        w.finish().unwrap(),
        oxivorbis::encode(&pcm16[..60_000], rate, channels, &Options::default())
    );
}

#[test]
fn planar_wrong_channel_count_does_not_panic() {
    let x = signal_f32(Sig::Sine, 30_000, 1, 44_100, 1);
    let x16 = signal_i16(Sig::Sine, 30_000, 1, 44_100, 1);
    for channels in [Channels::MONO, Channels::STEREO, Channels::SURROUND_5_1] {
        for count in [0usize, 1, 2, 3, 5, 6, 7, 3000] {
            // About 30k samples in total, whatever the slice count.
            let len = 30_000 / count.max(1) - 10;
            let f: Vec<&[f32]> = (0..count).map(|i| &x[i % 7..i % 7 + len]).collect();
            let s: Vec<&[i16]> = (0..count).map(|i| &x16[i % 5..i % 5 + len]).collect();
            let mut enc = Encoder::new(SampleRate::HZ_44100, channels, &Options::default());
            let mut ogg = enc.write_planar_f32(&f).to_vec();
            ogg.extend_from_slice(enc.write_planar_i16(&s));
            ogg.extend_from_slice(&enc.finish());
            decodes(&ogg);
            if count != usize::from(channels.count()) {
                // A mismatched slice count encodes nothing.
                assert_eq!(
                    ogg,
                    oxivorbis::encode(&[], SampleRate::HZ_44100, channels, &Options::default()),
                    "{count} slices into a {channels:?} encoder"
                );
            }
        }
        // Zero slices encode nothing.
        let mut enc = Encoder::new(SampleRate::HZ_44100, channels, &Options::default());
        let none: [&[f32]; 0] = [];
        let mut ogg = enc.write_planar_f32(&none).to_vec();
        ogg.extend_from_slice(&enc.finish());
        assert_eq!(
            ogg,
            oxivorbis::encode(&[], SampleRate::HZ_44100, channels, &Options::default())
        );
    }
}

#[test]
fn planar_accepts_common_container_shapes() {
    let l = vec![0.25f32; 4096];
    let r = vec![-0.25f32; 4096];
    let mut want = Vec::new();
    for shape in 0..4 {
        let mut enc = Encoder::new(SampleRate::HZ_44100, Channels::STEREO, &Options::default());
        let mut ogg = match shape {
            0 => enc.write_planar_f32(&[&l, &r]).to_vec(),
            1 => enc.write_planar_f32(&[l.as_slice(), r.as_slice()]).to_vec(),
            2 => {
                let v: Vec<Vec<f32>> = [&l, &r].map(|c| c.to_vec()).into();
                enc.write_planar_f32(&v).to_vec()
            }
            _ => enc.write_planar_f32(&[l.clone(), r.clone()]).to_vec(),
        };
        ogg.extend_from_slice(&enc.finish());
        if shape == 0 {
            let itl: Vec<f32> = l.iter().zip(&r).flat_map(|(&a, &b)| [a, b]).collect();
            let one_shot = oxivorbis::encode_f32(
                &itl,
                SampleRate::HZ_44100,
                Channels::STEREO,
                &Options::default(),
            );
            assert_eq!(ogg, one_shot);
            want = ogg;
        } else {
            assert_eq!(ogg, want);
        }
    }
}
