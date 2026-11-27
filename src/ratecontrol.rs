//! Bitrate-driven encoding modes: the public request types and their
//! translation into libvorbis 1.3.7's high-level encoder setup.
//!
//! Each [`BitrateMode`] is one of the ways `oggenc` (vorbis-tools 1.4.3,
//! `oggenc/encode.c`) drives libvorbis, and this module replays those
//! calls on a model of `highlevel_encode_setup`: `vorbis_encode_setup_vbr`
//! / `vorbis_encode_setup_managed`, the `OV_ECTL_RATEMANAGE2_GET/SET`
//! round trips, and the bitrate half of `vorbis_encode_setup_init`. The
//! result, a [`RatePlan`], is what the encoder needs: the template and
//! `base_setting`, whether the setup is managed, the ID-header bitrate
//! fields and the bitrate manager's parameters.

use std::fmt;
use std::ops::RangeInclusive;

use crate::bitrate::BitrateInfo;
use crate::tables::quality as t;
use crate::{Channels, Quality, SampleRate};

/// Largest kbps value accepted anywhere: oggenc multiplies its `int` kbps
/// by 1000 in `int` arithmetic.
const MAX_KBPS: u32 = i32::MAX as u32 / 1000;

/// A bitrate-driven encoding mode, set with [`Options::with_bitrate`].
///
/// Each mode reproduces one way `oggenc` sets up libvorbis, so a stream
/// encoded with a mode is byte-identical to libvorbis 1.3.7 configured by
/// the oggenc flags listed with it. All bitrates are in kilobits per
/// second (1000 bits), as oggenc takes them, for the whole stream (all
/// channels).
///
/// Bitrate management only exists for the libvorbis templates with a
/// bitrate table, i.e. at 8 kHz to 50 kHz; at other rates every mode is
/// rejected. Within that, each rate and channel count supports a range of
/// average bitrates ([`BitrateMode::supported_kbps`]).
///
/// ```
/// use oxivorbis::{BitrateMode, Channels, Options, SampleRate};
///
/// // oggenc --managed -b 128: average bitrate 128 kb/s.
/// let abr = Options::default()
///     .with_bitrate(SampleRate::HZ_44100, Channels::Stereo, BitrateMode::average(128))
///     .expect("128 kb/s is in range at 44.1 kHz stereo");
///
/// // oggenc --managed -b 128 -m 128 -M 128: "CBR".
/// let cbr = BitrateMode::Average { kbps: 128, min_kbps: Some(128), max_kbps: Some(128) };
/// assert!(Options::default().with_bitrate(SampleRate::HZ_44100, Channels::Stereo, cbr).is_ok());
///
/// // oggenc -q 6 -M 192: quality 6 with a hard maximum.
/// let capped = Options::default()
///     .with_quality(oxivorbis::Quality::new(6.0).unwrap())
///     .with_bitrate(
///         SampleRate::HZ_48000,
///         Channels::Stereo,
///         BitrateMode::QualityLimits { min_kbps: None, max_kbps: Some(192) },
///     )
///     .unwrap();
/// # let _ = (abr, capped);
/// ```
///
/// [`Options::with_bitrate`]: crate::Options::with_bitrate
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BitrateMode {
    /// `oggenc -b KBPS`: unmanaged VBR, with the encoder setup libvorbis
    /// picks for an average of `kbps` (`vorbis_encode_setup_managed`, then
    /// management switched off). There is no reservoir, so the actual
    /// bitrate follows the signal like any quality-mode encode; the ID
    /// header announces `kbps` as nominal.
    Nominal {
        /// Target average bitrate.
        kbps: u32,
    },
    /// `oggenc --managed -b KBPS [-m MIN] [-M MAX]`
    /// (`vorbis_encode_init(max, kbps, min)`): average bitrate management
    /// (ABR) through libvorbis's bit reservoir, optionally with hard
    /// minimum and maximum limits. With `min = avg = max` this is
    /// libvorbis's closest to constant bitrate.
    Average {
        /// Average bitrate.
        kbps: u32,
        /// Hard minimum, if any (`-m`); at most `kbps`.
        min_kbps: Option<u32>,
        /// Hard maximum, if any (`-M`); at least `kbps`.
        max_kbps: Option<u32>,
    },
    /// `oggenc -m MIN -M MAX` without `-b` or `-q`: hard limits only. The
    /// setup is the one for the midpoint of the limits (or 87.5 % of the
    /// maximum, or the minimum, if only one is given), and no average is
    /// tracked. At least one limit must be given.
    Limits {
        /// Hard minimum (`-m`).
        min_kbps: Option<u32>,
        /// Hard maximum (`-M`).
        max_kbps: Option<u32>,
    },
    /// `oggenc -q Q -m MIN -M MAX`: quality mode at
    /// [`Options::quality`](crate::Options::quality) with hard limits
    /// (`vorbis_encode_setup_vbr`, then `OV_ECTL_RATEMANAGE2_SET`). At
    /// least one limit must be given.
    ///
    /// Unlike oggenc, the limits need not bracket the quality's nominal
    /// bitrate: `oggenc -q 6 -M 128` fails because oggenc first passes the
    /// nominal bitrate as the average, but the setup libvorbis ends with
    /// (no average, the given limits) is the same, and it is accepted here.
    QualityLimits {
        /// Hard minimum (`-m`).
        min_kbps: Option<u32>,
        /// Hard maximum (`-M`).
        max_kbps: Option<u32>,
    },
}

