//! Vorbis header packet construction (id, comment, setup) per Vorbis I §4.2.
//!
//! The three packets are byte-identical to what libvorbis 1.3.7 produces in
//! VBR mode. The `write_id_header` / `write_setup_header` conveniences are the
//! Q5 packets; the encoder uses the quality-generic [`write_id_header_with`] and
//! [`write_setup_packet`].

use crate::bitpack::BitWriter;
use crate::setup_blob::{Q5_SETUP_MONO44, Q5_SETUP_MONO48, Q5_SETUP_STEREO44, Q5_SETUP_STEREO48};
use crate::{Channels, SampleRate};

/// Vendor string written by ffmpeg-libvorbis into the comment header.
/// This is the libavformat identification string, not the libvorbis string.
const VENDOR: &[u8] = b"Lavf61.7.100";

/// User comment written by ffmpeg into the comment header.
const ENCODER_TAG: &[u8] = b"encoder=Lavc61.19.101 libvorbis";

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
    let audio_sample_rate: u32 = match rate {
        SampleRate::Hz44100 => 44_100,
        SampleRate::Hz48000 => 48_000,
    };
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

/// Write the Vorbis comment header packet (packet type 0x03).
///
/// Per Vorbis I §4.2.3. Uses the provided vendor and encoder_tag strings
/// so the comment header matches exactly the ffmpeg version on the current
/// machine.  Pass `None` to use the compile-time defaults.
pub fn write_comment_header_with_strings(
    w: &mut BitWriter,
    vendor: Option<&[u8]>,
    encoder_tag: Option<&[u8]>,
) {
    let vendor = vendor.unwrap_or(VENDOR);
    let encoder_tag = encoder_tag.unwrap_or(ENCODER_TAG);

    w.write(0x03, 8);
    for &b in b"vorbis" {
        w.write(b as u32, 8);
    }

    w.write(vendor.len() as u32, 32);
    for &b in vendor {
        w.write(b as u32, 8);
    }

    if encoder_tag.is_empty() {
        w.write(0, 32);
    } else {
        w.write(1, 32);
        w.write(encoder_tag.len() as u32, 32);
        for &b in encoder_tag {
            w.write(b as u32, 8);
        }
    }

    w.write(1, 1);
}

/// Write the Vorbis comment header using compile-time vendor/encoder strings.
pub fn write_comment_header(w: &mut BitWriter) {
    write_comment_header_with_strings(w, None, None);
}

/// Write a complete, pre-built setup header packet (as produced by the
/// setup generator; it already starts with the 0x05 + "vorbis" sync).
pub fn write_setup_packet(packet: &[u8], w: &mut BitWriter) {
    for &b in packet {
        w.write(b as u32, 8);
    }
}

/// Write the Q5 Vorbis setup header packet (packet type 0x05).
///
/// The packet is the embedded Q5 setup blob byte-for-byte; it already
/// starts with the 0x05 + "vorbis" sync sequence. The blob varies by
/// both sample rate and channel count.
pub fn write_setup_header(rate: SampleRate, channels: Channels, w: &mut BitWriter) {
    let blob = match (rate, channels) {
        (SampleRate::Hz44100, Channels::Mono) => Q5_SETUP_MONO44,
        (SampleRate::Hz48000, Channels::Mono) => Q5_SETUP_MONO48,
        (SampleRate::Hz44100, Channels::Stereo) => Q5_SETUP_STEREO44,
        (SampleRate::Hz48000, Channels::Stereo) => Q5_SETUP_STEREO48,
    };
    write_setup_packet(blob, w);
}
