//! `oxv_encoder`: the streaming encoder, returning Ogg bytes per call.

use crate::options::{options_or_default, oxv_options};
use crate::{catch, guard, non_null, oxv_status, rate_channels, slice_arg, store};
use vorbis::Encoder;

/// Streaming Ogg Vorbis encoder (opaque).
///
/// Create with `oxv_encoder_new`, feed PCM with the `oxv_encoder_write_*`
/// functions in chunks of any size, end the stream with
/// `oxv_encoder_finish`, and always release it with `oxv_encoder_free`
/// (whether or not it was finished). The concatenated output of every
/// write and of finish is the complete Ogg stream, byte-identical to the
/// one-shot `oxv_encode_*` (and the Rust `oxivorbis::encode`) on the same
/// input however it is split.
///
/// **Output buffers:** each write and finish hands back a pointer into a
/// buffer owned by the encoder. Bytes from a write are valid until the next
/// call that takes this encoder (including `oxv_encoder_free`); bytes from
/// `oxv_encoder_finish` are valid until `oxv_encoder_free`. Copy or write
/// them out before then. Never free them yourself.
///
/// **Threads:** an encoder may be created on one thread and used or freed
/// on another, but must not be used from two threads at once. Different
/// encoders are independent; every function is reentrant.
pub struct oxv_encoder {
    state: State,
    channels: usize,
}

enum State {
    Active(Encoder),
    /// `oxv_encoder_finish` returned these bytes; they live until free.
    /// Never read again from Rust: C holds a pointer into them.
    Finished(#[allow(dead_code)] Vec<u8>),
    /// A call panicked; the encoder state is unknown.
    Failed,
}

/// Create an encoder for `rate` Hz (`OXV_SAMPLE_RATE_MIN` to
/// `OXV_SAMPLE_RATE_MAX`) and `channels` channels (1 to
/// `OXV_CHANNELS_MAX`, interleaved in Vorbis channel order; see
/// ffi/README.md) with `opts` (NULL = defaults; copied, so it may be freed
/// afterwards). On success `*out` receives the encoder; free it with
/// `oxv_encoder_free`.
///
/// Returns `OXV_STATUS_UNSUPPORTED` for an unsupported rate or channel
/// count, or a bitrate mode (`oxv_options_set_bitrate`) libvorbis can't
/// encode at this rate and channel count (`oxv_options_check` says why),
/// and `OXV_STATUS_INVALID_ARGUMENT` if `out` is NULL. On failure `*out`
/// is set to NULL.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`; `out` must be NULL or valid
/// for writing a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_new(
    rate: u32,
    channels: u32,
    opts: *const oxv_options,
    out: *mut *mut oxv_encoder,
) -> oxv_status {
    // SAFETY: `out` is NULL or writable per the contract.
    unsafe { store(out, std::ptr::null_mut()) };
    guard(|| {
        non_null(out)?;
        let (rate, ch) = rate_channels(rate, channels)?;
        let default = oxv_options::default();
        // SAFETY: forwarded from this function's contract.
        let opts = unsafe { options_or_default(opts, &default) };
        let enc = Box::new(oxv_encoder {
            state: State::Active(opts.encoder(rate, ch)?),
            channels: usize::from(ch.count()),
        });
        // SAFETY: non-NULL (checked) and writable per the contract.
        unsafe { out.write(Box::into_raw(enc)) };
        Ok(())
    })
}

/// Free an encoder (finished or not). NULL is a no-op. Invalidates every
/// buffer the encoder returned.
///
/// # Safety
///
/// `enc` must be NULL or a pointer from `oxv_encoder_new` not yet freed,
/// and not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_free(enc: *mut oxv_encoder) {
    if !enc.is_null() {
        // SAFETY: from Box::into_raw in oxv_encoder_new and not yet freed,
        // per the caller.
        let enc = unsafe { Box::from_raw(enc) };
        // Dropping only frees memory; catch anyway so nothing unwinds.
        let _ = catch(move || drop(enc));
    }
}

