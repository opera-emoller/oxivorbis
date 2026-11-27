//! End-to-end tests of the `oxivorbis` binary: WAVs in every supported
//! format are generated, encoded by the CLI, compared byte for byte with
//! the library's `encode`/`encode_f32` on the same samples, and decoded
//! with lewton.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::io::Read;
use std::path::Path;
use std::process::{Output, Stdio};
use tempfile::{TempDir, tempdir};

// ---- library calls (the only place the tests touch the oxivorbis API) ----

fn opts(q: f32) -> oxivorbis::Options {
    oxivorbis::Options::default().with_quality(oxivorbis::Quality::new(q).unwrap())
}

fn lib_i16(pcm: &[i16], rate: u32, ch: u16, o: &oxivorbis::Options) -> Vec<u8> {
    let r = oxivorbis::SampleRate::try_from(rate).unwrap();
    let c = oxivorbis::Channels::try_from(ch).unwrap();
    oxivorbis::encode(pcm, r, c, o)
}

fn lib_f32(pcm: &[f32], rate: u32, ch: u16, o: &oxivorbis::Options) -> Vec<u8> {
    let r = oxivorbis::SampleRate::try_from(rate).unwrap();
    let c = oxivorbis::Channels::try_from(ch).unwrap();
    oxivorbis::encode_f32(pcm, r, c, o)
}

// ---- helpers ----

fn cmd() -> assert_cmd::Command {
    cargo_bin_cmd!("oxivorbis")
}

fn scratch() -> TempDir {
    tempdir().unwrap()
}