impl BitrateMode {
    /// [`BitrateMode::Average`] without hard limits (`oggenc --managed -b kbps`).
    pub const fn average(kbps: u32) -> Self {
        BitrateMode::Average {
            kbps,
            min_kbps: None,
            max_kbps: None,
        }
    }

    /// The average bitrates (in whole kb/s) that libvorbis 1.3.7 has an
    /// encoder setup for at `rate` / `channels`, or `None` if it has none
    /// (rates below 8 kHz or above 50 kHz). This bounds
    /// [`Nominal`](Self::Nominal) and [`Average`](Self::Average)'s `kbps`,
    /// and the derived midpoint of [`Limits`](Self::Limits).
    ///
    /// ```
    /// use oxivorbis::{BitrateMode, Channels, SampleRate};
    ///
    /// assert_eq!(BitrateMode::supported_kbps(SampleRate::HZ_44100, Channels::Stereo), Some(45..=500));
    /// assert_eq!(BitrateMode::supported_kbps(SampleRate::HZ_96000, Channels::Stereo), None);
    /// ```
    pub fn supported_kbps(rate: SampleRate, channels: Channels) -> Option<RangeInclusive<u32>> {
        let ch = channels.count() as i64;
        let ok = |k: u32| {
            crate::quality::select_template_bitrate((k as i64 * 1000) as f64, rate.hz(), ch)
                .is_some()
        };
        // Every template tops out below 300 kb/s per channel.
        let top = 300 * ch as u32;
        let lo = (1..=top).find(|&k| ok(k))?;
        let hi = (lo..=top).rev().find(|&k| ok(k))?;
        Some(lo..=hi)
    }
}

/// Fine-tuning of libvorbis's bit reservoir, as oggenc's
/// `--advanced-encode-option bit_reservoir_bits=`, `bit_reservoir_bias=`
/// and `bitrate_average_damping=` set it (fields of
/// `ovectl_ratemanage2_arg`). `None` keeps libvorbis's default.
///
/// Applying any tuning makes libvorbis round-trip the limits through
/// whole kb/s, as oggenc does; an unset limit then reads back as 0
/// instead of -1 in the ID header.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct BitrateTuning {
    /// Reservoir size in bits (`bit_reservoir_bits`; default twice the
    /// nominal bitrate, i.e. two seconds). 0 turns management off while
    /// keeping the managed setup.
    pub reservoir_bits: Option<u32>,
    /// Preferred reservoir fill, 0.0..=1.0 (`bit_reservoir_bias`;
    /// default 0.1). Lower values bank bits against future peaks.
    pub reservoir_bias: Option<f64>,
    /// Minimum time in seconds for the average tracker to swing from one
    /// extreme to the other (`bitrate_average_damping`; default 1.5).
    /// Must be positive.
    pub average_damping: Option<f64>,
}

