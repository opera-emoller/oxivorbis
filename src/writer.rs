//! [`OggWriter`]: an [`Encoder`] that writes its pages to any
//! [`std::io::Write`] sink, and [`S16LeWriter`], which accepts raw s16le
//! PCM bytes through `std::io::Write` itself.

use crate::{Channels, Encoder, Options, SampleRate};
use std::io::{self, Write};

/// Streaming Ogg Vorbis encoder that writes to a sink: a [`File`], a
/// socket, a `Vec<u8>`, anything [`Write`].
///
/// Each `write_*` call encodes its input and writes the Ogg pages it
/// completed straight to the sink with [`Write::write_all`]; nothing is
/// buffered beyond what [`Encoder`] itself holds. Call
/// [`finish`](Self::finish) to end the stream; it returns the sink.
/// The bytes written are exactly those of [`encode`](crate::encode) /
/// [`encode_f32`](crate::encode_f32) on the same input, however it is
/// split across calls.
///
/// Dropping an `OggWriter` without calling `finish` leaves the stream
/// truncated (no final pages, no end-of-stream flag).
///
/// # Errors
///
/// Errors come only from the sink. After a sink error the encoder state
/// is still consistent and further calls do not panic, but the pages that
/// call produced may have been written to the sink partially or not at
/// all, so the stream in the sink is corrupt. Treat any error as fatal for
/// the stream.
///
/// The sink is not flushed by the `write_*` methods (wrap a `File` or
/// socket in a [`BufWriter`](std::io::BufWriter) if you write small
/// chunks); [`finish`](Self::finish) flushes it.
///
/// ```
/// use oxivorbis::{Channels, OggWriter, Options, SampleRate};
///
/// # fn main() -> std::io::Result<()> {
/// let pcm = vec![0i16; 44_100 * 2]; // 1 s of stereo silence
/// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Stereo, &Options::default())?;
/// for chunk in pcm.chunks(4096) {
///     w.write_i16(chunk)?;
/// }
/// let ogg: Vec<u8> = w.finish()?;
/// assert_eq!(ogg, oxivorbis::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &Options::default()));
/// # Ok(())
/// # }
/// ```
///
/// To a file:
///
/// ```no_run
/// use oxivorbis::{Channels, OggWriter, Options, SampleRate};
/// use std::{fs::File, io::BufWriter};
///
/// # fn main() -> std::io::Result<()> {
/// let file = BufWriter::new(File::create("out.ogg")?);
/// let mut w = OggWriter::new(file, SampleRate::Hz48000, Channels::Mono, &Options::default())?;
/// w.write_f32(&[0.0; 48_000])?;
/// w.finish()?;
/// # Ok(())
/// # }
/// ```
///
/// [`File`]: std::fs::File
pub struct OggWriter<W: Write> {
    encoder: Encoder,
    sink: W,
}

