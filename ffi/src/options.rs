//! `oxv_options`: an opaque, growable set of encoder settings.

use crate::{guard, non_null, oxv_status, str_arg};
use std::ffi::c_char;

/// Encoder settings (opaque). Create with `oxv_options_new`, change with
/// the `oxv_options_set_*` / `oxv_options_add_*` functions, pass to
/// `oxv_encoder_new`, `oxv_writer_new` or `oxv_encode_*`, and free with
/// `oxv_options_free`.
///
/// Fresh options are oxivorbis's defaults: quality 5, the fixed default
/// serial (so output is deterministic), vendor `"oxivorbis <version>"` and
/// no comments. Every function that takes options copies what it needs, so
/// the options may be changed or freed right after the call. Passing NULL
/// options means the defaults.
///
/// Options are plain data: they may be used from any thread, but not
/// changed on one thread while another uses them.
pub struct oxv_options {
    pub(crate) inner: vorbis::Options,
}

/// Borrow options from C: NULL means the defaults.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options` not being changed meanwhile.
pub(crate) unsafe fn options_or_default(
    opts: *const oxv_options,
    default: &vorbis::Options,
) -> &vorbis::Options {
    if opts.is_null() {
        default
    } else {
        // SAFETY: non-NULL, and a live, unaliased-for-writes oxv_options
        // per the caller.
        unsafe { &(*opts).inner }
    }
}

/// Mutably borrow options from C.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options` not used elsewhere meanwhile.
unsafe fn options_mut<'a>(opts: *mut oxv_options) -> Result<&'a mut vorbis::Options, oxv_status> {
    non_null(opts)?;
    // SAFETY: non-NULL (checked); live and exclusively ours per the caller.
    Ok(unsafe { &mut (*opts).inner })
}

/// New options with the defaults. Free with `oxv_options_free`. Returns
/// NULL only on an internal error.
#[unsafe(no_mangle)]
pub extern "C" fn oxv_options_new() -> *mut oxv_options {
    crate::catch(|| {
        Box::into_raw(Box::new(oxv_options {
            inner: vorbis::Options::default(),
        }))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Free options from `oxv_options_new`. NULL is a no-op.
///
/// # Safety
///
/// `opts` must be NULL or a pointer from `oxv_options_new` not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_free(opts: *mut oxv_options) {
    if !opts.is_null() {
        // SAFETY: from Box::into_raw in oxv_options_new and not yet freed,
        // per the caller. Dropping a Vec/String can't panic.
        drop(unsafe { Box::from_raw(opts) });
    }
}

/// Set the quality on oggenc's `-q` scale: -1.0 to 10.0, fractional values
/// allowed (default 5). It is converted to libvorbis's `base_quality`
/// exactly as oggenc does, so `-q 7.5` here and in oggenc pick the same
/// setup.
///
/// Returns `OXV_STATUS_INVALID_ARGUMENT` (and leaves the options unchanged)
/// for NULL `opts`, NaN, or a value outside -1.0..=10.0.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_set_quality(
    opts: *mut oxv_options,
    quality: f32,
) -> oxv_status {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { options_mut(opts) }?;
        o.quality = vorbis::Quality::new(quality).ok_or(oxv_status::InvalidArgument)?;
        Ok(())
    })
}

/// Set the Ogg stream serial number. Streams that are chained or
/// multiplexed together need distinct serials.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_set_serial(opts: *mut oxv_options, serial: u32) -> oxv_status {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { options_mut(opts) }?;
        o.serial = Some(serial);
        Ok(())
    })
}

/// Set the comment-header vendor string (NUL-terminated UTF-8; copied).
/// Default: `"oxivorbis <version>"`.
///
/// Returns `OXV_STATUS_INVALID_ARGUMENT` for NULL arguments or a string
/// that is not valid UTF-8.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`; `vendor` must be NULL or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_set_vendor(
    opts: *mut oxv_options,
    vendor: *const c_char,
) -> oxv_status {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let vendor = unsafe { str_arg(vendor) }?;
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { options_mut(opts) }?;
        o.vendor = Some(vendor.to_owned());
        Ok(())
    })
}

/// Append the user comment `key=value` (both NUL-terminated UTF-8; copied),
/// e.g. `("TITLE", "Song")`. Comments are written in the order added.
///
/// As in the Rust API, bytes the Vorbis spec doesn't allow in a key
/// (anything outside ASCII 0x20..=0x7D, and `=`) are dropped from it, and
/// the comment is skipped if no key byte is left. The value is written as
/// given.
///
/// Returns `OXV_STATUS_INVALID_ARGUMENT` for NULL arguments or strings that
/// are not valid UTF-8.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`; `key` and `value` must be
/// NULL or NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_add_comment(
    opts: *mut oxv_options,
    key: *const c_char,
    value: *const c_char,
) -> oxv_status {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let key = unsafe { str_arg(key) }?;
        // SAFETY: forwarded from this function's contract.
        let value = unsafe { str_arg(value) }?;
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { options_mut(opts) }?;
        o.comments.push((key.to_owned(), value.to_owned()));
        Ok(())
    })
}