impl BitrateTuning {
    /// Set [`reservoir_bits`](Self::reservoir_bits).
    pub fn with_reservoir_bits(mut self, bits: u32) -> Self {
        self.reservoir_bits = Some(bits);
        self
    }

    /// Set [`reservoir_bias`](Self::reservoir_bias).
    pub fn with_reservoir_bias(mut self, bias: f64) -> Self {
        self.reservoir_bias = Some(bias);
        self
    }

    /// Set [`average_damping`](Self::average_damping).
    pub fn with_average_damping(mut self, seconds: f64) -> Self {
        self.average_damping = Some(seconds);
        self
    }

    fn is_set(&self) -> bool {
        self.reservoir_bits.is_some()
            || self.reservoir_bias.is_some()
            || self.average_damping.is_some()
    }
}

/// libvorbis's other encoder controls, as oggenc's
/// `--advanced-encode-option lowpass_frequency=`, `impulse_noisetune=` and
/// `disable_coupling` set them (`OV_ECTL_LOWPASS_SET`,
/// `OV_ECTL_IBLOCK_SET`, `OV_ECTL_COUPLING_SET`). Set with
/// [`Options::with_encoder_tuning`]; they apply to VBR and the bitrate
/// modes alike, except where noted. The default changes nothing.
///
/// Values are clamped as libvorbis clamps them. NaN, which libvorbis would
/// carry into undefined float-to-int conversions, is ignored (the setting
/// keeps its default).
///
/// [`Options::with_encoder_tuning`]: crate::Options::with_encoder_tuning
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct EncoderTuning {
    /// Lowpass frequency in kHz (`lowpass_frequency`), clamped to
    /// 2.0..=99.0. `None` keeps the setup's own lowpass.
    pub lowpass_khz: Option<f64>,
    /// Extra noise-curve bias in dB for impulse (short, transient) blocks
    /// (`impulse_noisetune`), clamped to -15.0..=0.0; more negative
    /// spends more bits on transients.
    pub impulse_noisetune: Option<f64>,
    /// Encode stereo without channel coupling (`disable_coupling`), with
    /// libvorbis's uncoupled template. VBR only: libvorbis finds no
    /// uncoupled template for a bitrate request, so in the bitrate modes
    /// this has no effect, as with oggenc.
    pub disable_coupling: bool,
}

impl EncoderTuning {
    /// Set [`lowpass_khz`](Self::lowpass_khz).
    pub fn with_lowpass_khz(mut self, khz: f64) -> Self {
        self.lowpass_khz = Some(khz);
        self
    }

    /// Set [`impulse_noisetune`](Self::impulse_noisetune).
    pub fn with_impulse_noisetune(mut self, db: f64) -> Self {
        self.impulse_noisetune = Some(db);
        self
    }

    /// Set [`disable_coupling`](Self::disable_coupling).
    pub fn with_coupling_disabled(mut self) -> Self {
        self.disable_coupling = true;
        self
    }
}

/// Why a [`BitrateMode`] can't be used.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum BitrateError {
    /// libvorbis has no encoder setup for this bitrate at this rate and
    /// channel count (its `OV_EIMPL`). `supported` is the range
    /// [`BitrateMode::supported_kbps`] reports, `None` when the rate has
    /// no bitrate-managed setups at all.
    OutOfRange {
        /// The nominal bitrate libvorbis looked up, in bits per second.
        bps: u32,
        /// The sample rate.
        rate: SampleRate,
        /// The channel layout.
        channels: Channels,
        /// The supported average bitrates in kb/s, if any.
        supported: Option<RangeInclusive<u32>>,
    },
    /// The request itself is inconsistent (libvorbis's `OV_EINVAL`):
    /// minimum above maximum or above the average, maximum below the
    /// average, a bitrate of 0 or above 2147483 kb/s, a limits mode with
    /// no limit, or a tuning value out of range.
    Invalid(&'static str),
}