impl<W: Write> OggWriter<W> {
    /// New writer encoding `rate` / `channels` with `options` into `sink`.
    ///
    /// Nothing is written to the sink yet; the header pages go out with
    /// the first `write_*` call (or with `finish`). This currently always
    /// succeeds; the `io::Result` leaves room for rate / channel / quality
    /// combinations that can be rejected, which would be reported as
    /// [`io::ErrorKind::InvalidInput`] wrapping [`Unsupported`](crate::Unsupported).
    ///
    /// ```
    /// use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    ///
    /// let w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Mono, &Options::default())?;
    /// let ogg = w.finish()?; // an empty but complete stream
    /// assert_eq!(&ogg[..4], b"OggS");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn new(
        sink: W,
        rate: SampleRate,
        channels: Channels,
        options: &Options,
    ) -> io::Result<Self> {
        Ok(Self::from_encoder(
            Encoder::new(rate, channels, options),
            sink,
        ))
    }

    /// Wrap an existing [`Encoder`]. If the encoder has already returned
    /// some output from `write`, the caller is responsible for having
    /// written that to `sink` first.
    ///
    /// ```
    /// use oxivorbis::{Channels, Encoder, OggWriter, Options, SampleRate};
    ///
    /// let enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
    /// let mut w = OggWriter::from_encoder(enc, Vec::new());
    /// w.write_i16(&[0; 1024])?;
    /// let ogg = w.finish()?;
    /// # assert_eq!(&ogg[..4], b"OggS");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn from_encoder(encoder: Encoder, sink: W) -> Self {
        Self { encoder, sink }
    }

    /// Encode interleaved 16-bit samples (see [`Encoder::write`]) and
    /// write the completed pages to the sink.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Stereo, &Options::default())?;
    /// w.write_i16(&[0, 0, 100, -100])?; // two stereo frames
    /// # w.finish()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_i16(&mut self, interleaved: &[i16]) -> io::Result<()> {
        let pages = self.encoder.write(interleaved);
        self.sink.write_all(pages)
    }

    /// Encode interleaved `f32` samples (see [`Encoder::write_f32`]) and
    /// write the completed pages to the sink.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz48000, Channels::Mono, &Options::default())?;
    /// w.write_f32(&[0.0, 0.25, 0.5])?;
    /// # w.finish()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_f32(&mut self, interleaved: &[f32]) -> io::Result<()> {
        let pages = self.encoder.write_f32(interleaved);
        self.sink.write_all(pages)
    }

    /// Encode planar `f32` audio, one slice per channel (see
    /// [`Encoder::write_planar_f32`] for the length rules), and write the
    /// completed pages to the sink.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// let (left, right) = (vec![0.1f32; 1000], vec![-0.1f32; 1000]);
    /// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Stereo, &Options::default())?;
    /// w.write_planar_f32(&[&left, &right])?;
    /// # w.finish()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_planar_f32<S: AsRef<[f32]>>(&mut self, channels: &[S]) -> io::Result<()> {
        let pages = self.encoder.write_planar_f32(channels);
        self.sink.write_all(pages)
    }

    /// Encode planar 16-bit audio, one slice per channel (see
    /// [`Encoder::write_planar_i16`]), and write the completed pages to
    /// the sink.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Mono, &Options::default())?;
    /// w.write_planar_i16(&[vec![0i16; 1000]])?;
    /// # w.finish()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_planar_i16<S: AsRef<[i16]>>(&mut self, channels: &[S]) -> io::Result<()> {
        let pages = self.encoder.write_planar_i16(channels);
        self.sink.write_all(pages)
    }

    /// End the stream: encode the buffered input, write the remaining
    /// pages, flush the sink and return it. A stereo sample still waiting
    /// for its pair is dropped, as in [`Encoder::finish`]. On error the
    /// sink is dropped.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Mono, &Options::default())?;
    /// w.write_i16(&[0; 44_100])?;
    /// let ogg: Vec<u8> = w.finish()?;
    /// # assert_eq!(&ogg[..4], b"OggS");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn finish(mut self) -> io::Result<W> {
        let tail = self.encoder.finish();
        self.sink.write_all(&tail)?;
        self.sink.flush()?;
        Ok(self.sink)
    }

    /// The sink.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// let w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Mono, &Options::default())?;
    /// assert!(w.get_ref().is_empty()); // headers go out with the first write
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn get_ref(&self) -> &W {
        &self.sink
    }

    /// The sink, mutably. Writing to it directly corrupts the stream;
    /// this is meant for things like flushing or inspecting it.
    ///
    /// ```
    /// # use oxivorbis::{Channels, OggWriter, Options, SampleRate};
    /// use std::io::Write;
    /// let mut w = OggWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Mono, &Options::default())?;
    /// w.write_i16(&[0; 44_100])?;
    /// w.get_mut().flush()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.sink
    }
}

/// Interleave conversion scratch, in samples.
const SCRATCH: usize = 2048;

