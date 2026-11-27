//! C API for [oxivorbis](https://github.com/opera-emoller/oxivorbis).
//!
//! This crate builds `liboxivorbis` (cdylib + staticlib) and is described
//! by `include/oxivorbis.h`, which cbindgen generates from the doc comments
//! here (`just ffi-header`). See `ffi/README.md` for building, linking,
//! ownership and threading rules.
//!
//! Conventions, applied to every function:
//!
//! - Every fallible function returns an [`oxv_status`]; `OXV_STATUS_OK` is 0.
//! - Every pointer argument is checked for NULL (except where NULL is
//!   documented as meaningful) and reported as
//!   `OXV_STATUS_INVALID_ARGUMENT`.
//! - Out-pointers are written on success, and reset (NULL / 0) on failure
//!   when they are themselves non-NULL.
//! - No Rust panic unwinds into C: each entry point catches it and returns
//!   `OXV_STATUS_PANIC` (the library is designed not to panic; this is a
//!   backstop). An encoder or writer that panicked is unusable afterwards.

#![allow(non_camel_case_types)]

mod encoder;
mod oneshot;
mod options;
mod writer;

pub use encoder::*;
pub use oneshot::*;
pub use options::*;
pub use writer::*;

use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// ABI version of this header. Bumped on any incompatible change to a
/// function signature, struct layout or enum value. New functions and new
/// status values do not bump it. Compare against `oxv_abi_version()` to
/// detect a header / library mismatch at run time.
pub const OXV_ABI_VERSION: u32 = 1;

/// Lowest supported sample rate in Hz.
pub const OXV_SAMPLE_RATE_MIN: u32 = 1;

/// Highest supported sample rate in Hz (libvorbis has no template above).
pub const OXV_SAMPLE_RATE_MAX: u32 = 200_000;

/// Most channels a stream can have (the Vorbis ID header stores the count
/// in 8 bits). Every count from 1 up is supported, in Vorbis channel order.
pub const OXV_CHANNELS_MAX: u32 = 255;

/// Result of every fallible call. `OXV_STATUS_OK` (0) is success; any other
/// value is an error, described by `oxv_status_message`. New values may be
/// added in later versions, so treat unknown non-zero values as errors.
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum oxv_status {
    /// Success.
    Ok = 0,
    /// A NULL pointer where one is not allowed, a misaligned or oversized
    /// buffer, a NaN or out-of-range quality, a string that is not UTF-8,
    /// or a planar channel count that does not match the encoder.
    InvalidArgument = 1,
    /// A sample rate or channel count oxivorbis does not encode, or a
    /// bitrate mode libvorbis has no setup for at the rate and channel
    /// count (`oxv_options_check` describes it).
    Unsupported = 2,
    /// The write callback of an `oxv_writer` reported an error.
    Io = 3,
    /// The encoder or writer can't take this call: it was already
    /// finished, or an earlier call failed with `OXV_STATUS_IO` or
    /// `OXV_STATUS_PANIC`.
    InvalidState = 4,
    /// Internal error: oxivorbis panicked. Please report it.
    Panic = 5,
}

/// The version of liboxivorbis, e.g. `"0.1.0"`, as a static NUL-terminated
/// string. Never NULL; do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn oxv_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// The `OXV_ABI_VERSION` the library was built with.
#[unsafe(no_mangle)]
pub extern "C" fn oxv_abi_version() -> u32 {
    OXV_ABI_VERSION
}

/// A static, NUL-terminated English description of `status`. Takes any
/// integer: unknown values return `"unknown status"`. Never NULL; do not
/// free it.
#[unsafe(no_mangle)]
pub extern "C" fn oxv_status_message(status: i32) -> *const c_char {
    let msg: &'static std::ffi::CStr = match status {
        0 => c"ok",
        1 => c"invalid argument",
        2 => c"unsupported sample rate, channel count or bitrate",
        3 => c"write callback failed",
        4 => c"invalid state: already finished or failed",
        5 => c"internal error (panic) in oxivorbis",
        _ => c"unknown status",
    };
    msg.as_ptr()
}

/// Run `f` and collapse its result into a status, turning a panic into
/// `OXV_STATUS_PANIC`.
fn guard(f: impl FnOnce() -> Result<(), oxv_status>) -> oxv_status {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => oxv_status::Ok,
        Ok(Err(s)) => s,
        Err(_) => oxv_status::Panic,
    }
}

/// Run `f`, turning a panic into `None`.
fn catch<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// `Err(InvalidArgument)` if `p` is NULL.
fn non_null<T>(p: *const T) -> Result<(), oxv_status> {
    if p.is_null() {
        Err(oxv_status::InvalidArgument)
    } else {
        Ok(())
    }
}