impl fmt::Display for BitrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitrateError::OutOfRange {
                bps,
                rate,
                channels,
                supported,
            } => {
                write!(
                    f,
                    "no libvorbis setup for {bps} b/s at {rate} {}",
                    match channels.count() {
                        1 => "mono",
                        _ => "stereo",
                    }
                )?;
                match supported {
                    Some(r) => write!(f, " (supported: {}..={} kb/s)", r.start(), r.end()),
                    None => write!(f, " (bitrate modes need 8000..=50000 Hz)"),
                }
            }
            BitrateError::Invalid(why) => write!(f, "invalid bitrate request: {why}"),
        }
    }
}

impl std::error::Error for BitrateError {}

/// A validated bitrate request, kept in `Options`.
#[derive(Clone, Debug)]
pub(crate) struct BitrateRequest {
    pub mode: BitrateMode,
    pub tuning: BitrateTuning,
    /// The rate and channels it was validated for.
    pub rate: SampleRate,
    pub channels: Channels,
}

/// What the encoder needs from the high-level setup.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RatePlan {
    /// Index into `SETUP_LIST`, and `hi->base_setting`.
    pub template: usize,
    pub base_setting: f64,
    /// `hi->managed` when `vorbis_encode_setup_init` runs: the managed
    /// stereo setup and residue books.
    pub managed_setup: bool,
    /// ID header `[bitrate_upper, bitrate_nominal, bitrate_lower]`.
    pub header_bitrates: [i32; 3],
    /// `ci->bi` (only filled in for managed setups).
    pub bitrate_info: Option<BitrateInfo>,
    /// `hi->impulse_noisetune` (`OV_ECTL_IBLOCK_SET`; 0 by default).
    pub impulse_noisetune: f64,
    /// `hi->lowpass_kHz` if `OV_ECTL_LOWPASS_SET` altered it.
    pub lowpass_khz: Option<f64>,
}

/// `highlevel_encode_setup`, the fields the bitrate paths touch.
#[derive(Clone, Copy, Debug)]
struct Hi {
    template: usize,
    base_setting: f64,
    managed: bool,
    bitrate_min: i64,
    bitrate_av: i64,
    bitrate_av_damp: f64,
    bitrate_max: i64,
    bitrate_reservoir: i64,
    bitrate_reservoir_bias: f64,
}

/// `struct ovectl_ratemanage2_arg`.
#[derive(Clone, Copy, Debug)]
struct RateManage2 {
    management_active: bool,
    bitrate_limit_min_kbps: i64,
    bitrate_limit_max_kbps: i64,
    bitrate_limit_reservoir_bits: i64,
    bitrate_limit_reservoir_bias: f64,
    bitrate_average_kbps: i64,
    bitrate_average_damping: f64,
}

/// libvorbis's `OV_EINVAL` from `OV_ECTL_RATEMANAGE2_SET`.
#[derive(Debug)]
struct EInval;

impl Hi {
    /// `vorbis_encode_setup_vbr` (on a fresh `vorbis_info`).
    fn setup_vbr(base_quality: f32, rate: u32, channels: usize) -> Self {
        let (template, base_setting) =
            crate::quality::select_template(base_quality, rate, channels)
                .expect("every Quality has a template at every SampleRate");
        Hi {
            template,
            base_setting,
            managed: false,
            bitrate_min: 0,
            bitrate_av: 0,
            bitrate_av_damp: 0.0,
            bitrate_max: 0,
            bitrate_reservoir: 0,
            bitrate_reservoir_bias: 0.0,
        }
    }