/// An [`OggWriter`] that takes raw **interleaved signed 16-bit
/// little-endian** PCM bytes (`s16le`, as from `ffmpeg -f s16le` or a WAV
/// data chunk) through [`std::io::Write`], so it works with
/// [`std::io::copy`] and anything else that writes bytes.
///
/// Writes may split samples and frames anywhere: an odd trailing byte is
/// carried over and joined with the first byte of the next write.
/// [`finish`](Self::finish) ends the stream (dropping a dangling odd byte,
/// and a dangling unpaired stereo sample) and returns the sink.
///
/// [`Write::flush`] flushes the sink. It cannot force out audio the
/// encoder is still holding; only `finish` does that.
///
/// # Errors
///
/// As for [`OggWriter`]: errors come only from the sink and are fatal for
/// the stream. Unlike the general `io::Write` contract, a `write` that
/// fails may already have encoded a prefix of its input.
///
/// ```
/// use oxivorbis::{Channels, Options, S16LeWriter, SampleRate};
///
/// # fn main() -> std::io::Result<()> {
/// let pcm = vec![0i16; 44_100 * 2];
/// let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
///
/// let mut w = S16LeWriter::new(Vec::new(), SampleRate::Hz44100, Channels::Stereo, &Options::default())?;
/// std::io::copy(&mut &bytes[..], &mut w)?;
/// let ogg = w.finish()?;
/// assert_eq!(ogg, oxivorbis::encode(&pcm, SampleRate::Hz44100, Channels::Stereo, &Options::default()));
/// # Ok(())
/// # }
/// ```
pub struct S16LeWriter<W: Write> {
    inner: OggWriter<W>,
    odd: Option<u8>,
}

impl<W: Write> S16LeWriter<W> {
    /// New writer; see [`OggWriter::new`].
    ///
    /// ```
    /// # use oxivorbis::{Channels, Options, S16LeWriter, SampleRate};
    /// let w = S16LeWriter::new(Vec::new(), SampleRate::Hz48000, Channels::Mono, &Options::default())?;
    /// # w.finish()?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn new(
        sink: W,
        rate: SampleRate,
        channels: Channels,
        options: &Options,
    ) -> io::Result<Self> {
        OggWriter::new(sink, rate, channels, options).map(Self::from)
    }

    /// End the stream and return the flushed sink; see
    /// [`OggWriter::finish`]. A dangling odd byte is dropped.
    ///
    /// ```
    /// # use oxivorbis::{Channels, Options, S16LeWriter, SampleRate};
    /// use std::io::Write;
    /// let mut w = S16LeWriter::new(Vec::new(), SampleRate::Hz48000, Channels::Mono, &Options::default())?;
    /// w.write_all(&[0x00, 0x10, 0xff])?; // one sample and a dangling byte
    /// let ogg = w.finish()?;
    /// # assert_eq!(&ogg[..4], b"OggS");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn finish(self) -> io::Result<W> {
        self.inner.finish()
    }

    /// The sink.
    ///
    /// ```
    /// # use oxivorbis::{Channels, Options, S16LeWriter, SampleRate};
    /// let w = S16LeWriter::new(Vec::new(), SampleRate::Hz48000, Channels::Mono, &Options::default())?;
    /// assert!(w.get_ref().is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref()
    }

    /// The sink, mutably; see [`OggWriter::get_mut`].
    ///
    /// ```
    /// # use oxivorbis::{Channels, Options, S16LeWriter, SampleRate};
    /// let mut w = S16LeWriter::new(Vec::new(), SampleRate::Hz48000, Channels::Mono, &Options::default())?;
    /// let _sink: &mut Vec<u8> = w.get_mut();
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut()
    }
}

impl<W: Write> From<OggWriter<W>> for S16LeWriter<W> {
    /// Take s16le bytes into an existing [`OggWriter`].
    fn from(inner: OggWriter<W>) -> Self {
        Self { inner, odd: None }
    }
}

impl<W: Write> Write for S16LeWriter<W> {
    /// Encode `buf` as s16le PCM. Always consumes all of `buf` on success.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes = buf;
        let mut samples = [0i16; SCRATCH];
        let mut n = 0;
        if let Some(lo) = self.odd {
            let Some((&hi, rest)) = bytes.split_first() else {
                return Ok(0);
            };
            self.odd = None;
            samples[0] = i16::from_le_bytes([lo, hi]);
            n = 1;
            bytes = rest;
        }
        let mut pairs = bytes.chunks_exact(2);
        for p in pairs.by_ref() {
            samples[n] = i16::from_le_bytes([p[0], p[1]]);
            n += 1;
            if n == SCRATCH {
                self.inner.write_i16(&samples)?;
                n = 0;
            }
        }
        self.inner.write_i16(&samples[..n])?;
        self.odd = pairs.remainder().first().copied();
        Ok(buf.len())
    }

    /// Flush the sink.
    fn flush(&mut self) -> io::Result<()> {
        self.inner.sink.flush()
    }
}
