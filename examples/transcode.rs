//! Transcode any file Symphonia can decode (MP3, FLAC, WAV, Ogg Vorbis)
//! to Ogg Vorbis, streaming the decoder's planar `f32` output straight
//! into an [`OggWriter`].
//!
//! ```sh
//! cargo run --release --example transcode -- input.mp3 output.ogg [quality]
//! ```
//!
//! `symphonia` is a dev-dependency of oxivorbis (used only by this
//! example), not a dependency of the library.

use oxivorbis::{Channels, OggWriter, Options, Quality, SampleRate};
use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, output) = match args.as_slice() {
        [i, o] | [i, o, _] => (i, o),
        _ => return Err("usage: transcode <input> <output.ogg> [quality -1..10]".into()),
    };
    let quality = match args.get(2) {
        Some(q) => q
            .parse()
            .ok()
            .and_then(Quality::new)
            .ok_or_else(|| format!("quality must be a number in -1..=10, got {q:?}"))?,
        None => Quality::Q5,
    };

    let src = File::open(input).map_err(|e| format!("{input}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("{input}: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| format!("{input}: no audio track"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| format!("{input}: not an audio track"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("{input}: {e}"))?;

    let opts = Options::default().with_quality(quality);
    // Created on the first decoded buffer, once its rate and layout are known.
    let mut writer: Option<OggWriter<BufWriter<File>>> = None;
    let mut planes: Vec<Vec<f32>> = Vec::new(); // reused for every packet
    let mut frames = 0u64;
    let mut n_channels = 0u16;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => return Err(format!("{input}: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // Skip a corrupt packet, as players do.
            Err(Error::DecodeError(e)) => {
                eprintln!("transcode: skipping packet: {e}");
                continue;
            }
            Err(e) => return Err(format!("{input}: {e}")),
        };
        let w = match &mut writer {
            Some(w) => w,
            None => {
                let spec = buf.spec();
                let rate = SampleRate::try_from(spec.rate()).map_err(|e| e.to_string())?;
                let n = u16::try_from(spec.channels().count()).unwrap_or(u16::MAX);
                let channels = Channels::try_from(n).map_err(|e| e.to_string())?;
                n_channels = n;
                let file = File::create(output).map_err(|e| format!("{output}: {e}"))?;
                let w = OggWriter::new(BufWriter::new(file), rate, channels, &opts)
                    .map_err(|e| format!("{output}: {e}"))?;
                writer.insert(w)
            }
        };
        if buf.spec().channels().count() != usize::from(n_channels) {
            return Err(format!("{input}: channel layout changes mid-stream"));
        }
        buf.copy_to_vecs_planar::<f32>(&mut planes);
        frames += buf.frames() as u64;
        w.write_planar_f32(&planes)
            .map_err(|e| format!("{output}: {e}"))?;
    }

    let w = writer.ok_or_else(|| format!("{input}: no audio decoded"))?;
    w.finish().map_err(|e| format!("{output}: {e}"))?;
    eprintln!("{input} -> {output}: {frames} frames at q{}", quality.get());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("transcode: {e}");
            ExitCode::FAILURE
        }
    }
}