    /// `vorbis_encode_setup_managed`; `None` is `OV_EIMPL` (the caller has
    /// ruled out `OV_EINVAL`), `Err(nominal)` carries the looked-up bitrate.
    fn setup_managed(
        rate: u32,
        channels: usize,
        max_bitrate: i64,
        nominal_bitrate: i64,
        min_bitrate: i64,
    ) -> Result<Self, i64> {
        let tnominal = nominal_bitrate;
        let mut nominal_bitrate = nominal_bitrate;
        if nominal_bitrate <= 0 {
            if max_bitrate > 0 {
                nominal_bitrate = if min_bitrate > 0 {
                    ((max_bitrate + min_bitrate) as f64 * 0.5) as i64
                } else {
                    (max_bitrate as f64 * 0.875) as i64
                };
            } else {
                debug_assert!(min_bitrate > 0, "validated: some bitrate is given");
                nominal_bitrate = min_bitrate;
            }
        }
        let (template, base_setting) =
            crate::quality::select_template_bitrate(nominal_bitrate as f64, rate, channels as i64)
                .ok_or(nominal_bitrate)?;
        Ok(Hi {
            template,
            base_setting,
            managed: true,
            bitrate_min: min_bitrate,
            bitrate_max: max_bitrate,
            bitrate_av: tnominal,
            // `hi->bitrate_av_damp=1.5f`
            bitrate_av_damp: 1.5_f32 as f64,
            bitrate_reservoir: nominal_bitrate * 2,
            bitrate_reservoir_bias: 0.1,
        })
    }

    /// `OV_ECTL_RATEMANAGE2_GET` (C `long` division truncates).
    fn get2(&self) -> RateManage2 {
        RateManage2 {
            management_active: self.managed,
            bitrate_limit_min_kbps: self.bitrate_min / 1000,
            bitrate_limit_max_kbps: self.bitrate_max / 1000,
            bitrate_average_kbps: self.bitrate_av / 1000,
            bitrate_average_damping: self.bitrate_av_damp,
            bitrate_limit_reservoir_bits: self.bitrate_reservoir,
            bitrate_limit_reservoir_bias: self.bitrate_reservoir_bias,
        }
    }

    /// `OV_ECTL_RATEMANAGE2_SET`; `None` is the NULL argument (management
    /// off).
    fn set2(&mut self, ai: Option<&RateManage2>) -> Result<(), EInval> {
        let Some(ai) = ai else {
            self.managed = false;
            return Ok(());
        };
        let (min, max, avg) = (
            ai.bitrate_limit_min_kbps,
            ai.bitrate_limit_max_kbps,
            ai.bitrate_average_kbps,
        );
        if (min > 0 && avg > 0 && min > avg)
            || (max > 0 && avg > 0 && max < avg)
            || (min > 0 && max > 0 && min > max)
            || ai.bitrate_average_damping <= 0.
            || ai.bitrate_limit_reservoir_bits < 0
            || ai.bitrate_limit_reservoir_bias < 0.
            || ai.bitrate_limit_reservoir_bias > 1.
        {
            return Err(EInval);
        }
        self.managed = ai.management_active;
        self.bitrate_min = min * 1000;
        self.bitrate_max = max * 1000;
        self.bitrate_av = avg * 1000;
        self.bitrate_av_damp = ai.bitrate_average_damping;
        self.bitrate_reservoir = ai.bitrate_limit_reservoir_bits;
        self.bitrate_reservoir_bias = ai.bitrate_limit_reservoir_bias;
        Ok(())
    }

    /// The bitrate half of `vorbis_encode_setup_init`.
    fn finish(&self, channels: usize) -> RatePlan {
        let nominal = if self.bitrate_av > 0 {
            self.bitrate_av
        } else {
            setting_to_approx_bitrate(self.template, self.base_setting, channels)
        };
        RatePlan {
            template: self.template,
            base_setting: self.base_setting,
            managed_setup: self.managed,
            // `long` fields, written to the header as their low 32 bits.
            header_bitrates: [
                self.bitrate_max as i32,
                nominal as i32,
                self.bitrate_min as i32,
            ],
            bitrate_info: self.managed.then_some(BitrateInfo {
                avg_rate: self.bitrate_av,
                min_rate: self.bitrate_min,
                max_rate: self.bitrate_max,
                reservoir_bits: self.bitrate_reservoir,
                reservoir_bias: self.bitrate_reservoir_bias,
                slew_damp: self.bitrate_av_damp,
            }),
            impulse_noisetune: 0.,
            lowpass_khz: None,
        }
    }
}

/// `setting_to_approx_bitrate`, as assigned to the `long`
/// `vi->bitrate_nominal`.
fn setting_to_approx_bitrate(template: usize, base_setting: f64, channels: usize) -> i64 {
    let r = t::SETUP_LIST[template].rate_mapping;
    let is = base_setting as usize;
    let ds = base_setting - is as f64;
    ((r[is] * (1. - ds) + r[is + 1] * ds) * channels as f64) as i64
}

