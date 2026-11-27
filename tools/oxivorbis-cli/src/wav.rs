//! Streaming RIFF/WAVE (and RF64/BW64) header parser.
//!
//! [`read_header`] consumes the stream up to the first byte of sample
//! data and returns the format and the data length, so the samples can
//! then be read straight from the same reader without buffering the file.
//! Chunks before `data` that aren't `fmt ` or `ds64` (`LIST`, `fact`,
//! `bext`, `JUNK`, ...) are skipped, with RIFF's pad byte after odd-sized
//! chunks. Anything after the `data` chunk is ignored.

use crate::pcm::SampleFormat;
use std::io::{self, Read};

/// Format and extent of the sample data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavInfo {
    pub rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    /// Data chunk length in bytes; `None` means "read to end of input"
    /// (a streamed WAV whose writer left the size as 0 or 0xFFFFFFFF).
    pub data_len: Option<u64>,
}

/// Largest `fmt `/`ds64` chunk accepted (real ones are 16 to 40 bytes).
const MAX_HEADER_CHUNK: u32 = 1 << 16;

/// `KSDATAFORMAT_SUBTYPE_*` GUIDs are `xxxxxxxx-0000-0010-8000-00aa00389b71`
/// with the format tag in the first field; these are the bytes after it.
const GUID_TAIL: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

/// Read exactly `buf.len()` bytes; `Ok(false)` on a clean EOF before the
/// first byte, an error on EOF part-way.
fn read_full(r: &mut impl Read, buf: &mut [u8], what: &str) -> Result<bool, String> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(format!("file ends inside the {what}")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    Ok(true)
}

fn skip(r: &mut impl Read, n: u64, id: &str) -> Result<(), String> {
    let copied =
        io::copy(&mut r.take(n), &mut io::sink()).map_err(|e| format!("read failed: {e}"))?;
    if copied < n {
        return Err(format!(
            "file ends inside the '{id}' chunk, before any audio data"
        ));
    }
    Ok(())
}

fn format_tag_name(tag: u16) -> &'static str {
    match tag {
        0x0002 => "Microsoft ADPCM",
        0x0006 => "A-law",
        0x0007 => "mu-law",
        0x0011 => "IMA ADPCM",
        0x0031 => "GSM 6.10",
        0x0050 => "MPEG",
        0x0055 => "MP3",
        0x00FF | 0x1600 | 0x1610 => "AAC",
        0x2000 => "AC-3",
        0xF1AC => "FLAC",
        _ => "compressed or unknown",
    }
}

/// Printable chunk id (`fmt `, `LIST`, or hex for binary junk).
fn chunk_name(id: &[u8; 4]) -> String {
    if id.iter().all(|b| (0x20..0x7F).contains(b)) {
        String::from_utf8_lossy(id).into_owned()
    } else {
        format!("0x{:08X}", u32::from_be_bytes(*id))
    }
}

/// Decode a `fmt ` chunk body into (rate, channels, sample format).
fn parse_fmt(b: &[u8]) -> Result<(u32, u16, SampleFormat), String> {
    if b.len() < 16 {
        return Err(format!("'fmt ' chunk too short ({} bytes)", b.len()));
    }
    let mut tag = u16_at(b, 0);
    let channels = u16_at(b, 2);
    let rate = u32_at(b, 4);
    let block_align = u16_at(b, 12);
    let bits = u16_at(b, 14);
    let extensible = tag == WAVE_FORMAT_EXTENSIBLE;
    if extensible {
        if b.len() < 40 {
            return Err(format!(
                "WAVE_FORMAT_EXTENSIBLE 'fmt ' chunk too short ({} bytes, need 40)",
                b.len()
            ));
        }
        let guid = &b[24..40];
        if guid[4..] != GUID_TAIL || guid[2..4] != [0, 0] {
            let hex: String = guid.iter().map(|x| format!("{x:02x}")).collect();
            return Err(format!(
                "unsupported WAVE_FORMAT_EXTENSIBLE sub-format GUID {hex}"
            ));
        }
        tag = u16_at(guid, 0);
    }
    let ext = if extensible {
        "WAVE_FORMAT_EXTENSIBLE "
    } else {
        ""
    };
    if tag != WAVE_FORMAT_PCM && tag != WAVE_FORMAT_IEEE_FLOAT {
        return Err(format!(
            "unsupported {ext}format tag 0x{tag:04X} ({}); only integer PCM and IEEE float are supported",
            format_tag_name(tag)
        ));
    }
    if channels == 0 {
        return Err("WAV header says 0 channels".into());
    }
    if channels > 2 {
        return Err(format!(
            "input has {channels} channels; oxivorbis currently encodes only mono or stereo \
             (downmix first, e.g. `ffmpeg -i in.wav -ac 2 out.wav`)"
        ));
    }
    if rate == 0 {
        return Err("WAV header says 0 Hz".into());
    }
    // Container size per sample. Samples narrower than their container
    // (e.g. 20 valid bits in 24) are left-justified, so scaling by the
    // container's full range is correct.
    if block_align == 0 || block_align % channels != 0 {
        return Err(format!(
            "inconsistent WAV header: block align {block_align} for {channels} channel(s)"
        ));
    }
    let container = block_align / channels;
    let format = match (tag, container) {
        (WAVE_FORMAT_PCM, 1) => SampleFormat::U8,
        (WAVE_FORMAT_PCM, 2) => SampleFormat::S16,
        (WAVE_FORMAT_PCM, 3) => SampleFormat::S24,
        (WAVE_FORMAT_PCM, 4) => SampleFormat::S32,
        (WAVE_FORMAT_IEEE_FLOAT, 4) => SampleFormat::F32,
        (WAVE_FORMAT_IEEE_FLOAT, 8) => SampleFormat::F64,
        (WAVE_FORMAT_PCM, _) => {
            return Err(format!(
                "unsupported {ext}PCM sample size: {bits} bits in {container} bytes"
            ));
        }
        _ => {
            return Err(format!(
                "unsupported {ext}float sample size: {bits} bits in {container} bytes"
            ));
        }
    };
    let width = 8 * container;
    let valid = match tag {
        WAVE_FORMAT_PCM => bits > width - 8 && bits <= width,
        _ => bits == width,
    };
    if !valid {
        return Err(format!(
            "inconsistent WAV header: {bits} bits per sample in a {container}-byte container"
        ));
    }
    Ok((rate, channels, format))
}

