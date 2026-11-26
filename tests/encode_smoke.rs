use lewtoff::{Channels, SampleRate};

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
    let bytes = lewtoff::encode(&silence(44100), SampleRate::Hz44100, Channels::Mono);
    check_ogg(&bytes);
}

#[test]
fn encode_silence_mono48_does_not_panic() {
    let bytes = lewtoff::encode(&silence(48000), SampleRate::Hz48000, Channels::Mono);
    check_ogg(&bytes);
}

#[test]
fn encode_silence_stereo44_does_not_panic() {
    let bytes = lewtoff::encode(&silence(44100 * 2), SampleRate::Hz44100, Channels::Stereo);
    check_ogg(&bytes);
}

#[test]
fn encode_silence_stereo48_does_not_panic() {
    let bytes = lewtoff::encode(&silence(48000 * 2), SampleRate::Hz48000, Channels::Stereo);
    check_ogg(&bytes);
}

#[test]
fn encode_sine_stereo44_does_not_panic() {
    let samples = sine_stereo(44100, 44100);
    let bytes = lewtoff::encode(&samples, SampleRate::Hz44100, Channels::Stereo);
    check_ogg(&bytes);
}

#[test]
fn lewton_can_decode_our_silence_mono44() {
    let bytes = lewtoff::encode(&silence(44100), SampleRate::Hz44100, Channels::Mono);
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
    let bytes = lewtoff::encode(&silence(44100 * 2), SampleRate::Hz44100, Channels::Stereo);
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
    // float (see lewtoff::encode::oggenc_base_quality and the oracle-gated
    // tests/parity.rs::parity_oggenc_quality_conversion).
    let bits = |q: f32| lewtoff::oggenc_base_quality(q).to_bits();
    assert_eq!(lewtoff::oggenc_base_quality(5.0), 0.5);
    assert_eq!(lewtoff::oggenc_base_quality(10.0), 1.0);
    assert_eq!(lewtoff::oggenc_base_quality(11.0), 1.0);
    assert_eq!(bits(9.344), 0x3f6f34d6);
    assert_eq!(bits(9.303), 0x3f6e2825);
}

#[test]
fn encode_with_quality_q5_matches_encode_and_others_decode() {
    let samples = sine_stereo(44100, 44100);
    // Same settings except the random serial (and therefore page CRCs).
    let a = lewtoff::encode(&samples, SampleRate::Hz44100, Channels::Stereo);
    let b = lewtoff::encode_with_quality(&samples, SampleRate::Hz44100, Channels::Stereo, 5.0);
    assert_eq!(a.len(), b.len());
    let mut prev_len = 0;
    for q in [0.0f32, 2.5, 6.0, 10.0] {
        let bytes =
            lewtoff::encode_with_quality(&samples, SampleRate::Hz44100, Channels::Stereo, q);
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
#[should_panic(expected = "q-1 not yet supported")]
fn encode_with_quality_rejects_q_minus_1() {
    lewtoff::encode_with_quality(&silence(4410), SampleRate::Hz44100, Channels::Mono, -0.5);
}