impl RatePlan {
    /// Plain VBR (`vorbis_encode_init_vbr`).
    pub(crate) fn vbr(base_quality: f32, rate: u32, channels: usize) -> Self {
        Hi::setup_vbr(base_quality, rate, channels).finish(channels)
    }

    /// The [`EncoderTuning`] ctls, after the rate setup (as oggenc applies
    /// its advanced options). `vbr_quality` is the request of a plain VBR
    /// setup, the only kind `OV_ECTL_COUPLING_SET` can re-fetch a template
    /// for.
    pub(crate) fn tune(
        mut self,
        t: &EncoderTuning,
        vbr_quality: Option<f32>,
        rate: u32,
        channels: usize,
    ) -> Self {
        if let Some(khz) = t.lowpass_khz.filter(|k| !k.is_nan()) {
            self.lowpass_khz = Some(khz.clamp(2., 99.));
        }
        if let Some(db) = t.impulse_noisetune.filter(|d| !d.is_nan()) {
            self.impulse_noisetune = db.clamp(-15., 0.);
        }
        if let (true, Some(q)) = (t.disable_coupling, vbr_quality) {
            let (template, base_setting) = crate::quality::select_template_uncoupled(q, rate)
                .expect("every rate has an uncoupled template");
            self.template = template;
            self.base_setting = base_setting;
            // VBR's nominal bitrate follows the new setting.
            self.header_bitrates[1] =
                setting_to_approx_bitrate(template, base_setting, channels) as i32;
        }
        self
    }
}

fn check_kbps(k: u32) -> Result<i64, BitrateError> {
    if k == 0 {
        return Err(BitrateError::Invalid("a bitrate of 0 kb/s"));
    }
    if k > MAX_KBPS {
        return Err(BitrateError::Invalid("a bitrate above 2147483 kb/s"));
    }
    Ok(k as i64 * 1000)
}

/// The libvorbis spelling of an optional limit in bits per second.
fn limit_bps(k: Option<u32>, unset: i64) -> Result<i64, BitrateError> {
    k.map_or(Ok(unset), check_kbps)
}

