//! Vorbis header packet construction (id, comment, setup) per Vorbis I §4.2.
//!
//! The three packets are byte-identical to what libvorbis 1.3.7 produces in
//! VBR mode. The `write_id_header` / `write_setup_header` conveniences are the
//! Q5 packets (for tests); the encoder uses the quality-generic
//! [`write_id_header_with`] and [`write_setup_packet`].

use crate::bitpack::BitWriter;
use crate::{Channels, SampleRate};

/// Nominal bitrate for mono streams at Q5 (bits/sec).
const BITRATE_NOMINAL_MONO: i32 = 96_000;

/// Nominal bitrate for stereo streams at Q5 (bits/sec).
const BITRATE_NOMINAL_STEREO: i32 = 160_000;

/// Q5 block sizes (short, long).
const Q5_BLOCKSIZES: [usize; 2] = [256, 2048];

/// Write the Q5 Vorbis identification header packet (packet type 0x01).
pub fn write_id_header(rate: SampleRate, channels: Channels, w: &mut BitWriter) {
    let bitrate_nominal = match channels {
        Channels::Mono => BITRATE_NOMINAL_MONO,
        Channels::Stereo => BITRATE_NOMINAL_STEREO,
    };
    write_id_header_with(rate, channels, bitrate_nominal, Q5_BLOCKSIZES, w);
}

/// Write the Vorbis identification header packet (packet type 0x01) with
/// the given nominal bitrate and `[short, long]` block sizes (powers of
/// two). libvorbis VBR mode leaves bitrate_upper/lower at 0 (unset).
///
/// Per Vorbis I §4.2.2.
pub fn write_id_header_with(
    rate: SampleRate,
    channels: Channels,
    bitrate_nominal: i32,
    blocksizes: [usize; 2],
    w: &mut BitWriter,
) {
    debug_assert!(blocksizes.iter().all(|b| b.is_power_of_two()));
    let audio_sample_rate: u32 = rate.hz();
    let audio_channels: u32 = match channels {
        Channels::Mono => 1,
        Channels::Stereo => 2,
    };
    w.write(0x01, 8);
    for &b in b"vorbis" {
        w.write(b as u32, 8);
    }

    w.write(0, 32);
    w.write(audio_channels, 8);
    w.write(audio_sample_rate, 32);
    w.write(0, 32);
    w.write(bitrate_nominal as u32, 32);
    w.write(0, 32);

    w.write(blocksizes[0].trailing_zeros(), 4);
    w.write(blocksizes[1].trailing_zeros(), 4);

    w.write(1, 1);
}

/// Write the Vorbis comment header packet (packet type 0x03): the vendor
/// string, then each user comment (`KEY=value` bytes) in order.
///
/// Per Vorbis I §4.2.3. Lengths and the comment count are 32-bit fields:
/// a string longer than `u32::MAX` bytes is truncated to `u32::MAX` bytes
/// and comments past the `u32::MAX`th are dropped, so the header always
/// stays self-consistent (rather than the length wrapping).
pub fn write_comment_header<C: AsRef<[u8]>>(w: &mut BitWriter, vendor: &[u8], comments: &[C]) {
    let write_str = |w: &mut BitWriter, s: &[u8]| {
        let s = &s[..s.len().min(U32_MAX)];
        w.write(s.len() as u32, 32);
        for &b in s {
            w.write(b as u32, 8);
        }
    };
    w.write(0x03, 8);
    for &b in b"vorbis" {
        w.write(b as u32, 8);
    }
    write_str(w, vendor);
    let comments = &comments[..comments.len().min(U32_MAX)];
    w.write(comments.len() as u32, 32);
    for c in comments {
        write_str(w, c.as_ref());
    }
    w.write(1, 1);
}

/// `u32::MAX` as a length (`usize::MAX` where usize is narrower).
const U32_MAX: usize = u32::MAX as usize;

/// Write a complete, pre-built setup header packet (as produced by the
/// setup generator; it already starts with the 0x05 + "vorbis" sync).
pub fn write_setup_packet(packet: &[u8], w: &mut BitWriter) {
    for &b in packet {
        w.write(b as u32, 8);
    }
}

/// Write the Q5 Vorbis setup header packet (packet type 0x05), as the
/// encoder generates it for `rate` / `channels`.
pub fn write_setup_header(rate: SampleRate, channels: Channels, w: &mut BitWriter) {
    let setup = crate::setup::encoder_setup_for(rate.hz(), channels.count() as usize, 0.5);
    write_setup_packet(&setup.packet, w);
}
