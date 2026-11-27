//! Codebook tests: Layer A (synthetic unit tests) + Layer B (lewton cross-check).
//!
//! Layer B (lewton cross-check) is partially limited because lewton's SetupHeader
//! fields are pub(crate) — we can verify that lewton parses successfully but
//! cannot compare internal codebook field values directly. Phase 9 parity is the
//! ground truth for encode correctness.

// ---------------------------------------------------------------------------
// Layer A: synthetic unit tests
// ---------------------------------------------------------------------------

/// Layer A unit tests live in src/codebook.rs (pub(crate) items can't be
/// tested from integration tests without extra test-export machinery).
/// This test verifies the encoder's Q5 setup packet has the expected sync
/// bytes and size.
#[test]
fn q5_setup_sync_bytes_and_size() {
    let blob = &q5_header_packets()[2];
    assert_eq!(
        &blob[0..7],
        b"\x05vorbis",
        "setup header must start with 0x05 vorbis"
    );
    assert!(
        blob.len() > 1000,
        "setup header suspiciously small: {} bytes",
        blob.len()
    );
    assert!(
        blob.len() < 20_000,
        "setup header suspiciously large: {} bytes",
        blob.len()
    );
}

/// The three header packets of a Q5 44.1 kHz mono oxivorbis encode.
fn q5_header_packets() -> Vec<Vec<u8>> {
    let ogg = oxivorbis::encode(
        &[0i16; 1024],
        oxivorbis::SampleRate::HZ_44100,
        oxivorbis::Channels::Mono,
        &oxivorbis::Options::default(),
    );
    let mut reader = ogg::PacketReader::new(std::io::Cursor::new(ogg));
    (0..3)
        .map(|_| reader.read_packet().unwrap().unwrap().data)
        .collect()
}

// ---------------------------------------------------------------------------
// Layer B: lewton cross-check
//
// lewton's SetupHeader.codebooks field is pub(crate), so we cannot compare
// individual codebook field values. We CAN verify that lewton successfully
// parses the same blob (proving the blob is valid Vorbis). The encode-side
// correctness gate is Phase 9 parity.
// ---------------------------------------------------------------------------

/// Verify lewton can parse the encoder's Q5 headers without error.
/// This proves the generated setup is a valid Vorbis setup header.
#[test]
fn lewton_parses_q5_setup_header() {
    use lewton::header::{read_header_comment, read_header_ident, read_header_setup};

    let pkts = q5_header_packets();
    let ident = read_header_ident(&pkts[0]).expect("lewton failed to parse ident header");
    let _comment = read_header_comment(&pkts[1]).expect("lewton failed to parse comment header");
    let _setup = read_header_setup(
        &pkts[2],
        ident.audio_channels,
        (ident.blocksize_0, ident.blocksize_1),
    )
    .expect("lewton failed to parse setup header");
}