/// Parse a WAV header from `r`, leaving it positioned at the first
/// sample byte.
pub fn read_header(r: &mut impl Read) -> Result<WavInfo, String> {
    // Identify the file from its first 4 bytes before demanding all 12,
    // so a short non-WAV input is reported as such.
    let mut head = [0u8; 12];
    let first = read_full(r, &mut head[..4], "RIFF header").map_err(|e| {
        if e.starts_with("file ends") {
            "not a WAV file (no RIFF header); use --raw for headerless PCM".to_string()
        } else {
            e
        }
    })?;
    if !first {
        return Err("input is empty".into());
    }
    let rf64 = match &head[0..4] {
        b"RIFF" => false,
        b"RF64" | b"BW64" => true,
        b"OggS" => return Err("input is already an Ogg file".into()),
        _ => {
            return Err("not a WAV file (no RIFF header); use --raw for headerless PCM".into());
        }
    };
    if !read_full(r, &mut head[4..], "RIFF header")? {
        return Err("file ends inside the RIFF header".into());
    }
    if &head[8..12] != b"WAVE" {
        return Err("RIFF file but not WAVE".into());
    }
    let mut fmt: Option<(u32, u16, SampleFormat)> = None;
    let mut ds64_data: Option<u64> = None;
    loop {
        let mut ch = [0u8; 8];
        if !read_full(r, &mut ch, "chunk header")? {
            return Err(if fmt.is_some() {
                "no 'data' chunk".into()
            } else {
                "no 'fmt ' chunk".into()
            });
        }
        let id: [u8; 4] = [ch[0], ch[1], ch[2], ch[3]];
        let len = u32_at(&ch, 4);
        match &id {
            b"data" => {
                let (rate, channels, format) = fmt.ok_or("'data' chunk before the 'fmt ' chunk")?;
                let data_len = match len {
                    0xFFFF_FFFF if rf64 => ds64_data,
                    0 | 0xFFFF_FFFF => None,
                    n => Some(n as u64),
                };
                return Ok(WavInfo {
                    rate,
                    channels,
                    format,
                    data_len,
                });
            }
            b"fmt " | b"ds64" => {
                let name = chunk_name(&id);
                if len > MAX_HEADER_CHUNK {
                    return Err(format!("'{name}' chunk is implausibly large ({len} bytes)"));
                }
                let mut body = vec![0u8; len as usize + (len & 1) as usize];
                if !read_full(r, &mut body, &format!("'{name}' chunk"))? && !body.is_empty() {
                    return Err(format!("file ends inside the '{name}' chunk"));
                }
                let body = &body[..len as usize];
                if &id == b"fmt " {
                    if fmt.is_some() {
                        return Err("more than one 'fmt ' chunk".into());
                    }
                    fmt = Some(parse_fmt(body)?);
                } else {
                    if body.len() < 16 {
                        return Err("'ds64' chunk too short".into());
                    }
                    // riffSize (u64), dataSize (u64), sampleCount, table...
                    ds64_data = Some(u64_at(body, 8));
                }
            }
            _ => skip(r, len as u64 + (len & 1) as u64, &chunk_name(&id))?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_body(tag: u16, ch: u16, rate: u32, bits: u16, container: u16) -> Vec<u8> {
        let ba = ch * container;
        let mut b = Vec::new();
        b.extend(tag.to_le_bytes());
        b.extend(ch.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * ba as u32).to_le_bytes());
        b.extend(ba.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b
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

    fn wav(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut v = b"RIFF".to_vec();
        v.extend((body.len() as u32 + 4).to_le_bytes());
        v.extend(b"WAVE");
        v.extend(body);
        v
    }

    fn parse(b: &[u8]) -> Result<WavInfo, String> {
        read_header(&mut &b[..])
    }

    #[test]
    fn basic_and_unknown_chunks() {
        let f = wav(&[
            chunk(b"LIST", b"INFOabc"), // odd: padded
            chunk(b"fmt ", &fmt_body(1, 2, 44100, 16, 2)),
            chunk(b"fact", &[0; 4]),
            chunk(b"data", &[1, 2, 3, 4]),
        ]);
        let info = parse(&f).unwrap();
        assert_eq!(
            info,
            WavInfo {
                rate: 44100,
                channels: 2,
                format: SampleFormat::S16,
                data_len: Some(4)
            }
        );
    }

    #[test]
    fn leaves_reader_at_data() {
        let f = wav(&[
            chunk(b"fmt ", &fmt_body(1, 1, 48000, 8, 1)),
            chunk(b"data", &[7, 8, 9]),
        ]);
        let mut r = &f[..];
        let info = read_header(&mut r).unwrap();
        assert_eq!(info.format, SampleFormat::U8);
        assert_eq!(&r[..3], &[7, 8, 9]);
    }

    #[test]
    fn streamed_sizes() {
        for size in [0u32, 0xFFFF_FFFF] {
            let mut f = wav(&[chunk(b"fmt ", &fmt_body(3, 1, 48000, 32, 4))]);
            f.extend(b"data");
            f.extend(size.to_le_bytes());
            assert_eq!(parse(&f).unwrap().data_len, None);
        }
    }

    #[test]
    fn extensible() {
        let mut body = fmt_body(0xFFFE, 2, 48000, 24, 3);
        body.extend(22u16.to_le_bytes());
        body.extend(20u16.to_le_bytes()); // valid bits
        body.extend(3u32.to_le_bytes()); // channel mask
        body.extend(1u16.to_le_bytes());
        body.extend([0, 0]);
        body.extend(GUID_TAIL);
        let f = wav(&[chunk(b"fmt ", &body), chunk(b"data", &[])]);
        assert_eq!(parse(&f).unwrap().format, SampleFormat::S24);
        // Not the PCM/float GUID family.
        body[30] ^= 1;
        let f = wav(&[chunk(b"fmt ", &body), chunk(b"data", &[])]);
        assert!(parse(&f).unwrap_err().contains("GUID"));
    }

    #[test]
    fn rf64() {
        let mut ds64 = Vec::new();
        ds64.extend(100u64.to_le_bytes());
        ds64.extend(5_000_000_000u64.to_le_bytes());
        ds64.extend(0u64.to_le_bytes());
        ds64.extend(0u32.to_le_bytes());
        let mut f = b"RF64".to_vec();
        f.extend(u32::MAX.to_le_bytes());
        f.extend(b"WAVE");
        f.extend(chunk(b"ds64", &ds64));
        f.extend(chunk(b"fmt ", &fmt_body(1, 2, 44100, 16, 2)));
        f.extend(b"data");
        f.extend(u32::MAX.to_le_bytes());
        assert_eq!(parse(&f).unwrap().data_len, Some(5_000_000_000));
    }

    #[test]
    fn rejections() {
        let e = |f: Vec<u8>| parse(&f).unwrap_err();
        assert!(e(Vec::new()).contains("empty"));
        assert!(e(b"OggS\0\0\0\0\0\0\0\0".to_vec()).contains("Ogg"));
        assert!(e(b"hello world!".to_vec()).contains("--raw"));
        assert!(e(b"hi".to_vec()).contains("not a WAV"));
        assert!(e(b"RIFF\0\0".to_vec()).contains("RIFF header"));
        assert!(e(wav(&[chunk(b"fmt ", &fmt_body(2, 2, 44100, 4, 1))])).contains("ADPCM"));
        assert!(e(wav(&[chunk(b"fmt ", &fmt_body(1, 6, 48000, 16, 2))])).contains("6 channels"));
        assert!(e(wav(&[chunk(b"fmt ", &fmt_body(1, 2, 48000, 12, 1))])).contains("12 bits"));
        assert!(e(wav(&[chunk(b"fmt ", &fmt_body(3, 1, 48000, 16, 2))])).contains("float"));
        assert!(e(wav(&[chunk(b"data", &[0; 4])])).contains("before"));
        assert!(e(wav(&[chunk(b"fmt ", &fmt_body(1, 1, 48000, 16, 2))])).contains("no 'data'"));
        // Truncated inside a skipped chunk and inside fmt.
        let mut f = wav(&[chunk(b"LIST", &[0; 100])]);
        f.truncate(40);
        assert!(e(f).contains("LIST"));
        let mut f = wav(&[chunk(b"fmt ", &fmt_body(1, 1, 48000, 16, 2))]);
        f.truncate(25);
        assert!(e(f).contains("fmt"));
        // Every truncation of a valid header is an error, never a panic.
        let good = wav(&[
            chunk(b"LIST", b"x"),
            chunk(b"fmt ", &fmt_body(1, 2, 44100, 16, 2)),
            chunk(b"data", &[0; 4]),
        ]);
        for n in 0..good.len() - 4 {
            assert!(parse(&good[..n]).is_err(), "{n}");
        }
    }
}