/// Borrow `len` elements at `ptr` as a slice.
///
/// `len == 0` gives an empty slice whatever `ptr` is (NULL allowed).
/// Otherwise `ptr` must be non-NULL and aligned, and `len` elements must
/// fit in `isize::MAX` bytes; anything else is `InvalidArgument`.
///
/// # Safety
///
/// If `len > 0`, `ptr` must point to `len` initialized `T`s that stay valid
/// and unmodified for `'a`.
unsafe fn slice_arg<'a, T>(ptr: *const T, len: usize) -> Result<&'a [T], oxv_status> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null()
        || !ptr.is_aligned()
        || len > isize::MAX as usize / std::mem::size_of::<T>().max(1)
    {
        return Err(oxv_status::InvalidArgument);
    }
    // SAFETY: `ptr` is non-NULL and aligned and the byte length fits in
    // `isize` (checked above); the caller guarantees `len` initialized
    // elements valid for `'a`.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Read a NUL-terminated UTF-8 string argument.
///
/// # Safety
///
/// `ptr` must be NULL or point to a NUL-terminated string valid for `'a`.
unsafe fn str_arg<'a>(ptr: *const c_char) -> Result<&'a str, oxv_status> {
    non_null(ptr)?;
    // SAFETY: non-NULL (checked) and NUL-terminated per the caller.
    let c = unsafe { std::ffi::CStr::from_ptr(ptr) };
    c.to_str().map_err(|_| oxv_status::InvalidArgument)
}

/// Write `value` through an out-pointer if it is non-NULL.
///
/// # Safety
///
/// `out` must be NULL or valid for a write of `T`.
unsafe fn store<T>(out: *mut T, value: T) {
    if !out.is_null() {
        // SAFETY: non-NULL, and valid for writes per the caller. `write`
        // does not drop the old (possibly uninitialized) value.
        unsafe { out.write(value) };
    }
}

/// Validate a rate / channel count pair from C.
fn rate_channels(
    rate: u32,
    channels: u32,
) -> Result<(vorbis::SampleRate, vorbis::Channels), oxv_status> {
    let rate = vorbis::SampleRate::new(rate).ok_or(oxv_status::Unsupported)?;
    let channels = u16::try_from(channels)
        .ok()
        .and_then(|n| vorbis::Channels::try_from(n).ok())
        .ok_or(oxv_status::Unsupported)?;
    Ok((rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_turns_panics_into_status() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let s = guard(|| panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(s, oxv_status::Panic);
    }

    #[test]
    fn every_status_has_a_message() {
        for s in 0..=5 {
            // SAFETY: oxv_status_message returns a static C string.
            let m = unsafe { std::ffi::CStr::from_ptr(oxv_status_message(s)) };
            assert_ne!(m.to_str().unwrap(), "unknown status", "{s}");
        }
        // SAFETY: as above.
        let m = unsafe { std::ffi::CStr::from_ptr(oxv_status_message(-7)) };
        assert_eq!(m.to_str().unwrap(), "unknown status");
    }

    #[test]
    fn version_matches_crate() {
        // SAFETY: oxv_version returns a static C string.
        let v = unsafe { std::ffi::CStr::from_ptr(oxv_version()) };
        assert_eq!(v.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
        assert_eq!(oxv_abi_version(), OXV_ABI_VERSION);
    }

    #[test]
    fn slice_arg_rejects_bad_pointers() {
        let buf = [0i16; 4];
        // A misaligned i16 pointer (never dereferenced).
        let misaligned = buf.as_ptr().cast::<u8>().wrapping_add(1).cast::<i16>();
        // SAFETY: slice_arg rejects these before forming a slice.
        unsafe {
            assert_eq!(slice_arg(misaligned, 2), Err(oxv_status::InvalidArgument));
        }
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                slice_arg(std::ptr::null::<i16>(), 1),
                Err(oxv_status::InvalidArgument)
            );
        }
        // SAFETY: as above (byte length would exceed isize::MAX).
        unsafe {
            assert_eq!(
                slice_arg(buf.as_ptr(), usize::MAX / 2),
                Err(oxv_status::InvalidArgument)
            );
        }
        // SAFETY: len 0 never reads; NULL and misaligned are allowed.
        unsafe {
            assert_eq!(slice_arg(std::ptr::null::<i16>(), 0), Ok(&[][..]));
        }
        // SAFETY: as above.
        unsafe {
            assert_eq!(slice_arg(misaligned, 0), Ok(&[][..]));
        }
        // SAFETY: `buf` holds 4 values.
        unsafe {
            assert_eq!(slice_arg(buf.as_ptr(), 4), Ok(&buf[..]));
        }
    }

    #[test]
    fn rate_limits_match_library() {
        assert_eq!(OXV_SAMPLE_RATE_MIN, vorbis::SampleRate::MIN.hz());
        assert_eq!(OXV_SAMPLE_RATE_MAX, vorbis::SampleRate::MAX.hz());
        assert_eq!(OXV_CHANNELS_MAX, u32::from(vorbis::Channels::MAX.count()));
    }
}
