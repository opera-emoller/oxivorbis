//! Planar (one slice per channel) input for [`Encoder`].
//!
//! The planar methods interleave through a fixed stack buffer and feed the
//! encoder core in chunks, so they allocate nothing and give exactly the
//! bytes of interleaving the input yourself and calling
//! [`Encoder::write`] / [`Encoder::write_f32`]. Mono input is passed
//! straight through without a copy.

use crate::Encoder;
use crate::encode::{self, EncoderCore};

/// Interleave scratch size in samples (1024 stereo frames; 8 KiB of `f32`
/// or 4 KiB of `i16` on the stack).
const SCRATCH: usize = 2048;

impl Encoder {
    /// Encode planar `f32` audio: one slice per channel (`[left, right]`
    /// for stereo), the layout most DSP and decoding crates produce.
    /// Returns the Ogg bytes completed by this call, like
    /// [`write_f32`](Self::write_f32), and gives exactly the bytes of
    /// interleaving the channels and calling `write_f32`. Samples are
    /// interpreted as in [`write_f32`](Self::write_f32).
    ///
    /// Accepts anything slice-of-slices shaped: `&[&[f32]]`,
    /// `&[Vec<f32>]`, `&[left, right]`, ...
    ///
    /// - **Unequal lengths:** only as many frames as the *shortest*
    ///   channel holds are encoded; the excess in longer channels is
    ///   ignored (not carried over to the next call). This never panics.
    /// - **Wrong channel count:** `channels` must hold one slice per
    ///   encoder channel. Any other count is a logic error: it does not
    ///   panic and the stream stays valid, but the audio it encodes is
    ///   unspecified.
    ///
    /// Planar and interleaved writes may be mixed. Nothing is allocated
    /// per call.
    ///
    /// ```
    /// use oxivorbis::{Channels, Encoder, Options, SampleRate};
    ///
    /// let left: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.03).sin() * 0.5).collect();
    /// let right: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    ///
    /// let mut enc = Encoder::new(SampleRate::Hz44100, Channels::Stereo, &Options::default());
    /// let mut ogg = enc.write_planar_f32(&[&left, &right]).to_vec();
    /// ogg.extend_from_slice(&enc.finish());
    ///
    /// // Same bytes as interleaving first.
    /// let interleaved: Vec<f32> = left.iter().zip(&right).flat_map(|(&l, &r)| [l, r]).collect();
    /// let one_shot = oxivorbis::encode_f32(
    ///     &interleaved, SampleRate::Hz44100, Channels::Stereo, &Options::default());
    /// assert_eq!(ogg, one_shot);
    /// ```
    pub fn write_planar_f32<S: AsRef<[f32]>>(&mut self, channels: &[S]) -> &[u8] {
        self.core.begin_write();
        feed_planar(&mut self.core, channels, encode::f32_sample);
        self.core.take_output()
    }

    /// Encode planar 16-bit audio: one slice per channel. The `i16`
    /// counterpart of [`write_planar_f32`](Self::write_planar_f32), with
    /// the same length and channel-count rules; gives exactly the bytes of
    /// interleaving the channels and calling [`write`](Self::write).
    ///
    /// ```
    /// use oxivorbis::{Channels, Encoder, Options, SampleRate};
    ///
    /// let left = vec![1000i16; 4800];
    /// let right = vec![-1000i16; 4800];
    /// let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Stereo, &Options::default());
    /// let mut ogg = enc.write_planar_i16(&[left, right]).to_vec();
    /// ogg.extend_from_slice(&enc.finish());
    /// assert_eq!(&ogg[..4], b"OggS");
    /// ```
    pub fn write_planar_i16<S: AsRef<[i16]>>(&mut self, channels: &[S]) -> &[u8] {
        self.core.begin_write();
        feed_planar(&mut self.core, channels, encode::i16_to_f32);
        self.core.take_output()
    }
}

/// Interleave the first `min(len)` frames of `channels` and feed them to
/// `core`, `SCRATCH` samples at a time. The core accepts any chunking
/// (even one that splits a frame), so the chunk size never affects output.
fn feed_planar<T, S, F>(core: &mut EncoderCore, channels: &[S], conv: F)
where
    T: Copy + Default,
    S: AsRef<[T]>,
    F: Fn(T) -> f32 + Copy,
{
    let frames = channels.iter().map(|c| c.as_ref().len()).min().unwrap_or(0);
    match channels {
        [] => {}
        [mono] => core.feed(&mono.as_ref()[..frames], conv),
        [l, r] => {
            let (l, r) = (&l.as_ref()[..frames], &r.as_ref()[..frames]);
            let mut buf = [T::default(); SCRATCH];
            for (lc, rc) in l.chunks(SCRATCH / 2).zip(r.chunks(SCRATCH / 2)) {
                let out = &mut buf[..lc.len() * 2];
                for ((o, &a), &b) in out.chunks_exact_mut(2).zip(lc).zip(rc) {
                    o[0] = a;
                    o[1] = b;
                }
                core.feed(out, conv);
            }
        }
        _ => {
            // Any other count: a generic (slower) interleave.
            let mut buf = [T::default(); SCRATCH];
            let mut n = 0;
            for f in 0..frames {
                for c in channels {
                    buf[n] = c.as_ref()[f];
                    n += 1;
                    if n == SCRATCH {
                        core.feed(&buf, conv);
                        n = 0;
                    }
                }
            }
            core.feed(&buf[..n], conv);
        }
    }
}
