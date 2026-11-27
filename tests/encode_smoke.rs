use oxivorbis::{Channels, Options, Quality, SampleRate};

fn silence(n: usize) -> Vec<i16> {
    vec![0i16; n]
}

fn sine_stereo(n_samples: usize, rate: u32) -> Vec<i16> {
    let mut out = Vec::with_capacity(n_samples * 2);
    for i in 0..n_samples {
        let t = i as f32 / rate as f32;
        let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        let s = (v * 16000.0) as i16;
        out.push(s);
        out.push(s);
    }
    out
}

fn check_ogg(bytes: &[u8]) {
    assert!(
        bytes.len() > 1000,
        "output too short: {} bytes",
        bytes.len()
    );
    assert_eq!(&bytes[0..4], b"OggS", "missing OggS magic");
}

#[test]
fn encode_silence_mono44_does_not_panic() {
    let bytes = oxivorbis::encode(
        &silence(44100),
        SampleRate::HZ_44100,
        Channels::Mono,
        &Options::default(),
    );
    check_ogg(&bytes);
}

#[test]
fn encode_silence_mono48_does_not_panic() {
    let bytes = oxivorbis::encode(
        &silence(48000),
        SampleRate::HZ_48000,
        Channels::Mono,
        &Options::default(),
    );
    check_ogg(&bytes);
}

