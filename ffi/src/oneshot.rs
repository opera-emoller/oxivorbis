//! One-shot encoding into a library-allocated `oxv_buffer`.

use crate::options::{options_or_default, oxv_options};
use crate::{catch, guard, non_null, oxv_status, rate_channels, slice_arg, store};

/// A byte buffer allocated by liboxivorbis, e.g. a whole Ogg stream from
/// `oxv_encode_i16`. Read `data[0..len]`; release it with
/// `oxv_buffer_free` (never with `free()`), and don't change the fields
/// before then. An empty buffer is `{NULL, 0}`.
#[repr(C)]
pub struct oxv_buffer {
    /// The bytes, or NULL if empty.
    pub data: *mut u8,
    /// Number of bytes at `data`.
    pub len: usize,
}

impl oxv_buffer {
    /// `{NULL, 0}`.
    fn empty() -> oxv_buffer {
        oxv_buffer {
            data: std::ptr::null_mut(),
            len: 0,
        }
    }

    fn from_vec(v: Vec<u8>) -> oxv_buffer {
        if v.is_empty() {
            return oxv_buffer::empty();
        }
        let len = v.len();
        let data = Box::into_raw(v.into_boxed_slice()).cast::<u8>();
        oxv_buffer { data, len }
    }
}

/// Free the bytes of `buf` and reset it to `{NULL, 0}`. NULL `buf`, or an
/// already-empty buffer, is a no-op, so freeing twice is harmless.
///
/// # Safety
///
/// `buf` must be NULL or point to an `oxv_buffer` filled in by
/// liboxivorbis (or `{NULL, 0}`), with its fields unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_buffer_free(buf: *mut oxv_buffer) {
    if buf.is_null() {
        return;
    }
    // SAFETY: non-NULL and a valid oxv_buffer per the contract; reading
    // and resetting it first makes a second free a no-op.
    let b = unsafe { buf.replace(oxv_buffer::empty()) };
    if !b.data.is_null() {
        let slice = std::ptr::slice_from_raw_parts_mut(b.data, b.len);
        // SAFETY: `data`/`len` came from `Box<[u8]>::into_raw` in
        // `from_vec` and were not changed (contract), so this is the same
        // allocation with the same length.
        let boxed = unsafe { Box::from_raw(slice) };
        let _ = catch(move || drop(boxed));
    }
}

/// Shared body of the one-shot functions.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`; `out` NULL or writable.
unsafe fn encode_with(
    rate: u32,
    channels: u32,
    opts: *const oxv_options,
    out: *mut oxv_buffer,
    f: impl FnOnce(
        vorbis::SampleRate,
        vorbis::Channels,
        &vorbis::Options,
    ) -> Result<Vec<u8>, oxv_status>,
) -> oxv_status {
    // SAFETY: NULL or writable per the contract.
    unsafe { store(out, oxv_buffer::empty()) };
    guard(|| {
        non_null(out)?;
        let (rate, ch) = rate_channels(rate, channels)?;
        let default = vorbis::Options::default();
        // SAFETY: forwarded from this function's contract.
        let opts = unsafe { options_or_default(opts, &default) };
        let bytes = f(rate, ch, opts)?;
        // SAFETY: non-NULL (checked) and writable per the contract.
        unsafe { out.write(oxv_buffer::from_vec(bytes)) };
        Ok(())
    })
}

/// Encode `samples` interleaved 16-bit samples into a complete Ogg Vorbis
/// stream in `*out` (free with `oxv_buffer_free`). Same bytes as streaming
/// the input through an `oxv_encoder`; a trailing partial frame is
/// ignored. `pcm` may be NULL when `samples` is 0. `opts` NULL means the
/// defaults.
///
/// Errors: `OXV_STATUS_UNSUPPORTED` for the rate or channel count,
/// `OXV_STATUS_INVALID_ARGUMENT` for NULL / misaligned pointers. On failure
/// `*out` is set to `{NULL, 0}`.
///
/// # Safety
///
/// `pcm` must point to `samples` readable `int16_t`s (unless 0); `opts`
/// must be NULL or a live `oxv_options`; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encode_i16(
    pcm: *const i16,
    samples: usize,
    rate: u32,
    channels: u32,
    opts: *const oxv_options,
    out: *mut oxv_buffer,
) -> oxv_status {
    let f = |r: vorbis::SampleRate, c: vorbis::Channels, o: &vorbis::Options| {
        // SAFETY: `samples` readable values per the contract.
        let pcm = unsafe { slice_arg(pcm, samples) }?;
        Ok(vorbis::encode(pcm, r, c, o))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { encode_with(rate, channels, opts, out, f) }
}

/// The `float` counterpart of `oxv_encode_i16` (samples nominally in
/// -1.0..=1.0, unscaled; see `oxv_encoder_write_f32`).
///
/// # Safety
///
/// As `oxv_encode_i16`, with `pcm` pointing to `samples` `float`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encode_f32(
    pcm: *const f32,
    samples: usize,
    rate: u32,
    channels: u32,
    opts: *const oxv_options,
    out: *mut oxv_buffer,
) -> oxv_status {
    let f = |r: vorbis::SampleRate, c: vorbis::Channels, o: &vorbis::Options| {
        // SAFETY: `samples` readable values per the contract.
        let pcm = unsafe { slice_arg(pcm, samples) }?;
        Ok(vorbis::encode_f32(pcm, r, c, o))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { encode_with(rate, channels, opts, out, f) }
}