/// The rate-independent checks (`OV_EINVAL`), then the setup.
pub(crate) fn plan(
    mode: &BitrateMode,
    tuning: &BitrateTuning,
    quality: Quality,
    rate: SampleRate,
    channels: Channels,
) -> Result<RatePlan, BitrateError> {
    let ch = channels.count() as usize;
    let hz = rate.hz();
    let out_of_range = |bps: i64| BitrateError::OutOfRange {
        bps: bps.clamp(0, u32::MAX as i64) as u32,
        rate,
        channels,
        supported: BitrateMode::supported_kbps(rate, channels),
    };
    let ordered = |lo: Option<u32>, hi: Option<u32>, what| match (lo, hi) {
        (Some(a), Some(b)) if a > b => Err(BitrateError::Invalid(what)),
        _ => Ok(()),
    };
    if let Some(b) = tuning.reservoir_bias {
        if !(0.0..=1.0).contains(&b) {
            return Err(BitrateError::Invalid("a reservoir bias outside 0.0..=1.0"));
        }
    }
    if let Some(d) = tuning.average_damping {
        if d.is_nan() || d <= 0.0 {
            return Err(BitrateError::Invalid(
                "an average damping that is not positive",
            ));
        }
    }

    let mut hi = match *mode {
        BitrateMode::Nominal { kbps } => {
            // oggenc -b: vorbis_encode_setup_managed(-1, kbps, -1), then
            // OV_ECTL_RATEMANAGE2_SET(NULL) since --managed is off.
            let mut hi =
                Hi::setup_managed(hz, ch, -1, check_kbps(kbps)?, -1).map_err(out_of_range)?;
            hi.set2(None).expect("NULL never fails");
            hi
        }
        BitrateMode::Average {
            kbps,
            min_kbps,
            max_kbps,
        } => {
            ordered(min_kbps, Some(kbps), "a minimum above the average")?;
            ordered(Some(kbps), max_kbps, "a maximum below the average")?;
            ordered(min_kbps, max_kbps, "a minimum above the maximum")?;
            // oggenc --managed -b kbps [-m] [-M]
            Hi::setup_managed(
                hz,
                ch,
                limit_bps(max_kbps, -1)?,
                check_kbps(kbps)?,
                limit_bps(min_kbps, -1)?,
            )
            .map_err(out_of_range)?
        }
        BitrateMode::Limits { min_kbps, max_kbps } => {
            if min_kbps.is_none() && max_kbps.is_none() {
                return Err(BitrateError::Invalid("a limits mode without a limit"));
            }
            ordered(min_kbps, max_kbps, "a minimum above the maximum")?;
            // oggenc --managed -m/-M: setup_managed(max, -1 * 1000, min),
            // then (`managed && bitrate < 0`) GET, average -1, SET.
            let mut hi = Hi::setup_managed(
                hz,
                ch,
                limit_bps(max_kbps, -1)?,
                -1000,
                limit_bps(min_kbps, -1)?,
            )
            .map_err(out_of_range)?;
            let mut ai = hi.get2();
            ai.bitrate_average_kbps = -1;
            hi.set2(Some(&ai)).expect("validated limits");
            hi
        }
        BitrateMode::QualityLimits { min_kbps, max_kbps } => {
            if min_kbps.is_none() && max_kbps.is_none() {
                return Err(BitrateError::Invalid("a limits mode without a limit"));
            }
            ordered(min_kbps, max_kbps, "a minimum above the maximum")?;
            let (min, max) = (limit_bps(min_kbps, -1000)?, limit_bps(max_kbps, -1000)?);
            // oggenc -q -m/-M: setup_vbr, then RATEMANAGE2_SET with the
            // VBR setup's nominal bitrate filling in the reservoir.
            let mut hi = Hi::setup_vbr(quality.base(), hz, ch);
            let bitrate = setting_to_approx_bitrate(hi.template, hi.base_setting, ch);
            let ai = RateManage2 {
                management_active: true,
                bitrate_limit_min_kbps: min / 1000,
                bitrate_limit_max_kbps: max / 1000,
                bitrate_limit_reservoir_bits: bitrate * 2,
                bitrate_limit_reservoir_bias: 0.1,
                // oggenc passes bitrate / 1000 here, which makes libvorbis
                // refuse limits that don't bracket it; the -1 it sets
                // right after is what remains.
                bitrate_average_kbps: -1,
                bitrate_average_damping: 1.5,
            };
            // The catch-all templates' nominal bitrate is -1 per channel,
            // so the reservoir is negative: OV_EINVAL.
            hi.set2(Some(&ai)).map_err(|_| out_of_range(0))?;
            let mut ai = hi.get2();
            ai.bitrate_average_kbps = -1;
            hi.set2(Some(&ai)).expect("same values");
            hi
        }
    };

    if tuning.is_set() {
        // oggenc's set_advanced_encoder_options: GET, change, SET.
        let mut ai = hi.get2();
        if let Some(bits) = tuning.reservoir_bits {
            ai.bitrate_limit_reservoir_bits = bits as i64;
        }
        if let Some(b) = tuning.reservoir_bias {
            ai.bitrate_limit_reservoir_bias = b;
        }
        if let Some(d) = tuning.average_damping {
            ai.bitrate_average_damping = d;
        }
        hi.set2(Some(&ai))
            .map_err(|_| BitrateError::Invalid("tuning rejected by libvorbis"))?;
    }
    Ok(hi.finish(ch))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATES: [u32; 12] = [
        8000, 8999, 9000, 11025, 15000, 16000, 19000, 22050, 32000, 44100, 48000, 50000,
    ];

    /// The supported range is contiguous: every kbps in it has a setup.
    #[test]
    fn supported_range_is_contiguous() {
        for &hz in &RATES {
            for ch in [Channels::Mono, Channels::Stereo] {
                let rate = SampleRate::new(hz).unwrap();
                let r = BitrateMode::supported_kbps(rate, ch).expect("8-50 kHz have setups");
                for k in r.clone() {
                    let m = BitrateMode::average(k);
                    assert!(
                        plan(&m, &BitrateTuning::default(), Quality::Q5, rate, ch).is_ok(),
                        "{hz} {ch:?} {k} in {r:?}"
                    );
                }
                for k in [r.start() - 1, r.end() + 1] {
                    let m = BitrateMode::average(k);
                    let e = plan(&m, &BitrateTuning::default(), Quality::Q5, rate, ch);
                    assert!(matches!(e, Err(BitrateError::OutOfRange { .. })), "{k}");
                }
            }
        }
        for hz in [7999, 50001, 96000] {
            let rate = SampleRate::new(hz).unwrap();
            assert_eq!(BitrateMode::supported_kbps(rate, Channels::Stereo), None);
        }
    }

    #[test]
    fn header_fields_follow_oggenc() {
        let (r, c) = (SampleRate::HZ_44100, Channels::Stereo);
        let none = BitrateTuning::default();
        let p = |m| plan(&m, &none, Quality::Q5, r, c).unwrap();
        // -b 128: no management, nominal 128000, limits -1.
        let n = p(BitrateMode::Nominal { kbps: 128 });
        assert_eq!(n.header_bitrates, [-1, 128_000, -1]);
        assert!(!n.managed_setup && n.bitrate_info.is_none());
        // --managed -b 128 -M 160
        let a = p(BitrateMode::Average {
            kbps: 128,
            min_kbps: None,
            max_kbps: Some(160),
        });
        assert_eq!(a.header_bitrates, [160_000, 128_000, -1]);
        assert_eq!(a.bitrate_info.unwrap().reservoir_bits, 256_000);
        // --managed -M 160: nominal 140 kb/s setup; limits round-tripped.
        let l = p(BitrateMode::Limits {
            min_kbps: None,
            max_kbps: Some(160),
        });
        assert_eq!(l.header_bitrates[0], 160_000);
        assert_eq!(l.header_bitrates[2], 0);
        assert_eq!(l.bitrate_info.unwrap().avg_rate, -1000);
        assert_eq!(l.bitrate_info.unwrap().reservoir_bits, 280_000);
        // -q 5 -M 128
        let q = p(BitrateMode::QualityLimits {
            min_kbps: None,
            max_kbps: Some(128),
        });
        assert_eq!(q.header_bitrates, [128_000, 160_000, -1000]);
        assert_eq!(q.bitrate_info.unwrap().reservoir_bits, 320_000);
    }

    #[test]
    fn invalid_requests() {
        let (r, c) = (SampleRate::HZ_44100, Channels::Stereo);
        let none = BitrateTuning::default();
        let bad = |m: BitrateMode, t: &BitrateTuning| {
            matches!(
                plan(&m, t, Quality::Q5, r, c),
                Err(BitrateError::Invalid(_))
            )
        };
        assert!(bad(BitrateMode::average(0), &none));
        assert!(bad(BitrateMode::average(MAX_KBPS + 1), &none));
        assert!(bad(
            BitrateMode::Average {
                kbps: 128,
                min_kbps: Some(129),
                max_kbps: None
            },
            &none
        ));
        assert!(bad(
            BitrateMode::Limits {
                min_kbps: Some(200),
                max_kbps: Some(100)
            },
            &none
        ));
        assert!(bad(
            BitrateMode::QualityLimits {
                min_kbps: None,
                max_kbps: None
            },
            &none
        ));
        let t = BitrateTuning::default().with_reservoir_bias(1.5);
        assert!(bad(BitrateMode::average(128), &t));
        let t = BitrateTuning::default().with_average_damping(f64::NAN);
        assert!(bad(BitrateMode::average(128), &t));
        // Quality limits need a template with a bitrate table.
        let e = plan(
            &BitrateMode::QualityLimits {
                min_kbps: None,
                max_kbps: Some(500),
            },
            &none,
            Quality::Q5,
            SampleRate::HZ_96000,
            c,
        );
        assert!(matches!(
            e,
            Err(BitrateError::OutOfRange {
                supported: None,
                ..
            })
        ));
    }
}