/// Run the binary; stdin is `stdin` if given, else empty (assert_cmd
/// always pipes it).
fn run(args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut c = cmd();
    c.args(args);
    if let Some(d) = stdin {
        c.write_stdin(d.to_vec());
    }
    c.output().unwrap()
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[track_caller]
fn ok(o: &Output) {
    assert!(o.status.success(), "exit {:?}: {}", o.status, stderr(o));
    assert!(!stderr(o).contains("panicked"), "{}", stderr(o));
}

#[track_caller]
fn fails(o: &Output, code: i32, msg: &str) {
    assert_eq!(o.status.code(), Some(code), "stderr: {}", stderr(o));
    assert!(stderr(o).contains(msg), "want {msg:?} in: {}", stderr(o));
    assert!(!stderr(o).contains("panicked"), "{}", stderr(o));
}

fn signal(frames: usize, ch: usize) -> Vec<f64> {
    let mut seed = 0x1234_5678u32;
    let mut v = Vec::with_capacity(frames * ch);
    for i in 0..frames {
        for c in 0..ch {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
            let t = i as f64 / 44_100.0;
            let s = 0.5 * (t * 440.0 * (c + 1) as f64 * std::f64::consts::TAU).sin()
                + 0.2 * (t * 3_100.0 * std::f64::consts::TAU).sin()
                + 0.1 * noise;
            v.push(s.clamp(-1.0, 0.999));
        }
    }
    v
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fmt {
    U8,
    S16,
    S24,
    S32,
    F32,
    F64,
}

impl Fmt {
    fn bytes(self) -> u16 {
        match self {
            Fmt::U8 => 1,
            Fmt::S16 => 2,
            Fmt::S24 => 3,
            Fmt::S32 | Fmt::F32 => 4,
            Fmt::F64 => 8,
        }
    }

    fn float(self) -> bool {
        matches!(self, Fmt::F32 | Fmt::F64)
    }
}

/// Sample bytes for `fmt`, plus the exact values the encoder should see:
/// `Ok(i16)` for 16-bit (the i16 path), `Err(f32)` otherwise.
fn quantize(fmt: Fmt, sig: &[f64]) -> (Vec<u8>, Result<Vec<i16>, Vec<f32>>) {
    let mut bytes = Vec::new();
    let mut f = Vec::new();
    let mut i = Vec::new();
    for &s in sig {
        match fmt {
            Fmt::U8 => {
                let x = ((s * 128.0).round() as i32 + 128).clamp(0, 255);
                bytes.push(x as u8);
                f.push((x - 128) as f32 / 128.0);
            }
            Fmt::S16 => {
                let x = (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                bytes.extend(x.to_le_bytes());
                i.push(x);
            }
            Fmt::S24 => {
                let x = (s * 8_388_608.0).round() as i32;
                bytes.extend(&x.to_le_bytes()[..3]);
                f.push(x as f32 / 8_388_608.0);
            }
            Fmt::S32 => {
                let x = (s * 2_147_483_648.0).round() as i32;
                bytes.extend(x.to_le_bytes());
                f.push((x as f64 / 2_147_483_648.0) as f32);
            }
            Fmt::F32 => {
                bytes.extend((s as f32).to_le_bytes());
                f.push(s as f32);
            }
            Fmt::F64 => {
                bytes.extend(s.to_le_bytes());
                f.push(s as f32);
            }
        }
    }
    let vals = if fmt == Fmt::S16 { Ok(i) } else { Err(f) };
    (bytes, vals)
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

#[derive(Default, Clone, Copy)]
struct WavOpts {
    extensible: bool,
    /// Valid bits for WAVE_FORMAT_EXTENSIBLE (default: container size).
    valid_bits: Option<u16>,
    /// Add LIST (odd size), fact and JUNK chunks before data, and a
    /// trailing chunk after it.
    extra_chunks: bool,
    /// Override the data chunk size field.
    data_size: Option<u32>,
    rf64: bool,
    tag: Option<u16>,
    channels_field: Option<u16>,
}

fn fmt_chunk(fmt: Fmt, ch: u16, rate: u32, o: &WavOpts) -> Vec<u8> {
    let bits = fmt.bytes() * 8;
    let ba = ch * fmt.bytes();
    let base_tag = if fmt.float() { 3u16 } else { 1 };
    let mut b = Vec::new();
    b.extend(
        o.tag
            .unwrap_or(if o.extensible { 0xFFFE } else { base_tag })
            .to_le_bytes(),
    );
    b.extend(o.channels_field.unwrap_or(ch).to_le_bytes());
    b.extend(rate.to_le_bytes());
    b.extend((rate * ba as u32).to_le_bytes());
    b.extend(ba.to_le_bytes());
    b.extend(bits.to_le_bytes());
    if o.extensible {
        b.extend(22u16.to_le_bytes());
        b.extend(o.valid_bits.unwrap_or(bits).to_le_bytes());
        b.extend((if ch == 1 { 4u32 } else { 3 }).to_le_bytes());
        b.extend(base_tag.to_le_bytes());
        b.extend([
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    b
}

fn wav(fmt: Fmt, ch: u16, rate: u32, data: &[u8], o: WavOpts) -> Vec<u8> {
    let mut body = Vec::new();
    if o.rf64 {
        let mut ds64 = Vec::new();
        ds64.extend(0u64.to_le_bytes());
        ds64.extend((data.len() as u64).to_le_bytes());
        ds64.extend(0u64.to_le_bytes());
        ds64.extend(0u32.to_le_bytes());
        body.extend(chunk(b"ds64", &ds64));
    }
    if o.extra_chunks {
        body.extend(chunk(b"JUNK", &[0; 28]));
    }
    body.extend(chunk(b"fmt ", &fmt_chunk(fmt, ch, rate, &o)));
    if o.extra_chunks {
        body.extend(chunk(b"fact", &[1, 2, 3, 4]));
        body.extend(chunk(b"LIST", b"INFOISFT\x05\0\0\0test\0"));
    }
    let size = o
        .data_size
        .unwrap_or(if o.rf64 { u32::MAX } else { data.len() as u32 });
    body.extend(b"data");
    body.extend(size.to_le_bytes());
    body.extend(data);
    if data.len() % 2 == 1 {
        body.push(0);
    }
    if o.extra_chunks {
        body.extend(chunk(b"LIST", b"INFOIART\x03\0\0\0me\0"));
    }
    let mut v = if o.rf64 {
        b"RF64".to_vec()
    } else {
        b"RIFF".to_vec()
    };
    v.extend(
        if o.rf64 {
            u32::MAX
        } else {
            body.len() as u32 + 4
        }
        .to_le_bytes(),
    );
    v.extend(b"WAVE");
    v.extend(body);
    v
}

struct Decoded {
    channels: u8,
    rate: u32,
    frames: u64,
    granule: u64,
    comments: Vec<(String, String)>,
    serial: u32,
}

fn decode(ogg: &[u8]) -> Decoded {
    let mut r = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(ogg.to_vec()))
        .expect("lewton open");
    let channels = r.ident_hdr.audio_channels;
    let mut frames = 0u64;
    while let Some(pkt) = r.read_dec_packet_itl().expect("lewton decode") {
        frames += (pkt.len() / channels as usize) as u64;
    }
    // Last page's granule position.
    let mut granule = 0;
    let mut i = 0;
    while let Some(off) = ogg[i..].windows(4).position(|w| w == b"OggS") {
        let at = i + off;
        granule = u64::from_le_bytes(ogg[at + 6..at + 14].try_into().unwrap());
        i = at + 4;
    }
    Decoded {
        channels,
        rate: r.ident_hdr.audio_sample_rate,
        frames,
        granule,
        comments: r.comment_hdr.comment_list.clone(),
        serial: u32::from_le_bytes(ogg[14..18].try_into().unwrap()),
    }
}

#[track_caller]
fn check_decodes(ogg: &[u8], ch: u16, rate: u32, frames: usize) {
    let d = decode(ogg);
    assert_eq!(d.channels as u16, ch);
    assert_eq!(d.rate, rate);
    assert_eq!(d.granule, frames as u64, "granule = input frames");
    // lewton trims the final packet to the granule only when the stream
    // has more than one audio page; a very short stream decodes with up to
    // one block of padding.
    if frames > 10_000 {
        assert_eq!(d.frames, frames as u64, "decoded frames");
    } else {
        assert!((frames as u64..frames as u64 + 2048).contains(&d.frames));
    }
}

/// Encode `wav_bytes` through the CLI as a file and return the output.
fn encode_file(dir: &Path, name: &str, wav_bytes: &[u8], extra: &[&str]) -> Vec<u8> {
    let input = dir.join(format!("{name}.wav"));
    std::fs::write(&input, wav_bytes).unwrap();
    let mut args = vec!["-Q"];
    args.extend_from_slice(extra);
    args.push(p(&input));
    ok(&run(&args, None));
    std::fs::read(dir.join(format!("{name}.ogg"))).unwrap()
}

// ---- tests ----

#[test]
fn help_and_version() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--quality"))
        .stdout(predicate::str::contains("Exit status"));
    cmd()
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: oxivorbis"));
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(concat!(
            "oxivorbis ",
            env!("CARGO_PKG_VERSION")
        )));
    for (kind, marker) in [
        ("man", ".TH oxivorbis 1"),
        ("complete-bash", "_oxivorbis()"),
        ("complete-zsh", "#compdef oxivorbis"),
        ("complete-fish", "complete -c oxivorbis"),
    ] {
        cmd()
            .args(["--generate", kind])
            .assert()
            .success()
            .stdout(predicate::str::contains(marker));
    }
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &[][..],
        &["--bogus", "x.wav"],
        &["-q", "11", "x.wav"],
        &["-q", "loud", "x.wav"],
        &["-q"],
        &["-o", "o.ogg", "a.wav", "b.wav"],
        &["--raw", "-"],
        &["-R", "44100", "x.wav"],
        &["-c", "notag", "x.wav"],
        &["-", "-"],
    ] {
        cmd()
            .args(args)
            .assert()
            .code(2)
            .stdout("")
            .stderr(predicate::str::contains("error:"))
            .stderr(predicate::str::contains("--help"));
    }
}

#[test]
fn s16_wav_matches_raw_and_library() {
    let dir = scratch();
    let dir = dir.path();
    let (rate, ch, frames) = (44_100, 2, 70_001);
    let (data, vals) = quantize(Fmt::S16, &signal(frames, 2));
    let pcm = vals.unwrap();
    let want = lib_i16(&pcm, rate, ch, &opts(3.0)); // default quality is 3

    let plain = encode_file(
        dir,
        "plain",
        &wav(Fmt::S16, ch, rate, &data, WavOpts::default()),
        &[],
    );
    assert_eq!(plain, want, "16-bit WAV == library encode");
    let extra = WavOpts {
        extra_chunks: true,
        ..Default::default()
    };
    let chunks = encode_file(dir, "chunks", &wav(Fmt::S16, ch, rate, &data, extra), &[]);
    assert_eq!(chunks, want, "unknown/odd chunks are skipped");
    let ext = WavOpts {
        extensible: true,
        ..Default::default()
    };
    let ext = encode_file(dir, "ext", &wav(Fmt::S16, ch, rate, &data, ext), &[]);
    assert_eq!(ext, want, "WAVE_FORMAT_EXTENSIBLE 16-bit");

    cmd()
        .args(["-Q", "--raw", "--rate", "44100", "--channels", "2", "-"])
        .write_stdin(data.clone())
        .assert()
        .success()
        .stdout(want.clone());
    cmd()
        .args(["--stream", "44100", "stereo", "3"])
        .write_stdin(data.clone())
        .assert()
        .success()
        .stdout(want.clone());
    check_decodes(&want, ch, rate, frames);
}

/// More than two channels: WAV input in WAV/SMPTE order is reordered to
/// Vorbis order (as oggenc does), raw input is encoded as given, and both
/// match the library.
#[test]
fn multichannel_wav_is_reordered_raw_is_not() {
    let dir = scratch();
    let dir = dir.path();
    for (ch, rate, map) in [
        (6u16, 48_000u32, &[0usize, 2, 1, 4, 5, 3][..]),
        (3, 44_100, &[0, 2, 1]),
        (4, 32_000, &[0, 1, 2, 3]),
        (8, 44_100, &[0, 2, 1, 6, 7, 4, 5, 3]),
        (10, 22_050, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
    ] {
        let frames = 9_001;
        let (data, vals) = quantize(Fmt::S16, &signal(frames, ch as usize));
        let pcm = vals.unwrap();
        let reordered: Vec<i16> = pcm
            .chunks_exact(ch as usize)
            .flat_map(|f| map.iter().map(|&m| f[m]))
            .collect();
        let want = lib_i16(&reordered, rate, ch, &opts(3.0));
        let got = encode_file(
            dir,
            &format!("mc{ch}"),
            &wav(Fmt::S16, ch, rate, &data, WavOpts::default()),
            &[],
        );
        assert_eq!(
            got, want,
            "{ch}-channel WAV == library encode of Vorbis order"
        );
        check_decodes(&want, ch, rate, frames);
        let ch_s = ch.to_string();
        let rate_s = rate.to_string();
        cmd()
            .args(["-Q", "--raw", "--rate", &rate_s, "--channels", &ch_s, "-"])
            .write_stdin(data.clone())
            .assert()
            .success()
            .stdout(lib_i16(&pcm, rate, ch, &opts(3.0)));
    }
    // 32-bit float input goes through the same reordering.
    let (data, vals) = quantize(Fmt::F32, &signal(5_000, 6));
    let pcm = vals.unwrap_err();
    let reordered: Vec<f32> = pcm
        .chunks_exact(6)
        .flat_map(|f| [0, 2, 1, 4, 5, 3].map(|m| f[m]))
        .collect();
    let got = encode_file(
        dir,
        "mcf",
        &wav(Fmt::F32, 6, 44_100, &data, WavOpts::default()),
        &[],
    );
    assert_eq!(got, lib_f32(&reordered, 44_100, 6, &opts(3.0)));
    cmd()
        .args(["-Q", "--raw", "-R", "44100", "-C", "5.1", "-"])
        .write_stdin(quantize(Fmt::S16, &signal(100, 6)).0)
        .assert()
        .success();
    cmd()
        .args(["-Q", "--raw", "-R", "44100", "-C", "256", "-"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("want 1 to 255"));
}

#[test]
fn stdin_to_stdout_and_derived_output_path() {
    let dir = scratch();
    let (data, vals) = quantize(Fmt::S16, &signal(12_345, 1));
    let w = wav(Fmt::S16, 1, 48_000, &data, WavOpts::default());
    let want = lib_i16(&vals.unwrap(), 48_000, 1, &opts(3.0));
    // stdin -> stdout, summary on stderr.
    cmd()
        .arg("-")
        .write_stdin(w.clone())
        .assert()
        .success()
        .stdout(want.clone())
        .stderr(predicate::str::starts_with(
            "stdin -> stdout: 0:00.26, 48000 Hz mono 16-bit, q3,",
        ));
    // File input: the .wav extension is replaced; nothing on stdout.
    let input = dir.path().join("my.song.wav");
    std::fs::write(&input, &w).unwrap();
    cmd()
        .arg(&input)
        .assert()
        .success()
        .stdout("")
        .stderr(predicate::str::contains("my.song.ogg"));
    assert_eq!(std::fs::read(dir.path().join("my.song.ogg")).unwrap(), want);
    // -o picks the name; -o - is stdout.
    let out = dir.path().join("elsewhere.ogg");
    cmd()
        .arg(&input)
        .arg("-o")
        .arg(&out)
        .arg("-Q")
        .assert()
        .success()
        .stderr("");
    assert_eq!(std::fs::read(&out).unwrap(), want);
    cmd()
        .arg(&input)
        .args(["-o", "-"])
        .assert()
        .success()
        .stdout(want);
}

#[test]
fn every_format_matches_the_library() {
    let dir = scratch();
    let dir = dir.path();
    let cases: [(Fmt, WavOpts); 9] = [
        (Fmt::U8, WavOpts::default()),
        (Fmt::S24, WavOpts::default()),
        (Fmt::S32, WavOpts::default()),
        (Fmt::F32, WavOpts::default()),
        (Fmt::F64, WavOpts::default()),
        (
            Fmt::S24,
            WavOpts {
                extensible: true,
                valid_bits: Some(20),
                ..Default::default()
            },
        ),
        (
            Fmt::S32,
            WavOpts {
                extensible: true,
                valid_bits: Some(24),
                ..Default::default()
            },
        ),
        (
            Fmt::F32,
            WavOpts {
                extensible: true,
                extra_chunks: true,
                ..Default::default()
            },
        ),
        (
            Fmt::U8,
            WavOpts {
                extensible: true,
                ..Default::default()
            },
        ),
    ];
    for (n, (fmt, o)) in cases.into_iter().enumerate() {
        for (rate, ch) in [(48_000u32, 1u16), (44_100, 2)] {
            let frames = 30_011;
            let (data, vals) = quantize(fmt, &signal(frames, ch as usize));
            let want = lib_f32(&vals.unwrap_err(), rate, ch, &opts(6.0));
            let got = encode_file(
                dir,
                &format!("{n}_{ch}"),
                &wav(fmt, ch, rate, &data, o),
                &["-q", "6"],
            );
            assert_eq!(got, want, "{fmt:?} ext={} {rate}/{ch}", o.extensible);
            check_decodes(&got, ch, rate, frames);
        }
    }
}

#[test]
fn raw_formats_match_the_library() {
    for (fmt, name) in [
        (Fmt::F32, "f32le"),
        (Fmt::S24, "s24le"),
        (Fmt::S32, "s32le"),
        (Fmt::U8, "u8"),
        (Fmt::F64, "f64le"),
    ] {
        let (data, vals) = quantize(fmt, &signal(20_000, 1));
        let want = lib_f32(&vals.unwrap_err(), 48_000, 1, &opts(3.0));
        cmd()
            .args(["-Q", "-r", "-R", "48000", "-C", "1", "-F", name, "-"])
            .write_stdin(data)
            .assert()
            .success()
            .stdout(want);
    }
}

#[test]
fn streamed_and_rf64_wavs() {
    let dir = scratch();
    let dir = dir.path();
    let frames = 25_000;
    let (data, vals) = quantize(Fmt::S24, &signal(frames, 2));
    let want = lib_f32(&vals.unwrap_err(), 44_100, 2, &opts(3.0));
    for size in [0, u32::MAX] {
        let o = WavOpts {
            data_size: Some(size),
            ..Default::default()
        };
        // Read to EOF, from stdin (unseekable) and from a file.
        cmd()
            .args(["-Q", "-"])
            .write_stdin(wav(Fmt::S24, 2, 44_100, &data, o))
            .assert()
            .success()
            .stdout(want.clone());
        let got = encode_file(
            dir,
            &format!("s{size}"),
            &wav(Fmt::S24, 2, 44_100, &data, o),
            &[],
        );
        assert_eq!(got, want, "data size {size:#x} via file");
    }
    let rf = WavOpts {
        rf64: true,
        extra_chunks: true,
        ..Default::default()
    };
    let got = encode_file(dir, "rf64", &wav(Fmt::S24, 2, 44_100, &data, rf), &[]);
    assert_eq!(got, want, "RF64");
}

#[test]
fn truncated_wav_is_a_clean_error() {
    let dir = scratch();
    let (data, _) = quantize(Fmt::S16, &signal(25_000, 2));
    let mut f = wav(Fmt::S16, 2, 44_100, &data, WavOpts::default());
    f.truncate(f.len() - data.len() + 4001); // header claims more data
    let input = dir.path().join("trunc.wav");
    std::fs::write(&input, &f).unwrap();
    cmd()
        .arg(&input)
        .assert()
        .code(1)
        .stderr(predicate::str::contains(format!(
            "error: failed to read '{}': file is truncated: the data chunk declares 100000 bytes but only 4001 are present",
            input.display()
        )))
        .stderr(predicate::str::contains("panicked").not());
    assert!(!dir.path().join("trunc.ogg").exists(), "no partial output");
    // A header cut short is an error too.
    let input = dir.path().join("head.wav");
    std::fs::write(&input, &f[..30]).unwrap();
    cmd()
        .arg(&input)
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "file ends inside the 'fmt ' chunk",
        ));
}

#[test]
fn rejects_unsupported_inputs() {
    let dir = scratch();
    let dir = dir.path();
    let (data, _) = quantize(Fmt::S16, &signal(1000, 2));
    let tagged = |tag| {
        wav(
            Fmt::S16,
            2,
            44_100,
            &data,
            WavOpts {
                tag: Some(tag),
                ..Default::default()
            },
        )
    };
    let too_many = {
        let mut w = wav(Fmt::S16, 256, 48_000, &data, WavOpts::default());
        w.truncate(w.len() - data.len());
        w
    };
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("adpcm", tagged(2), "0x0002 (Microsoft ADPCM)"),
        ("mp3", tagged(0x55), "MP3"),
        ("too_many", too_many, "256 channels"),
        (
            "rate",
            wav(Fmt::S16, 2, 384_000, &data, WavOpts::default()),
            "sample rate 384000",
        ),
        ("text", b"hello, this is not audio".to_vec(), "not a WAV"),
        ("empty", Vec::new(), "empty"),
    ];
    for (name, bytes, msg) in cases {
        let input = dir.join(format!("{name}.wav"));
        std::fs::write(&input, &bytes).unwrap();
        fails(&run(&[p(&input)], None), 1, msg);
        assert!(
            !dir.join(format!("{name}.ogg")).exists(),
            "{name}: no output"
        );
        // Same via stdin.
        fails(&run(&["-"], Some(&bytes)), 1, msg);
    }
    cmd()
        .arg("missing.wav")
        .assert()
        .code(1)
        .stderr(predicate::str::starts_with(
            "error: failed to open 'missing.wav': ",
        ));
}

#[test]
fn overwrite_policy() {
    let dir = scratch();
    let dir = dir.path();
    let (data, _) = quantize(Fmt::S16, &signal(5000, 1));
    let input = dir.join("in.wav");
    let w = wav(Fmt::S16, 1, 48_000, &data, WavOpts::default());
    std::fs::write(&input, &w).unwrap();
    let out = dir.join("in.ogg");
    std::fs::write(&out, b"precious").unwrap();

    fails(&run(&[p(&input)], None), 1, "already exists (use -f");
    assert_eq!(std::fs::read(&out).unwrap(), b"precious");
    ok(&run(&["-f", "-Q", p(&input)], None));
    assert_eq!(&std::fs::read(&out).unwrap()[..4], b"OggS");

    // The input is never overwritten, even with -f, also via a hard link.
    fails(
        &run(&["-f", p(&input), "-o", p(&input)], None),
        1,
        "refusing to overwrite the input file",
    );
    let link = dir.join("link.ogg");
    std::fs::hard_link(&input, &link).unwrap();
    fails(
        &run(&["-f", p(&input), "-o", p(&link)], None),
        1,
        "input file",
    );
    assert_eq!(std::fs::read(&input).unwrap(), w);

    // A failed encode leaves no partial file and keeps an existing one.
    let bad = dir.join("bad.wav");
    std::fs::write(&bad, b"RIFF\0\0\0\0WAVEjunk").unwrap();
    std::fs::write(dir.join("bad.ogg"), b"old").unwrap();
    fails(&run(&["-f", p(&bad)], None), 1, "bad.wav");
    assert_eq!(std::fs::read(dir.join("bad.ogg")).unwrap(), b"old");
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Non-regular outputs (/dev/null) are written without -f.
    #[cfg(unix)]
    ok(&run(&["-Q", p(&input), "-o", "/dev/null"], None));
}

#[test]
fn multiple_inputs_comments_and_serial() {
    let dir = scratch();
    let dir = dir.path();
    let (data, vals) = quantize(Fmt::S16, &signal(9000, 2));
    let w = wav(Fmt::S16, 2, 44_100, &data, WavOpts::default());
    for n in ["a", "c"] {
        std::fs::write(dir.join(format!("{n}.wav")), &w).unwrap();
    }
    std::fs::write(dir.join("b.wav"), b"garbage").unwrap();
    let o = run(
        &[
            "-t",
            "First",
            "-t",
            "Second",
            "-a",
            "Artist",
            "-l",
            "Album",
            "-N",
            "7",
            "-G",
            "Test",
            "-d",
            "2026",
            "-c",
            "COMMENT=x=y",
            "--serial",
            "100",
            p(&dir.join("a.wav")),
            p(&dir.join("b.wav")),
            p(&dir.join("c.wav")),
        ],
        None,
    );
    fails(&o, 1, "b.wav': not a WAV file");
    let err = stderr(&o);
    assert!(
        err.contains("a.ogg: 0:00.20, 44100 Hz stereo 16-bit, q3"),
        "{err}"
    );
    assert!(err.contains("kbps") && err.contains("realtime"), "{err}");
    assert!(!dir.join("b.ogg").exists());
    let a = std::fs::read(dir.join("a.ogg")).unwrap();
    let c = std::fs::read(dir.join("c.ogg")).unwrap();

    let s = |v: &[(&str, &str)]| -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let common = [
        ("ARTIST", "Artist"),
        ("ALBUM", "Album"),
        ("TRACKNUMBER", "7"),
        ("GENRE", "Test"),
        ("DATE", "2026"),
        ("COMMENT", "x=y"),
    ];
    let da = decode(&a);
    let dc = decode(&c);
    let mut want_a = s(&[("TITLE", "First")]);
    want_a.extend(s(&common));
    let mut want_c = s(&common);
    assert_eq!(da.comments, want_a);
    assert_eq!(dc.comments, want_c, "third file: no third -t");
    assert_eq!((da.serial, dc.serial), (100, 102));

    // Byte-identical to the library with the same options.
    let mut o = opts(3.0).with_serial(102);
    for (k, v) in want_c.drain(..) {
        o = o.with_comment(k, v);
    }
    assert_eq!(c, lib_i16(&vals.unwrap(), 44_100, 2, &o));
}

/// `oxivorbis long.wav -o - | head -c 10`: the reader goes away early.
#[test]
fn broken_pipe_is_quiet() {
    let dir = scratch();
    // ~20 s of stereo noise, enough to still be encoding when the reader
    // goes away.
    let (data, _) = quantize(Fmt::S16, &signal(44_100 * 20, 2));
    let input = dir.path().join("long.wav");
    std::fs::write(&input, wav(Fmt::S16, 2, 44_100, &data, WavOpts::default())).unwrap();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin!("oxivorbis"))
        .arg(&input)
        .args(["-o", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut first = [0u8; 10];
    out.read_exact(&mut first).unwrap();
    assert_eq!(&first[..4], b"OggS");
    drop(out);
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stderr(&o), "", "no message on a closed pipe");
}

#[test]
fn bitrate_flags_match_the_library() {
    use oxivorbis::BitrateMode as B;
    let (data, vals) = quantize(Fmt::S16, &signal(60_000, 2));
    let pcm = vals.unwrap();
    let r = oxivorbis::SampleRate::HZ_44100;
    let c = oxivorbis::Channels::STEREO;
    for (args, mode, q) in [
        (&["-b", "128"][..], B::Nominal { kbps: 128 }, 3.0),
        (
            &["--managed", "-b", "112", "-M", "128"],
            B::Average {
                kbps: 112,
                min_kbps: None,
                max_kbps: Some(128),
            },
            3.0,
        ),
        (
            &["-m", "96"],
            B::Limits {
                min_kbps: Some(96),
                max_kbps: None,
            },
            3.0,
        ),
        (
            &["-q", "7", "-M", "160"],
            B::QualityLimits {
                min_kbps: None,
                max_kbps: Some(160),
            },
            7.0,
        ),
    ] {
        let o = opts(q).with_bitrate(r, c, mode).unwrap();
        let want = lib_i16(&pcm, 44_100, 2, &o);
        cmd()
            .args(["-Q", "-r", "-R", "44100", "-C", "2"])
            .args(args)
            .arg("-")
            .write_stdin(data.clone())
            .assert()
            .success()
            .stdout(want);
    }
    // Out of libvorbis's range: a clear error, exit 1.
    let o = run(
        &["-r", "-R", "44100", "-C", "2", "-b", "900", "-"],
        Some(&data),
    );
    fails(&o, 1, "no libvorbis setup for 900000 b/s");
}
