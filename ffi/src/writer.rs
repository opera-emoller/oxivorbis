//! `oxv_writer`: a streaming encoder that hands its pages to a C callback.

use crate::encoder::planar_arg;
use crate::options::{options_or_default, oxv_options};
use crate::{catch, guard, non_null, oxv_status, rate_channels, slice_arg, store};
use std::ffi::{c_int, c_void};
use std::io;
use vorbis::OggWriter;

/// Sink callback for `oxv_writer`: write all `len` bytes at `data` (e.g. to
/// a file, socket or growing buffer) and return 0, or return non-zero on
/// failure. `user_data` is the pointer given to `oxv_writer_new`. `data` is
/// valid only during the call; `len` is never 0.
///
/// The callback runs synchronously on the thread calling the
/// `oxv_writer_*` function. It must not call any `oxv_writer_*` function on
/// the same writer, and must not unwind (C++ exception) or `longjmp` out.
pub type oxv_write_fn =
    Option<unsafe extern "C" fn(user_data: *mut c_void, data: *const u8, len: usize) -> c_int>;

/// `io::Write` over a C callback.
struct CallbackSink {
    write: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> c_int,
    user_data: *mut c_void,
}

impl io::Write for CallbackSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // SAFETY: `write` is a valid callback per `oxv_writer_new`'s
        // contract, `buf` is valid for `buf.len()` bytes during the call,
        // and `user_data` is passed through untouched.
        let rc = unsafe { (self.write)(self.user_data, buf.as_ptr(), buf.len()) };
        if rc == 0 {
            Ok(buf.len())
        } else {
            Err(io::Error::other("oxv_write_fn returned non-zero"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Streaming encoder writing Ogg pages to an `oxv_write_fn` callback
/// (opaque).
///
/// Create with `oxv_writer_new`, feed PCM with the `oxv_writer_write_*`
/// functions, end the stream with `oxv_writer_finish`, and always release
/// it with `oxv_writer_free`. Each call passes the pages it completed to
/// the callback before returning; nothing else is buffered beyond the
/// encoder's own few blocks. The bytes delivered are exactly those of
/// `oxv_encode_*` on the same input.
///
/// If the callback fails, the call returns `OXV_STATUS_IO`. The pages of
/// that call may have been delivered partially, so the stream is corrupt:
/// later writes and finish return `OXV_STATUS_INVALID_STATE`; only
/// `oxv_writer_free` remains useful. Freeing an unfinished writer leaves
/// the stream truncated (no end-of-stream page).
///
/// **Threads:** as `oxv_encoder` (movable between threads, never used from
/// two at once), provided `user_data` and the callback tolerate being
/// called from whichever thread uses the writer.
pub struct oxv_writer {
    state: State,
    channels: usize,
}

enum State {
    Active(OggWriter<CallbackSink>),
    Finished,
    /// The callback failed or a call panicked.
    Failed,
}

/// Create a writer for `rate` Hz and `channels` channels with `opts` (NULL
/// = defaults; copied) that delivers its output to `write_fn(user_data,
/// ...)`. Nothing is written until the first write or finish call. On
/// success `*out` receives the writer; free it with `oxv_writer_free`.
///
/// Errors as `oxv_encoder_new`; a NULL `write_fn` is
/// `OXV_STATUS_INVALID_ARGUMENT`. `user_data` may be anything, including
/// NULL. On failure `*out` is set to NULL.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`; `out` must be NULL or
/// writable; `write_fn` must be safe to call with `user_data` for as long
/// as the writer lives.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_new(
    rate: u32,
    channels: u32,
    opts: *const oxv_options,
    write_fn: oxv_write_fn,
    user_data: *mut c_void,
    out: *mut *mut oxv_writer,
) -> oxv_status {
    // SAFETY: `out` is NULL or writable per the contract.
    unsafe { store(out, std::ptr::null_mut()) };
    guard(|| {
        non_null(out)?;
        let write = write_fn.ok_or(oxv_status::InvalidArgument)?;
        let (rate, ch) = rate_channels(rate, channels)?;
        let default = oxv_options::default();
        // SAFETY: forwarded from this function's contract.
        let opts = unsafe { options_or_default(opts, &default) };
        let sink = CallbackSink { write, user_data };
        let w = OggWriter::from_encoder(opts.encoder(rate, ch)?, sink);
        let w = Box::new(oxv_writer {
            state: State::Active(w),
            channels: usize::from(ch.count()),
        });
        // SAFETY: non-NULL (checked) and writable per the contract.
        unsafe { out.write(Box::into_raw(w)) };
        Ok(())
    })
}

/// Free a writer (finished or not). NULL is a no-op. Does not call the
/// callback.
///
/// # Safety
///
/// `w` must be NULL or a pointer from `oxv_writer_new` not yet freed, and
/// not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_free(w: *mut oxv_writer) {
    if !w.is_null() {
        // SAFETY: from Box::into_raw in oxv_writer_new and not yet freed,
        // per the caller.
        let w = unsafe { Box::from_raw(w) };
        let _ = catch(move || drop(w));
    }
}

/// Shared body of the write functions.
///
/// # Safety
///
/// `w` must be NULL or a live writer not used elsewhere meanwhile.
unsafe fn writer_with(
    w: *mut oxv_writer,
    f: impl FnOnce(&mut OggWriter<CallbackSink>, usize) -> Result<io::Result<()>, oxv_status>,
) -> oxv_status {
    guard(|| {
        non_null(w)?;
        // SAFETY: non-NULL (checked), live and exclusively ours per the
        // contract.
        let h = unsafe { &mut *w };
        let channels = h.channels;
        let State::Active(ow) = &mut h.state else {
            return Err(oxv_status::InvalidState);
        };
        match catch(|| f(ow, channels)) {
            Some(Ok(Ok(()))) => Ok(()),
            // Argument error: nothing was encoded, the stream is intact.
            Some(Err(s)) => Err(s),
            Some(Ok(Err(_))) => {
                h.state = State::Failed;
                Err(oxv_status::Io)
            }
            None => {
                h.state = State::Failed;
                Err(oxv_status::Panic)
            }
        }
    })
}

/// Encode interleaved 16-bit samples (as `oxv_encoder_write_i16`) and pass
/// the completed pages to the callback.
///
/// # Safety
///
/// `w` must be NULL or a live writer not used on another thread meanwhile;
/// `pcm` must point to `samples` readable `int16_t`s (unless 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_write_i16(
    w: *mut oxv_writer,
    pcm: *const i16,
    samples: usize,
) -> oxv_status {
    let f = |ow: &mut OggWriter<CallbackSink>, _: usize| {
        // SAFETY: `samples` readable values per the contract.
        let pcm = unsafe { slice_arg(pcm, samples) }?;
        Ok(ow.write_i16(pcm))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { writer_with(w, f) }
}

/// Encode interleaved `float` samples (as `oxv_encoder_write_f32`) and pass
/// the completed pages to the callback.
///
/// # Safety
///
/// As `oxv_writer_write_i16`, with `float` samples.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_write_f32(
    w: *mut oxv_writer,
    pcm: *const f32,
    samples: usize,
) -> oxv_status {
    let f = |ow: &mut OggWriter<CallbackSink>, _: usize| {
        // SAFETY: `samples` readable values per the contract.
        let pcm = unsafe { slice_arg(pcm, samples) }?;
        Ok(ow.write_f32(pcm))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { writer_with(w, f) }
}

/// Encode planar `float` audio (as `oxv_encoder_write_planar_f32`, same
/// channel-count rule) and pass the completed pages to the callback.
///
/// # Safety
///
/// `w` as in `oxv_writer_write_i16`; `channels` must point to
/// `channel_count` pointers, each to `frames` readable `float`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_write_planar_f32(
    w: *mut oxv_writer,
    channels: *const *const f32,
    channel_count: usize,
    frames: usize,
) -> oxv_status {
    let f = |ow: &mut OggWriter<CallbackSink>, n: usize| {
        if channel_count != n {
            return Err(oxv_status::InvalidArgument);
        }
        // SAFETY: per the contract.
        let planes = unsafe { planar_arg(channels, channel_count, frames) }?;
        Ok(ow.write_planar_f32(&planes))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { writer_with(w, f) }
}

/// Encode planar 16-bit audio (as `oxv_encoder_write_planar_i16`) and pass
/// the completed pages to the callback.
///
/// # Safety
///
/// As `oxv_writer_write_planar_f32`, with `int16_t` channels.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_write_planar_i16(
    w: *mut oxv_writer,
    channels: *const *const i16,
    channel_count: usize,
    frames: usize,
) -> oxv_status {
    let f = |ow: &mut OggWriter<CallbackSink>, n: usize| {
        if channel_count != n {
            return Err(oxv_status::InvalidArgument);
        }
        // SAFETY: per the contract.
        let planes = unsafe { planar_arg(channels, channel_count, frames) }?;
        Ok(ow.write_planar_i16(&planes))
    };
    // SAFETY: forwarded from this function's contract.
    unsafe { writer_with(w, f) }
}

/// End the stream and pass the remaining pages to the callback.
/// `oxv_writer_free` must still be called. A second finish, or a finish
/// after a failure, returns `OXV_STATUS_INVALID_STATE`.
///
/// # Safety
///
/// `w` must be NULL or a live writer not used on another thread meanwhile.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_writer_finish(w: *mut oxv_writer) -> oxv_status {
    guard(|| {
        non_null(w)?;
        // SAFETY: non-NULL (checked), live and exclusively ours per the
        // contract.
        let h = unsafe { &mut *w };
        let ow = match std::mem::replace(&mut h.state, State::Failed) {
            State::Active(ow) => ow,
            other => {
                h.state = other;
                return Err(oxv_status::InvalidState);
            }
        };
        // A panic or callback failure leaves the state Failed.
        match catch(move || ow.finish()) {
            Some(Ok(_sink)) => {
                h.state = State::Finished;
                Ok(())
            }
            Some(Err(_)) => Err(oxv_status::Io),
            None => Err(oxv_status::Panic),
        }
    })
}
