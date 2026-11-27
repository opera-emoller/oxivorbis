//! Vorbis setup-header parser (codebooks, floors, residues, mappings, modes)
//! plus the per-quality [`EncoderSetup`] cache.
//!
//! Reads a single Vorbis setup-header packet (the bytes starting with
//! `\x05vorbis`) and returns a `CodecSetup` with everything the encoder
//! needs at runtime.

#![allow(clippy::needless_range_loop)]

use crate::bitpack::BitReader;
use crate::codebook::{Codebook, static_book, unpack_codebook};
use crate::floor1::{Floor1State, floor1_look, unpack_floor1};
use crate::psy::VorbisInfoMapping0;
use crate::residue::{ResidueLook, ResidueSetup, residue_look, unpack_residue};
use crate::setup_gen::EncodeParams;

use crate::bitpack::ov_ilog;

// ---------------------------------------------------------------------------
// Mapping struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Mapping {
    pub submaps: usize,
    pub chmuxlist: Vec<usize>,
    pub floorsubmap: Vec<usize>,
    pub residuesubmap: Vec<usize>,
    #[allow(dead_code)]
    pub coupling_steps: usize,
    #[allow(dead_code)]
    pub coupling_mag: Vec<usize>,
    #[allow(dead_code)]
    pub coupling_ang: Vec<usize>,
    /// Mirrors VorbisInfoMapping0 for use in vp_couple_quantize_normalize
    pub vp_mapping: VorbisInfoMapping0,
}

// ---------------------------------------------------------------------------
// Mode struct (mirrors vorbis_info_mode)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Mode {
    pub blockflag: bool,
    pub mapping: usize,
}

// ---------------------------------------------------------------------------
// CodecSetup: everything the encoder needs (parsed setup header)
// ---------------------------------------------------------------------------

pub(crate) struct CodecSetup {
    #[allow(dead_code)]
    pub channels: usize,
    pub books: Vec<Codebook>,
    pub floor_states: Vec<Floor1State>,
    /// Residue type (0, 1, or 2) + setup + look for each residue
    pub residue_types: Vec<u16>,
    pub residue_setups: Vec<ResidueSetup>,
    pub residue_looks: Vec<ResidueLook>,
    pub mappings: Vec<Mapping>,
    pub modes: Vec<Mode>,
    pub modebits: u32,
}

