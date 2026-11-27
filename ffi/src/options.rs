//! `oxv_options`: an opaque, growable set of encoder settings.

use crate::{guard, non_null, oxv_status, rate_channels, store, str_arg};
use std::ffi::c_char;
use vorbis::{Bitrate, BitrateError, BitrateMode, BitrateTuning, Channels, Encoder, SampleRate};

/// Encoder settings (opaque). Create with `oxv_options_new`, change with
/// the `oxv_options_set_*` / `oxv_options_add_*` functions, pass to
/// `oxv_encoder_new`, `oxv_writer_new` or `oxv_encode_*`, and free with
/// `oxv_options_free`.
///
/// Fresh options are oxivorbis's defaults: quality 5 VBR, the fixed
/// default serial (so output is deterministic), vendor
/// `"oxivorbis <version>"` and no comments. Every function that takes
/// options copies what it needs, so the options may be changed or freed
/// right after the call. Passing NULL options means the defaults.
///
/// Options are plain data: they may be used from any thread, but not
/// changed on one thread while another uses them.
#[derive(Default)]
pub struct oxv_options {
    pub(crate) inner: vorbis::Options,
    /// `oxv_options_set_bitrate`; `None` is VBR at the quality.
    bitrate: Option<BitrateMode>,
    /// `oxv_options_set_bitrate_tuning`.
    tuning: BitrateTuning,
}

impl oxv_options {
    /// The bitrate request, if a bitrate mode is set.
    fn bitrate(&self) -> Option<Bitrate> {
        self.bitrate
            .map(|m| Bitrate::new(m).with_tuning(self.tuning))
    }

    /// Why a bitrate mode can't be encoded at `rate` / `channels`, if it
    /// can't.
    fn bitrate_error(&self, rate: SampleRate, ch: Channels) -> Option<BitrateError> {
        let b = self.bitrate()?;
        Encoder::with_bitrate(rate, ch, &self.inner, &b).err()
    }

    /// One-shot encode of `pcm`: `vbr` (`oxivorbis::encode` or
    /// `encode_f32`) without a bitrate mode, else `managed` with it
    /// (`OXV_STATUS_UNSUPPORTED` if libvorbis can't encode it here).
    pub(crate) fn encode<T>(
        &self,
        rate: SampleRate,
        ch: Channels,
        vbr: fn(&[T], SampleRate, Channels, &vorbis::Options) -> Vec<u8>,
        managed: impl FnOnce(&Bitrate) -> Result<Vec<u8>, BitrateError>,
        pcm: &[T],
    ) -> Result<Vec<u8>, oxv_status> {
        match self.bitrate() {
            None => Ok(vbr(pcm, rate, ch, &self.inner)),
            Some(b) => managed(&b).map_err(|_| oxv_status::Unsupported),
        }
    }

    /// A streaming encoder: VBR, or the bitrate mode, which is checked for
    /// `rate` / `channels` here (`OXV_STATUS_UNSUPPORTED` if libvorbis
    /// can't encode it there).
    pub(crate) fn encoder(&self, rate: SampleRate, ch: Channels) -> Result<Encoder, oxv_status> {
        match self.bitrate() {
            None => Ok(Encoder::new(rate, ch, &self.inner)),
            Some(b) => Encoder::with_bitrate(rate, ch, &self.inner, &b)
                .map_err(|_| oxv_status::Unsupported),
        }
    }
}

/// Borrow options from C: NULL means the defaults.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options` not being changed meanwhile.
pub(crate) unsafe fn options_or_default(
    opts: *const oxv_options,
    default: &oxv_options,
) -> &oxv_options {
    if opts.is_null() {
        default
    } else {
        // SAFETY: non-NULL, and a live, unaliased-for-writes oxv_options
        // per the caller.
        unsafe { &*opts }
    }
}

/// Mutably borrow options from C.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options` not used elsewhere meanwhile.
unsafe fn handle_mut<'a>(opts: *mut oxv_options) -> Result<&'a mut oxv_options, oxv_status> {
    non_null(opts)?;
    // SAFETY: non-NULL (checked); live and exclusively ours per the caller.
    Ok(unsafe { &mut *opts })
}

/// Mutably borrow the library options from C.
///
/// # Safety
///
/// As [`handle_mut`].
unsafe fn options_mut<'a>(opts: *mut oxv_options) -> Result<&'a mut vorbis::Options, oxv_status> {
    // SAFETY: forwarded from this function's contract.
    Ok(&mut unsafe { handle_mut(opts) }?.inner)
}