#[test]
fn encode_silence_stereo44_does_not_panic() {
    let bytes = oxivorbis::encode(
        &silence(44100 * 2),
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    check_ogg(&bytes);
}

#[test]
fn encode_silence_stereo48_does_not_panic() {
    let bytes = oxivorbis::encode(
        &silence(48000 * 2),
        SampleRate::HZ_48000,
        Channels::Stereo,
        &Options::default(),
    );
    check_ogg(&bytes);
}

#[test]
fn encode_sine_stereo44_does_not_panic() {
    let samples = sine_stereo(44100, 44100);
    let bytes = oxivorbis::encode(
        &samples,
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    check_ogg(&bytes);
}

#[test]
fn lewton_can_decode_our_silence_mono44() {
    let bytes = oxivorbis::encode(
        &silence(44100),
        SampleRate::HZ_44100,
        Channels::Mono,
        &Options::default(),
    );
    let cursor = std::io::Cursor::new(bytes);
    let mut reader =
        lewton::inside_ogg::OggStreamReader::new(cursor).expect("lewton failed to open stream");
    let mut max_abs = 0i16;
    loop {
        match reader.read_dec_packet_itl() {
            Ok(Some(pkt)) => {
                for s in pkt {
                    if s.abs() > max_abs {
                        max_abs = s.abs();
                    }
                }
            }
            Ok(None) => break,
            Err(e) => panic!("lewton decode error: {:?}", e),
        }
    }
    assert!(
        max_abs < 1000,
        "silence decoded with unexpected amplitude: {}",
        max_abs
    );
}

#[test]
fn lewton_can_decode_our_silence_stereo44() {
    let bytes = oxivorbis::encode(
        &silence(44100 * 2),
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    let cursor = std::io::Cursor::new(bytes);
    let mut reader =
        lewton::inside_ogg::OggStreamReader::new(cursor).expect("lewton failed to open stream");
    loop {
        match reader.read_dec_packet_itl() {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => panic!("lewton decode error: {:?}", e),
        }
    }
}

#[test]
fn oggenc_base_quality_bits() {
    // Bits produced by vorbis-tools 1.4.3 oggenc's `quality *= 0.1` on a
    // float (see oxivorbis::encode::oggenc_base_quality and the oracle-gated
    // tests/parity.rs::parity_oggenc_quality_conversion).
    let bits = |q: f32| oxivorbis::oggenc_base_quality(q).to_bits();
    assert_eq!(oxivorbis::oggenc_base_quality(5.0), 0.5);
    assert_eq!(oxivorbis::oggenc_base_quality(10.0), 1.0);
    assert_eq!(oxivorbis::oggenc_base_quality(11.0), 1.0);
    assert_eq!(bits(9.344), 0x3f6f34d6);
    assert_eq!(bits(9.303), 0x3f6e2825);
}

fn at(q: f32) -> Options {
    Options::default().with_quality(Quality::new(q).unwrap())
}

#[test]
fn explicit_q5_matches_default_and_others_decode() {
    let samples = sine_stereo(44100, 44100);
    let a = oxivorbis::encode(
        &samples,
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    let b = oxivorbis::encode(&samples, SampleRate::HZ_44100, Channels::Stereo, &at(5.0));
    assert_eq!(a, b);
    let mut prev_len = 0;
    for q in [-1.0f32, -0.5, 0.0, 2.5, 6.0, 10.0] {
        let bytes = oxivorbis::encode(&samples, SampleRate::HZ_44100, Channels::Stereo, &at(q));
        check_ogg(&bytes);
        assert!(bytes.len() > prev_len, "q{q} not larger than lower quality");
        prev_len = bytes.len();
        let mut reader = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(bytes))
            .expect("lewton failed to open stream");
        while reader
            .read_dec_packet_itl()
            .expect("lewton decode error")
            .is_some()
        {}
    }
}

#[test]
fn quality_range() {
    assert_eq!(Quality::default(), Quality::Q5);
    assert_eq!(Quality::Q5.get(), 5.0);
    assert_eq!(Quality::new(7.25).unwrap().get(), 7.25);
    for ok in [-1.0f32, -0.0, 0.0, 10.0] {
        assert!(Quality::new(ok).is_some(), "{ok}");
    }
    for bad in [f32::NAN, -1.0001, 10.0001, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(Quality::new(bad).is_none(), "{bad}");
    }
}

#[test]
fn rate_and_channel_conversions() {
    assert_eq!(SampleRate::try_from(44100), Ok(SampleRate::HZ_44100));
    assert_eq!(SampleRate::try_from(22050), Ok(SampleRate::HZ_22050));
    assert_eq!(SampleRate::try_from(1).unwrap().hz(), 1);
    assert_eq!(SampleRate::try_from(200_000), Ok(SampleRate::MAX));
    assert!(SampleRate::try_from(0).is_err());
    assert_eq!(
        SampleRate::try_from(200_001).unwrap_err().to_string(),
        "unsupported sample rate 200001"
    );
    assert_eq!(SampleRate::try_from(48000).unwrap().hz(), 48000);
    assert_eq!(Channels::try_from(2u16), Ok(Channels::Stereo));
    assert_eq!(Channels::Mono.count(), 1);
    assert!(Channels::try_from(3u16).is_err());
    assert_eq!(
        Channels::try_from(0u16).unwrap_err().to_string(),
        "unsupported channel count 0"
    );
}

/// Default output is deterministic: fixed serial, oxivorbis vendor, no
/// comments.
#[test]
fn default_headers_are_deterministic() {
    let pcm = sine_stereo(5000, 44100);
    let a = oxivorbis::encode(
        &pcm,
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    let b = oxivorbis::encode(
        &pcm,
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    assert_eq!(a, b);
    assert_eq!(
        u32::from_le_bytes(a[14..18].try_into().unwrap()),
        oxivorbis::DEFAULT_SERIAL
    );
    let comment = comment_packet(&a);
    let vendor = oxivorbis::DEFAULT_VENDOR.as_bytes();
    assert!(oxivorbis::DEFAULT_VENDOR.starts_with("oxivorbis "));
    assert_eq!(&comment[7..11], (vendor.len() as u32).to_le_bytes());
    assert_eq!(&comment[11..11 + vendor.len()], vendor);
    assert_eq!(&comment[11 + vendor.len()..15 + vendor.len()], [0, 0, 0, 0]);
}

/// Serial, vendor and comments from `Options` land in the headers, in order.
#[test]
fn options_set_serial_vendor_and_comments() {
    let pcm = sine_stereo(5000, 44100);
    let opts = Options::default()
        .with_serial(0xDEAD_BEEF)
        .with_vendor("my vendor")
        .with_comment("TITLE", "Tone")
        .with_comment("ARTIST", "A=B");
    let ogg = oxivorbis::encode(&pcm, SampleRate::HZ_44100, Channels::Stereo, &opts);
    assert_eq!(
        u32::from_le_bytes(ogg[14..18].try_into().unwrap()),
        0xDEAD_BEEF
    );
    let mut want = b"\x03vorbis".to_vec();
    want.extend(9u32.to_le_bytes());
    want.extend(b"my vendor");
    want.extend(2u32.to_le_bytes());
    for s in [&b"TITLE=Tone"[..], b"ARTIST=A=B"] {
        want.extend((s.len() as u32).to_le_bytes());
        want.extend(s);
    }
    want.push(1);
    assert_eq!(comment_packet(&ogg), want);
    // The audio itself is unaffected by header options.
    let plain = oxivorbis::encode(
        &pcm,
        SampleRate::HZ_44100,
        Channels::Stereo,
        &Options::default(),
    );
    assert_eq!(audio_packets(&ogg), audio_packets(&plain));
}

fn packets(ogg: &[u8]) -> Vec<Vec<u8>> {
    let mut r = ogg::PacketReader::new(std::io::Cursor::new(ogg.to_vec()));
    let mut out = Vec::new();
    while let Some(p) = r.read_packet().unwrap() {
        out.push(p.data);
    }
    out
}

fn comment_packet(ogg: &[u8]) -> Vec<u8> {
    packets(ogg).swap_remove(1)
}

fn audio_packets(ogg: &[u8]) -> Vec<Vec<u8>> {
    packets(ogg).split_off(3)
}