// ---------------------------------------------------------------------------
// Parse error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum SetupError {
    BadMagic,
    Codebook(#[allow(dead_code)] crate::codebook::CodebookError),
    Floor(#[allow(dead_code)] crate::floor1::Floor1Error),
    Residue(#[allow(dead_code)] crate::residue::ResidueError),
    BadMappingType,
    #[allow(dead_code)]
    BadChannels,
    BadModeWindowtype,
    BadBitstream,
    /// `EncodeParams` does not have one entry per floor / residue.
    ParamsMismatch,
}

impl From<crate::codebook::CodebookError> for SetupError {
    fn from(e: crate::codebook::CodebookError) -> Self {
        SetupError::Codebook(e)
    }
}
impl From<crate::floor1::Floor1Error> for SetupError {
    fn from(e: crate::floor1::Floor1Error) -> Self {
        SetupError::Floor(e)
    }
}
impl From<crate::residue::ResidueError> for SetupError {
    fn from(e: crate::residue::ResidueError) -> Self {
        SetupError::Residue(e)
    }
}

// ---------------------------------------------------------------------------
// unpack_setup: parse a full setup header
// ---------------------------------------------------------------------------

/// Parses a setup header. `params` supplies the encode-only floor/residue
/// fields (one entry per floor / residue, in header order), normally from
/// [`crate::setup_gen::build_setup`]. If `params.books` names one static
/// book per codebook in the header, those books are taken from
/// [`static_book`] instead of being unpacked again.
pub(crate) fn unpack_setup(
    blob: &[u8],
    channels: usize,
    params: &EncodeParams,
) -> Result<CodecSetup, SetupError> {
    if blob.len() < 7 || &blob[0..7] != b"\x05vorbis" {
        return Err(SetupError::BadMagic);
    }
    let mut r = BitReader::new(&blob[7..]);

    // --- Codebooks ---
    let books_count = r.read(8) as usize + 1;
    let mut books = Vec::with_capacity(books_count);
    if params.books.len() == books_count {
        // The packet holds these static books' packed bits verbatim: share
        // the process-wide unpacked copies and skip over the bits.
        for &id in &params.books {
            books.push(static_book(id).clone());
            r.skip(crate::setup_templates::BOOKS[id as usize].1 as usize);
        }
        if r.overran() {
            return Err(SetupError::Codebook(crate::codebook::CodebookError::Eof));
        }
    } else {
        for _ in 0..books_count {
            books.push(unpack_codebook(&mut r)?);
        }
    }

    // --- Time domain placeholders ---
    let times = r.read(6) as usize + 1;
    for _ in 0..times {
        let _t = r.read(16);
    }

    // --- Floors ---
    let floors_count = r.read(6) as usize + 1;
    let mut floor_setups = Vec::with_capacity(floors_count);
    for _ in 0..floors_count {
        let floor_type = r.read(16);
        if floor_type != 1 {
            return Err(SetupError::BadBitstream);
        }
        let mut setup = unpack_floor1(&mut r, books_count)?;
        // Encode-side fields are not in the bitstream; they come from the
        // libvorbis floor1 template (see setup_gen).
        let p = params
            .floors
            .get(floor_setups.len())
            .ok_or(SetupError::ParamsMismatch)?;
        setup.maxover = p.maxover;
        setup.maxunder = p.maxunder;
        setup.maxerr = p.maxerr;
        setup.twofitweight = p.twofitweight;
        setup.twofitatten = p.twofitatten;
        if crate::debug_flag!("LW_DEBUG_SETUP") {
            eprintln!(
                "  floor: postlist[1]={} partitions={}",
                setup.postlist[1], setup.partitions
            );
        }
        floor_setups.push(setup);
    }
    if crate::debug_flag!("LW_DEBUG_SETUP") {
        eprintln!("floors_count={}", floors_count);
    }

    if floor_setups.len() != params.floors.len() {
        return Err(SetupError::ParamsMismatch);
    }

    // Build floor states (look structs)
    let floor_states: Vec<Floor1State> = floor_setups.into_iter().map(floor1_look).collect();

    // --- Residues ---
    let residues_count = r.read(6) as usize + 1;
    let mut residue_types = Vec::with_capacity(residues_count);
    let mut residue_setups = Vec::with_capacity(residues_count);
    for _ in 0..residues_count {
        let residue_type = r.read(16) as u16;
        let mut setup = unpack_residue(&mut r, books_count)?;
        // classmetric1/2 are encode-only; they come from the libvorbis
        // residue template (see setup_gen).
        let p = params
            .residues
            .get(residue_setups.len())
            .ok_or(SetupError::ParamsMismatch)?;
        setup.classmetric1 = p.classmetric1;
        setup.classmetric2 = p.classmetric2;
        residue_types.push(residue_type);
        residue_setups.push(setup);
    }

    if residue_setups.len() != params.residues.len() {
        return Err(SetupError::ParamsMismatch);
    }

    // Build residue looks
    let residue_looks: Vec<ResidueLook> = residue_setups
        .iter()
        .map(|s| residue_look(s, &books))
        .collect();

    // --- Mappings ---
    let mappings_count = r.read(6) as usize + 1;
    let mut mappings = Vec::with_capacity(mappings_count);
    for _ in 0..mappings_count {
        let mapping_type = r.read(16);
        if mapping_type != 0 {
            return Err(SetupError::BadMappingType);
        }
        let mapping = unpack_mapping(&mut r, channels, floors_count, residues_count)?;
        mappings.push(mapping);
    }

    // --- Modes ---
    let modes_count = r.read(6) as usize + 1;
    let mut modes = Vec::with_capacity(modes_count);
    for _ in 0..modes_count {
        let blockflag = r.read(1) != 0;
        let windowtype = r.read(16);
        let transformtype = r.read(16);
        if windowtype != 0 || transformtype != 0 {
            return Err(SetupError::BadModeWindowtype);
        }
        let mapping_idx = r.read(8) as usize;
        if crate::debug_flag!("LW_DEBUG_SETUP") {
            eprintln!("  mode: blockflag={} mapping={}", blockflag, mapping_idx);
        }
        modes.push(Mode {
            blockflag,
            mapping: mapping_idx,
        });
    }

    // modebits: number of bits needed to encode mode number
    let modebits = ov_ilog(modes_count as u32 - 1);

    Ok(CodecSetup {
        channels,
        books,
        floor_states,
        residue_types,
        residue_setups,
        residue_looks,
        mappings,
        modes,
        modebits,
    })
}

fn unpack_mapping(
    r: &mut BitReader,
    channels: usize,
    floors_count: usize,
    residues_count: usize,
) -> Result<Mapping, SetupError> {
    let has_submaps = r.read(1) != 0;
    let submaps = if has_submaps {
        r.read(4) as usize + 1
    } else {
        1
    };

    let has_coupling = r.read(1) != 0;
    let coupling_steps;
    let mut coupling_mag = Vec::new();
    let mut coupling_ang = Vec::new();

    if has_coupling {
        coupling_steps = r.read(8) as usize + 1;
        let ch_bits = ov_ilog(channels as u32 - 1);
        for _ in 0..coupling_steps {
            let mag = r.read(ch_bits) as usize;
            let ang = r.read(ch_bits) as usize;
            coupling_mag.push(mag);
            coupling_ang.push(ang);
        }
    } else {
        coupling_steps = 0;
    }

    // reserved 2 bits
    let _reserved = r.read(2);

    let mut chmuxlist = vec![0usize; channels];
    if submaps > 1 {
        for i in 0..channels {
            chmuxlist[i] = r.read(4) as usize;
        }
    }

    let mut floorsubmap = vec![0usize; submaps];
    let mut residuesubmap = vec![0usize; submaps];
    for i in 0..submaps {
        let _time_config = r.read(8); // unused
        let fs = r.read(8) as usize;
        let rs = r.read(8) as usize;
        if fs >= floors_count || rs >= residues_count {
            return Err(SetupError::BadBitstream);
        }
        floorsubmap[i] = fs;
        residuesubmap[i] = rs;
    }

    if crate::debug_flag!("LW_DEBUG_SETUP") {
        eprintln!(
            "  mapping: submaps={} floorsubmap={:?} residuesubmap={:?}",
            submaps, &floorsubmap, &residuesubmap
        );
    }

    let vp_mapping = VorbisInfoMapping0 {
        coupling_steps,
        coupling_mag: coupling_mag.clone(),
        coupling_ang: coupling_ang.clone(),
    };

    Ok(Mapping {
        submaps,
        chmuxlist,
        floorsubmap,
        residuesubmap,
        coupling_steps,
        coupling_mag,
        coupling_ang,
        vp_mapping,
    })
}

// ---------------------------------------------------------------------------
// Per-quality encoder setup cache
// ---------------------------------------------------------------------------

use std::sync::{Arc, Mutex};

use crate::psy::{VorbisLookPsy, vp_psy_init_reusing};
use crate::quality::{QualityConfig, quality_config};

/// Everything quality-dependent the encoder needs that does not depend on
/// the input samples: the derived parameters, the setup header packet, its
/// parsed form, and the psychoacoustic lookups. All of it is immutable
/// once built, so one instance is shared by every encode with the same key.
pub(crate) struct EncoderSetup {
    pub cfg: QualityConfig,
    /// Complete setup header packet (starts with `\x05vorbis`).
    pub packet: Vec<u8>,
    pub setup: CodecSetup,
    /// `psy_look[blocktype]` (`vp_psy_init` of `ci->psy_param[0..4]`):
    /// impulse and padding at n = bs0/2, transition and long at bs1/2.
    pub psy_looks: [VorbisLookPsy; 4],
}

/// Number of distinct (rate, channels, quality) setups kept alive. Building
/// one costs about a millisecond (the psy tone curves and codebook
/// unpacking dominate); the cache keeps repeated encodes of short files at
/// the same settings from paying it every call.
const CACHE_CAP: usize = 8;

type CacheKey = (u32, usize, u32);

static CACHE: Mutex<Vec<(CacheKey, Arc<EncoderSetup>)>> = Mutex::new(Vec::new());

fn build_encoder_setup(rate: u32, channels: usize, base_quality: f32) -> EncoderSetup {
    let cfg = quality_config(base_quality, rate, channels);
    let generated =
        crate::setup_gen::build_setup(rate, channels, cfg.base_setting, cfg.lowpass_khz);
    debug_assert_eq!(
        [
            generated.blocksizes[0] as usize,
            generated.blocksizes[1] as usize
        ],
        cfg.blocksizes
    );
    let setup = unpack_setup(&generated.packet, channels, &generated.params)
        .expect("generated setup header failed to parse");
    let gi = &cfg.psy_global;
    let bs = crate::window::BlockSizes {
        short: cfg.blocksizes[0],
        long: cfg.blocksizes[1],
    };
    let [vi_impulse, vi_padding, vi_transition, vi_long] = cfg.psy.clone();
    let rate = rate as i64;
    let impulse = vp_psy_init_reusing(vi_impulse, gi, bs.short_half(), rate, &[]);
    let padding = vp_psy_init_reusing(
        vi_padding,
        gi,
        bs.short_half(),
        rate,
        std::slice::from_ref(&impulse),
    );
    let transition = vp_psy_init_reusing(vi_transition, gi, bs.long_half(), rate, &[]);
    let long = vp_psy_init_reusing(
        vi_long,
        gi,
        bs.long_half(),
        rate,
        std::slice::from_ref(&transition),
    );
    let psy_looks = [impulse, padding, transition, long];
    EncoderSetup {
        cfg,
        packet: generated.packet,
        setup,
        psy_looks,
    }
}

/// Returns the (cached) encoder setup for `rate` Hz, `channels` and the
/// libvorbis `base_quality` (-0.1..=1.0). The cache is keyed on the exact
/// f32 bits of `base_quality`.
pub(crate) fn encoder_setup_for(
    rate: u32,
    channels: usize,
    base_quality: f32,
) -> Arc<EncoderSetup> {
    let key = (rate, channels, base_quality.to_bits());
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, s)) = cache.iter().find(|(k, _)| *k == key) {
            return Arc::clone(s);
        }
    }
    // Build outside the lock so concurrent encodes at other settings don't
    // serialize behind us.
    let built = Arc::new(build_encoder_setup(rate, channels, base_quality));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = cache.iter().find(|(k, _)| *k == key) {
        return Arc::clone(s);
    }
    if cache.len() >= CACHE_CAP {
        cache.remove(0);
    }
    cache.push((key, Arc::clone(&built)));
    built
}