/// New options with the defaults. Free with `oxv_options_free`. Returns
/// NULL only on an internal error.
#[unsafe(no_mangle)]
pub extern "C" fn oxv_options_new() -> *mut oxv_options {
    crate::catch(|| Box::into_raw(Box::<oxv_options>::default())).unwrap_or(std::ptr::null_mut())
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
/// setup. `OXV_BITRATE_MODE_QUALITY_LIMITS` uses it too.
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

/// Rate control for `oxv_options_set_bitrate`: VBR at a quality (the
/// default) or one of libvorbis's bitrate-driven setups, each as oggenc
/// 1.4.3 sets it up (the output is byte-identical to libvorbis configured
/// by those oggenc flags). Bitrates are in kb/s (1000 bits) for the whole
/// stream.
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum oxv_bitrate_mode {
    /// VBR at the quality (`oggenc -q`); the default. No bitrates.
    Vbr = 0,
    /// `oggenc -b KBPS`: unmanaged VBR with the setup for about `kbps`
    /// (no reservoir). No limits.
    Nominal = 1,
    /// `oggenc --managed -b KBPS [-m MIN] [-M MAX]`: average bitrate
    /// through libvorbis's bit reservoir, optionally with hard limits
    /// (`min = kbps = max` is libvorbis's "CBR").
    Average = 2,
    /// `oggenc --managed -m MIN -M MAX`: hard limits only (at least one),
    /// with the setup for their midpoint. `kbps` must be 0.
    Limits = 3,
    /// `oggenc -q Q -m MIN -M MAX`: VBR at the quality with hard limits
    /// (at least one). `kbps` must be 0.
    QualityLimits = 4,
}

/// Choose the rate control (an `oxv_bitrate_mode` value). `kbps` is the
/// (average) bitrate of `OXV_BITRATE_MODE_NOMINAL` and
/// `OXV_BITRATE_MODE_AVERAGE`; `min_kbps` / `max_kbps` are the hard limits
/// of the managed modes, 0 for none. Arguments a mode doesn't take must be
/// 0. `OXV_BITRATE_MODE_VBR` (with all zeros) goes back to VBR.
///
/// Whether libvorbis can encode the request depends on the sample rate and
/// channel count, so it is checked when an encoder, writer or one-shot
/// encode is created with these options: those then return
/// `OXV_STATUS_UNSUPPORTED`. libvorbis has bitrate setups from 8 to 50 kHz
/// (for 6 channels, up to 70 kHz), each for a range of bitrates
/// (`oxv_bitrate_supported_kbps`); the request must also be consistent
/// (minimum at most the average and the maximum, average at most the
/// maximum, bitrates at most 2147483 kb/s). `oxv_options_check` tells
/// whether options can be used at a rate and channel count, and why not.
///
/// Returns `OXV_STATUS_INVALID_ARGUMENT` (and leaves the options unchanged)
/// for NULL `opts`, an unknown mode, or a non-zero argument the mode
/// doesn't take.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_set_bitrate(
    opts: *mut oxv_options,
    mode: i32,
    kbps: u32,
    min_kbps: u32,
    max_kbps: u32,
) -> oxv_status {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { handle_mut(opts) }?;
        let limit = |k: u32| (k != 0).then_some(k);
        let (min, max) = (limit(min_kbps), limit(max_kbps));
        let no_limits = min.is_none() && max.is_none();
        let bad = Err(oxv_status::InvalidArgument);
        o.bitrate = match mode {
            m if m == oxv_bitrate_mode::Vbr as i32 => {
                if kbps != 0 || !no_limits {
                    return bad;
                }
                None
            }
            m if m == oxv_bitrate_mode::Nominal as i32 => {
                if !no_limits {
                    return bad;
                }
                Some(BitrateMode::Nominal { kbps })
            }
            m if m == oxv_bitrate_mode::Average as i32 => Some(BitrateMode::Average {
                kbps,
                min_kbps: min,
                max_kbps: max,
            }),
            m if m == oxv_bitrate_mode::Limits as i32 => {
                if kbps != 0 {
                    return bad;
                }
                Some(BitrateMode::Limits {
                    min_kbps: min,
                    max_kbps: max,
                })
            }
            m if m == oxv_bitrate_mode::QualityLimits as i32 => {
                if kbps != 0 {
                    return bad;
                }
                Some(BitrateMode::QualityLimits {
                    min_kbps: min,
                    max_kbps: max,
                })
            }
            _ => return bad,
        };
        Ok(())
    })
}