/// Shared body of the write functions: reset the out-pointers, check them
/// and the state, run `f` on the live encoder and publish its output.
///
/// # Safety
///
/// `enc` must be NULL or a live encoder not used elsewhere meanwhile;
/// `out_ptr` / `out_len` must be NULL or writable.
unsafe fn write_with(
    enc: *mut oxv_encoder,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
    f: impl FnOnce(&mut Encoder, usize) -> Result<(*const u8, usize), oxv_status>,
) -> oxv_status {
    // SAFETY: NULL or writable per the contract.
    unsafe { store(out_ptr, std::ptr::null()) };
    // SAFETY: NULL or writable per the contract.
    unsafe { store(out_len, 0) };
    guard(|| {
        non_null(out_ptr)?;
        non_null(out_len)?;
        non_null(enc)?;
        // SAFETY: non-NULL (checked), live and exclusively ours per the
        // contract.
        let h = unsafe { &mut *enc };
        let channels = h.channels;
        let State::Active(e) = &mut h.state else {
            return Err(oxv_status::InvalidState);
        };
        let (p, n) = match catch(|| f(e, channels)) {
            Some(r) => r?,
            None => {
                h.state = State::Failed;
                return Err(oxv_status::Panic);
            }
        };
        // SAFETY: both non-NULL (checked) and writable per the contract.
        unsafe { out_ptr.write(p) };
        // SAFETY: as above.
        unsafe { out_len.write(n) };
        Ok(())
    })
}

/// `(ptr, len)` of a byte slice for C. The pointer is never NULL.
fn raw(bytes: &[u8]) -> (*const u8, usize) {
    (bytes.as_ptr(), bytes.len())
}

/// Encode `samples` interleaved 16-bit samples (scaled by 1/32768, as
/// libvorbis tools do). `samples` counts individual samples, not frames,
/// and need not be a multiple of the channel count: a trailing partial
/// frame is carried over to the next write. `pcm` may be NULL when
/// `samples` is 0.
///
/// On success `*out_ptr` / `*out_len` receive the Ogg bytes (whole pages)
/// completed by this call, possibly none (`*out_len == 0`; `*out_ptr` is
/// then non-NULL but must not be read). The bytes are owned by the encoder
/// and valid until the next call that takes `enc`. On failure they are set
/// to NULL / 0.
///
/// Errors: `OXV_STATUS_INVALID_ARGUMENT` for NULL or misaligned pointers,
/// `OXV_STATUS_INVALID_STATE` after `oxv_encoder_finish` or a panic.
///
/// # Safety
///
/// `enc` must be NULL or a live encoder not used on another thread
/// meanwhile; `pcm` must point to `samples` readable `int16_t`s (unless
/// `samples` is 0); `out_ptr` / `out_len` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_write_i16(
    enc: *mut oxv_encoder,
    pcm: *const i16,
    samples: usize,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> oxv_status {
    let f = |e: &mut Encoder, _: usize| {
        // SAFETY: `pcm` holds `samples` values per the contract.
        let pcm = unsafe { slice_arg(pcm, samples) }?;
        Ok(raw(e.write(pcm)))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { write_with(enc, out_ptr, out_len, f) }
}

/// Encode `samples` interleaved `float` samples, nominally in -1.0..=1.0
/// (passed to the encoder unscaled, as libvorbis's
/// `vorbis_analysis_buffer`; NaN and infinities are encoded as 0.0).
/// Otherwise identical to `oxv_encoder_write_i16`, and both may be mixed
/// on one stream.
///
/// # Safety
///
/// As `oxv_encoder_write_i16`, with `pcm` pointing to `samples` `float`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_write_f32(
    enc: *mut oxv_encoder,
    pcm: *const f32,
    samples: usize,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> oxv_status {
    let f = |e: &mut Encoder, _: usize| {
        // SAFETY: `pcm` holds `samples` values per the contract.
        let pcm = unsafe { slice_arg(pcm, samples) }?;
        Ok(raw(e.write_f32(pcm)))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { write_with(enc, out_ptr, out_len, f) }
}

/// Borrow `count` channel pointers of `frames` samples each.
///
/// # Safety
///
/// `channels` must point to `count` pointers (if `count > 0`), each to
/// `frames` readable `T`s (if `frames > 0`), valid for `'a`.
pub(crate) unsafe fn planar_arg<'a, T>(
    channels: *const *const T,
    count: usize,
    frames: usize,
) -> Result<Vec<&'a [T]>, oxv_status> {
    non_null(channels)?;
    // SAFETY: `count` readable pointers per the contract.
    let ptrs = unsafe { slice_arg(channels, count) }?;
    ptrs.iter()
        // SAFETY: each pointer has `frames` readable values per the contract.
        .map(|&p| unsafe { slice_arg(p, frames) })
        .collect()
}