/// Tune libvorbis's bit reservoir for the managed bitrate modes, as
/// oggenc's `--advanced-encode-option`s `bit_reservoir_bits`,
/// `bit_reservoir_bias` and `bitrate_average_damping` do. A negative
/// `reservoir_bits` or a NaN `reservoir_bias` / `average_damping` keeps
/// libvorbis's default (two seconds of the nominal bitrate, 0.1 and 1.5 s);
/// `(-1, NAN, NAN)` resets all three. `reservoir_bits` 0 turns management
/// off while keeping the managed setup. Has no effect in VBR.
///
/// The bias must be within 0.0..=1.0 and the damping positive; like the
/// bitrates, that is checked when an encoder is created
/// (`OXV_STATUS_UNSUPPORTED`).
///
/// Returns `OXV_STATUS_INVALID_ARGUMENT` for NULL `opts` or
/// `reservoir_bits` above `UINT32_MAX`.
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_set_bitrate_tuning(
    opts: *mut oxv_options,
    reservoir_bits: i64,
    reservoir_bias: f64,
    average_damping: f64,
) -> oxv_status {
    guard(|| {
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { handle_mut(opts) }?;
        let mut t = BitrateTuning::default();
        if reservoir_bits >= 0 {
            let bits = u32::try_from(reservoir_bits).map_err(|_| oxv_status::InvalidArgument)?;
            t = t.with_reservoir_bits(bits);
        }
        if !reservoir_bias.is_nan() {
            t = t.with_reservoir_bias(reservoir_bias);
        }
        if !average_damping.is_nan() {
            t = t.with_average_damping(average_damping);
        }
        o.tuning = t;
        Ok(())
    })
}

/// The average bitrates, in kb/s, that libvorbis 1.3.7 has an encoder setup
/// for at `rate` Hz and `channels` channels: `*min_kbps..=*max_kbps` (e.g.
/// 45..=500 for 44.1 kHz stereo, 84..=1440 for 5.1 at 48 kHz). They bound
/// the `kbps` of `OXV_BITRATE_MODE_NOMINAL` / `_AVERAGE` and the midpoint
/// of `_LIMITS`.
///
/// Returns `OXV_STATUS_UNSUPPORTED` (with both outputs 0) if the rate or
/// channel count is unsupported or libvorbis has no bitrate setups there,
/// `OXV_STATUS_INVALID_ARGUMENT` if an output pointer is NULL.
///
/// # Safety
///
/// `min_kbps` / `max_kbps` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_bitrate_supported_kbps(
    rate: u32,
    channels: u32,
    min_kbps: *mut u32,
    max_kbps: *mut u32,
) -> oxv_status {
    // SAFETY: NULL or writable per the contract.
    unsafe { store(min_kbps, 0) };
    // SAFETY: as above.
    unsafe { store(max_kbps, 0) };
    guard(|| {
        non_null(min_kbps)?;
        non_null(max_kbps)?;
        let (rate, ch) = rate_channels(rate, channels)?;
        let r = BitrateMode::supported_kbps(rate, ch).ok_or(oxv_status::Unsupported)?;
        // SAFETY: non-NULL (checked) and writable per the contract.
        unsafe { min_kbps.write(*r.start()) };
        // SAFETY: as above.
        unsafe { max_kbps.write(*r.end()) };
        Ok(())
    })
}

/// Check that `opts` (NULL = defaults) can encode `rate` Hz with
/// `channels` channels, i.e. that `oxv_encoder_new` and the other
/// constructors would succeed. Returns `OXV_STATUS_OK`, or
/// `OXV_STATUS_UNSUPPORTED` for an unsupported rate or channel count or a
/// bitrate mode libvorbis can't encode there.
///
/// If `message` is non-NULL and `message_size` > 0, an English description
/// of the problem (empty on success) is written there, NUL-terminated and
/// truncated to `message_size - 1` bytes, e.g. "no libvorbis setup for
/// 600000 b/s at 44100 Hz stereo (supported: 45..=500 kb/s)".
///
/// # Safety
///
/// `opts` must be NULL or a live `oxv_options`; `message` must be NULL or
/// point to `message_size` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oxv_options_check(
    opts: *const oxv_options,
    rate: u32,
    channels: u32,
    message: *mut c_char,
    message_size: usize,
) -> oxv_status {
    let mut text = String::new();
    let status = guard(|| {
        let default = oxv_options::default();
        // SAFETY: forwarded from this function's contract.
        let o = unsafe { options_or_default(opts, &default) };
        let (rate, ch) = rate_channels(rate, channels).inspect_err(|_| {
            text = format!(
                "unsupported sample rate {rate} Hz or channel count {channels} \
                 (1..=200000 Hz, 1..=255 channels)"
            );
        })?;
        if let Some(e) = o.bitrate_error(rate, ch) {
            text = e.to_string();
            return Err(oxv_status::Unsupported);
        }
        Ok(())
    });
    if !message.is_null() && message_size > 0 {
        let n = text.len().min(message_size - 1);
        // SAFETY: `message` has `message_size` writable bytes per the
        // contract, and `n < message_size`.
        let out = unsafe { std::slice::from_raw_parts_mut(message.cast::<u8>(), n + 1) };
        out[..n].copy_from_slice(&text.as_bytes()[..n]);
        out[n] = 0;
    }
    status
}