/// Encode planar `float` audio: `channels` points to `channel_count`
/// pointers (one per channel, in Vorbis channel order: left, right for
/// stereo; left, center, right, rear left, rear right, LFE for 5.1), each
/// to `frames` samples. Gives exactly the bytes of
/// interleaving the channels and calling `oxv_encoder_write_f32`; output
/// as there.
///
/// `channel_count` must equal the encoder's channel count, else
/// `OXV_STATUS_INVALID_ARGUMENT` (nothing is encoded). Channel pointers may
/// be NULL only when `frames` is 0.
///
/// # Safety
///
/// `enc`, `out_ptr`, `out_len` as in `oxv_encoder_write_i16`; `channels`
/// must point to `channel_count` pointers, each to `frames` readable
/// `float`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_write_planar_f32(
    enc: *mut oxv_encoder,
    channels: *const *const f32,
    channel_count: usize,
    frames: usize,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> oxv_status {
    let f = |e: &mut Encoder, n: usize| {
        if channel_count != n {
            return Err(oxv_status::InvalidArgument);
        }
        // SAFETY: per the contract.
        let planes = unsafe { planar_arg(channels, channel_count, frames) }?;
        Ok(raw(e.write_planar_f32(&planes)))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { write_with(enc, out_ptr, out_len, f) }
}

/// Encode planar 16-bit audio; the `int16_t` counterpart of
/// `oxv_encoder_write_planar_f32`, with the same rules.
///
/// # Safety
///
/// As `oxv_encoder_write_planar_f32`, with `int16_t` channels.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_write_planar_i16(
    enc: *mut oxv_encoder,
    channels: *const *const i16,
    channel_count: usize,
    frames: usize,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> oxv_status {
    let f = |e: &mut Encoder, n: usize| {
        if channel_count != n {
            return Err(oxv_status::InvalidArgument);
        }
        // SAFETY: per the contract.
        let planes = unsafe { planar_arg(channels, channel_count, frames) }?;
        Ok(raw(e.write_planar_i16(&planes)))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { write_with(enc, out_ptr, out_len, f) }
}

/// End the stream: flush buffered input and return the remaining Ogg bytes
/// (never empty) in `*out_ptr` / `*out_len`. A trailing partial frame is
/// dropped. The bytes are owned by the encoder and valid until
/// `oxv_encoder_free`, which must still be called.
///
/// After finish, writes and a second finish return
/// `OXV_STATUS_INVALID_STATE`. On failure `*out_ptr` / `*out_len` are set to
/// NULL / 0.
///
/// # Safety
///
/// As `oxv_encoder_write_i16` for `enc`, `out_ptr` and `out_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_encoder_finish(
    enc: *mut oxv_encoder,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> oxv_status {
    // SAFETY: NULL or writable per the contract.
    unsafe { store(out_ptr, std::ptr::null()) };
    // SAFETY: NULL or writable per the contract.
    unsafe { store(out_len, 0) };
    guard(|| {
        non_null(out_ptr)?;
        non_null(out_len)?;
        non_null(enc)?;
        // SAFETY: non-NULL (checked), live and exclusively ours per the
        // contract.
        let h = unsafe { &mut *enc };
        let e = match std::mem::replace(&mut h.state, State::Failed) {
            State::Active(e) => e,
            other => {
                h.state = other;
                return Err(oxv_status::InvalidState);
            }
        };
        // On a panic the state stays Failed.
        let bytes = catch(move || e.finish()).ok_or(oxv_status::Panic)?;
        let (p, n) = raw(&bytes);
        // Moving the Vec into the state keeps its heap buffer, so `p` stays
        // valid until the encoder is freed.
        h.state = State::Finished(bytes);
        // SAFETY: both non-NULL (checked) and writable per the contract.
        unsafe { out_ptr.write(p) };
        // SAFETY: as above.
        unsafe { out_len.write(n) };
        Ok(())
    })
}
