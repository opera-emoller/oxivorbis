//! Vorbis setup-header generation from the libvorbis 1.3.7 templates.
//!
//! Ports the setup-header half of `vorbisenc.c` (VBR, unmanaged):
//! `vorbis_encode_floor_setup`, `vorbis_encode_map_n_res_setup` /
//! `vorbis_encode_residue_setup` (incl. the lowpass -> residue `end`
//! rule) and the packers from `info.c` / `floor1.c` / `res0.c` /
//! `mapping0.c`. Codebooks are not re-packed here: every reachable static
//! book was packed by libvorbis's own `vorbis_staticbook_pack` at
//! generation time (`just regen-setup-templates`) and is bit-copied out
//! of [`setup_templates::BOOKS_PACKED`].
//!
//! Also returns the encode-only parameters that libvorbis keeps in
//! `vorbis_info_floor1` / `vorbis_info_residue0` but never writes to the
//! bitstream (floor fit tolerances, residue classmetrics).

#![allow(clippy::needless_range_loop)]

use crate::bitpack::{BitWriter, ov_ilog};
use crate::setup_templates as T;

// ---------------------------------------------------------------------------
// Template types (data lives in the generated `setup_templates.rs`)
// ---------------------------------------------------------------------------

/// "No book" marker in [`T::BOOKBLOCKS`] (a NULL `static_codebook *`).
pub(crate) const NB: u16 = u16::MAX;

/// `vorbis_info_floor1` template; integer slices are trailing-zero trimmed.
pub(crate) struct Floor1Template {
    pub partitions: i32,
    pub partitionclass: &'static [i32],
    pub class_dim: &'static [i32],
    pub class_subs: &'static [i32],
    pub class_book: &'static [i32],
    pub class_subbook: &'static [&'static [i32]],
    pub mult: i32,
    pub postlist: &'static [i32],
    pub maxover: f32,
    pub maxunder: f32,
    pub maxerr: f32,
    pub twofitweight: f32,
    pub twofitatten: f32,
}

/// `vorbis_info_residue0` template (secondstages/booklist are zero/unused
/// in all templates; the generator asserts this for secondstages).
#[allow(dead_code)]
pub(crate) struct Residue0Template {
    pub begin: i64,
    pub end: i64,
    pub grouping: i32,
    pub partitions: i32,
    pub partvals: i32,
    pub groupbook: i32,
    pub classmetric1: &'static [i32],
    pub classmetric2: &'static [i32],
}

/// `vorbis_residue_template`, unmanaged fields; indices into the
/// generated tables.
pub(crate) struct ResidueTemplate {
    pub res_type: i32,
    pub limit_type: i32,
    pub grouping: i32,
    pub res: u16,
    pub book_aux: u16,
    pub books_base: u16,
}

pub(crate) struct Mapping0Template {
    pub submaps: i32,
    pub chmuxlist: &'static [i32],
    pub floorsubmap: &'static [i32],
    pub residuesubmap: &'static [i32],
    pub coupling_steps: i32,
    pub coupling_mag: &'static [i32],
    pub coupling_ang: &'static [i32],
}

pub(crate) struct ModeTemplate {
    pub blockflag: i32,
    pub windowtype: i32,
    pub transformtype: i32,
    pub mapping: i32,
}

/// `vorbis_mapping_template`: index into [`T::MAPPINGS`] and
/// [`T::RESIDUE_SETS`].
pub(crate) struct MapResTemplate {
    pub map: u16,
    pub res: u16,
}

/// The subset of `ve_setup_data_template` the setup header depends on.
pub(crate) struct SetupTemplate {
    pub mappings: i32,
    pub quality_mapping: &'static [f64],
    pub coupling_restriction: i32,
    pub samplerate_min_restriction: i64,
    pub samplerate_max_restriction: i64,
    pub blocksize_short: &'static [i32],
    pub blocksize_long: &'static [i32],
    pub psy_lowpass: &'static [f64],
    pub floor_mapping_list: &'static [&'static [u16]],
    pub maps: &'static [MapResTemplate],
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Encode-only floor1 parameters (`vorbis_info_floor1` fields that are not
/// in the bitstream). Per floor, in setup-header order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FloorEncodeParams {
    pub maxover: f32,
    pub maxunder: f32,
    pub maxerr: f32,
    pub twofitweight: f32,
    pub twofitatten: f32,
}

/// Encode-only residue0 parameters. Full 64-entry arrays exactly as the
/// C `memcpy` of the template leaves them (zero padded).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResidueEncodeParams {
    pub classmetric1: [i32; 64],
    pub classmetric2: [i32; 64],
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EncodeParams {
    pub floors: Vec<FloorEncodeParams>,
    pub residues: Vec<ResidueEncodeParams>,
}

#[derive(Debug, Clone)]
pub(crate) struct GeneratedSetup {
    /// The complete setup header packet (starts with `\x05vorbis`).
    #[allow(dead_code)]
    pub packet: Vec<u8>,
    /// `ci->blocksizes` (short, long). Needed for the id header too.
    #[allow(dead_code)]
    pub blocksizes: [u32; 2],
    pub params: EncodeParams,
}

// ---------------------------------------------------------------------------
// Template selection / quality helpers
// ---------------------------------------------------------------------------

/// `get_setup_template`'s scan of `setup_list`, restricted to the 44.1/48
/// kHz templates (stereo precedes 5.1 precedes uncoupled in the list, so
/// for 1–2 channels the first match is stereo-for-2, uncoupled-for-1).
fn select_template(channels: usize, rate: u32) -> &'static SetupTemplate {
    for t in [&T::SETUP_44_STEREO, &T::SETUP_44_UNCOUPLED] {
        if (t.coupling_restriction == -1 || t.coupling_restriction as usize == channels)
            && (rate as i64) >= t.samplerate_min_restriction
            && (rate as i64) <= t.samplerate_max_restriction
        {
            return t;
        }
    }
    panic!("no setup template for {channels} ch @ {rate} Hz");
}

/// `vorbis_encode_setup_vbr` + `get_setup_template` + the lowpass line of
/// `vorbis_encode_setup_setting`: libvorbis base quality (-0.1..1.0) to
/// `(hi.base_setting, hi.lowpass_kHz)`, with libvorbis's float/double
/// mix reproduced exactly.
///
/// Agent-(a)'s `quality.rs` is the canonical owner of this derivation;
/// this copy exists so setup generation is testable stand-alone.
#[allow(dead_code)]
pub(crate) fn vbr_setting(rate: u32, channels: usize, quality: f32) -> (f64, f64) {
    // `quality+=.0000001;` — float += double: computed in double, stored float.
    let mut q = (quality as f64 + 0.0000001) as f32;
    if q as f64 >= 1.0 {
        q = 0.9999_f64 as f32;
    }
    let req = q as f64;
    let t = select_template(channels, rate);
    let mappings = t.mappings as usize;
    let map = t.quality_mapping;
    assert!(
        req >= map[0] && req <= map[mappings],
        "quality out of range"
    );
    let mut j = 0;
    while j < mappings {
        if req >= map[j] && req < map[j + 1] {
            break;
        }
        j += 1;
    }
    let base_setting = if j == mappings {
        j as f64 - 0.001
    } else {
        let low = map[j] as f32;
        let high = map[j + 1] as f32;
        // (double - float) / (float - float): the denominator is a float
        // subtraction, the division is in double, the result stored float.
        let del = ((req - low as f64) / (high - low) as f64) as f32;
        // `j+del` is an int + float add in float.
        (j as f32 + del) as f64
    };
    let is = base_setting as usize;
    let ds = base_setting - is as f64;
    let lowpass = t.psy_lowpass[is] * (1.0 - ds) + t.psy_lowpass[is + 1] * ds;
    (base_setting, lowpass)
}

// ---------------------------------------------------------------------------
// Internal setup model (mirrors codec_setup_info for the header subset)
// ---------------------------------------------------------------------------

struct Floor {
    partitions: i32,
    partitionclass: [i32; 31],
    class_dim: [i32; 16],
    class_subs: [i32; 16],
    class_book: [i32; 16],
    class_subbook: [[i32; 8]; 16],
    mult: i32,
    postlist: [i32; 65],
    enc: FloorEncodeParams,
}

struct Residue {
    res_type: i32,
    begin: i64,
    end: i64,
    grouping: i32,
    partitions: i32,
    groupbook: i32,
    secondstages: [i32; 64],
    booklist: Vec<i32>,
    enc: ResidueEncodeParams,
}

struct Mapping {
    submaps: i32,
    chmuxlist: [i32; 256],
    floorsubmap: [i32; 16],
    residuesubmap: [i32; 16],
    coupling_steps: i32,
    coupling_mag: [i32; 256],
    coupling_ang: [i32; 256],
}

struct Setup {
    /// `ci->book_param` as book ids into [`T::BOOKS`].
    books: Vec<u16>,
    floors: Vec<Floor>,
    residues: Vec<Option<Residue>>,
    maps: Vec<Mapping>,
    modes: Vec<&'static ModeTemplate>,
    blocksizes: [i64; 2],
}

fn pad<const N: usize>(src: &[i32]) -> [i32; N] {
    let mut a = [0i32; N];
    a[..src.len()].copy_from_slice(src);
    a
}

/// `book_dup_or_new`: pointer identity == book id identity.
fn book_dup_or_new(books: &mut Vec<u16>, id: u16) -> i32 {
    if let Some(i) = books.iter().position(|&b| b == id) {
        return i as i32;
    }
    books.push(id);
    books.len() as i32 - 1
}

fn vorbis_encode_floor_setup(s: &mut Setup, fi: usize) {
    let t = &T::FLOORS[fi];
    let mut class_subbook = [[0i32; 8]; 16];
    for (row, src) in class_subbook.iter_mut().zip(t.class_subbook) {
        *row = pad::<8>(src);
    }
    let mut f = Floor {
        partitions: t.partitions,
        partitionclass: pad::<31>(t.partitionclass),
        class_dim: pad::<16>(t.class_dim),
        class_subs: pad::<16>(t.class_subs),
        class_book: pad::<16>(t.class_book),
        class_subbook,
        mult: t.mult,
        postlist: pad::<65>(t.postlist),
        enc: FloorEncodeParams {
            maxover: t.maxover,
            maxunder: t.maxunder,
            maxerr: t.maxerr,
            twofitweight: t.twofitweight,
            twofitatten: t.twofitatten,
        },
    };
    let nbooks = s.books.len() as i32;
    let mut maxclass = -1;
    let mut maxbook = -1;
    for i in 0..f.partitions as usize {
        maxclass = maxclass.max(f.partitionclass[i]);
    }
    for i in 0..=maxclass as usize {
        maxbook = maxbook.max(f.class_book[i]);
        f.class_book[i] += nbooks;
        for k in 0..(1usize << f.class_subs[i]) {
            maxbook = maxbook.max(f.class_subbook[i][k]);
            if f.class_subbook[i][k] >= 0 {
                f.class_subbook[i][k] += nbooks;
            }
        }
    }
    let list = T::FLOOR_BOOKS[fi];
    assert_eq!(list.len() as i32, maxbook + 1, "floor {fi} book list");
    s.books.extend_from_slice(list);
    s.floors.push(f);
}

fn vorbis_encode_residue_setup(
    s: &mut Setup,
    number: usize,
    block: usize,
    rt: &ResidueTemplate,
    rate: u32,
    channels: usize,
    lowpass_khz: f64,
) {
    let base = &T::RESIDUE0_BASES[rt.res as usize];
    let mut r = Residue {
        res_type: rt.res_type,
        begin: base.begin,
        end: base.end,
        grouping: rt.grouping,
        partitions: base.partitions,
        groupbook: base.groupbook,
        secondstages: [0; 64],
        booklist: Vec::new(),
        enc: ResidueEncodeParams {
            classmetric1: pad::<64>(base.classmetric1),
            classmetric2: pad::<64>(base.classmetric2),
        },
    };
    if s.residues.len() <= number {
        s.residues.resize_with(number + 1, || None);
    }

    let bb = &T::BOOKBLOCKS[rt.books_base as usize];
    for i in 0..r.partitions as usize {
        for k in 0..4 {
            if bb[i][k] != NB {
                r.secondstages[i] |= 1 << k;
            }
        }
    }
    r.groupbook = book_dup_or_new(&mut s.books, rt.book_aux);
    for i in 0..r.partitions as usize {
        for k in 0..4 {
            if bb[i][k] != NB {
                let id = book_dup_or_new(&mut s.books, bb[i][k]);
                r.booklist.push(id);
            }
        }
    }

    // lowpass setup/pointlimit
    let mut freq: f64 = lowpass_khz * 1000.0;
    let nyq: f64 = rate as f64 / 2.0;
    let blocksize: i64 = s.blocksizes[block] >> 1;
    if freq > nyq {
        freq = nyq;
    }
    // (f->n = freq/nyq*blocksize is dead: floor1_look uses postlist[1].)
    // limit_type 1 (point-stereo) / 2 (LFE) only occur in 5.1 and
    // low-rate templates; the generator data has none for 44.1/48 kHz.
    assert_eq!(rt.limit_type, 0, "unsupported residue limit_type");

    let grouping = r.grouping;
    if r.res_type == 2 {
        let mut ch: i64 = 0;
        'outer: for mi in &s.maps {
            for j in 0..mi.submaps as usize {
                if mi.residuesubmap[j] as usize == number {
                    for k in 0..channels {
                        if mi.chmuxlist[k] as usize == j {
                            ch += 1;
                        }
                    }
                }
                if ch != 0 {
                    break 'outer;
                }
            }
        }
        r.end = (((freq / nyq * blocksize as f64 * ch as f64) / grouping as f64 + 0.9) as i32
            * grouping) as i64;
        if r.end > blocksize * ch {
            r.end = blocksize * ch / grouping as i64 * grouping as i64;
        }
    } else {
        r.end =
            (((freq / nyq * blocksize as f64) / grouping as f64 + 0.9) as i32 * grouping) as i64;
        if r.end > blocksize {
            r.end = blocksize / grouping as i64 * grouping as i64;
        }
    }
    if r.end == 0 {
        r.end = grouping as i64;
    }
    s.residues[number] = Some(r);
}

fn plan(rate: u32, channels: usize, base_setting: f64, lowpass_khz: f64) -> Setup {
    assert!((1..=2).contains(&channels), "1 or 2 channels supported");
    let t = select_template(channels, rate);
    let is = base_setting as usize;
    assert!(is < t.mappings as usize, "base_setting out of range");

    let mut s = Setup {
        books: Vec::new(),
        floors: Vec::new(),
        residues: Vec::new(),
        maps: Vec::new(),
        modes: Vec::new(),
        blocksizes: [t.blocksize_short[is] as i64, t.blocksize_long[is] as i64],
    };

    for list in t.floor_mapping_list {
        vorbis_encode_floor_setup(&mut s, list[is] as usize);
    }

    // vorbis_encode_map_n_res_setup
    let mr = &t.maps[is];
    let map = &T::MAPPINGS[mr.map as usize];
    let res = T::RESIDUE_SETS[mr.res as usize];
    let modes = if s.blocksizes[0] == s.blocksizes[1] {
        1
    } else {
        2
    };
    for i in 0..modes {
        s.modes.push(&T::MODE_TEMPLATE[i]);
        let m = &map[i];
        s.maps.push(Mapping {
            submaps: m.submaps,
            chmuxlist: pad::<256>(m.chmuxlist),
            floorsubmap: pad::<16>(m.floorsubmap),
            residuesubmap: pad::<16>(m.residuesubmap),
            coupling_steps: m.coupling_steps,
            coupling_mag: pad::<256>(m.coupling_mag),
            coupling_ang: pad::<256>(m.coupling_ang),
        });
        for j in 0..m.submaps as usize {
            let number = s.maps[i].residuesubmap[j] as usize;
            vorbis_encode_residue_setup(
                &mut s,
                number,
                i,
                &res[number],
                rate,
                channels,
                lowpass_khz,
            );
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Packing
// ---------------------------------------------------------------------------

fn copy_packed_book(w: &mut BitWriter, id: u16) {
    let (offset, bits) = T::BOOKS[id as usize];
    let bytes = &T::BOOKS_PACKED[offset as usize..offset as usize + bits.div_ceil(8) as usize];
    let full = (bits / 8) as usize;
    for &b in &bytes[..full] {
        w.write(b as u32, 8);
    }
    let rem = bits % 8;
    if rem != 0 {
        w.write(bytes[full] as u32, rem);
    }
}

fn floor1_pack(w: &mut BitWriter, f: &Floor) {
    let maxposit = f.postlist[1];
    let mut maxclass = -1;
    w.write(f.partitions as u32, 5);
    for j in 0..f.partitions as usize {
        w.write(f.partitionclass[j] as u32, 4);
        maxclass = maxclass.max(f.partitionclass[j]);
    }
    for j in 0..(maxclass + 1) as usize {
        w.write((f.class_dim[j] - 1) as u32, 3);
        w.write(f.class_subs[j] as u32, 2);
        if f.class_subs[j] != 0 {
            w.write(f.class_book[j] as u32, 8);
        }
        for k in 0..(1usize << f.class_subs[j]) {
            w.write((f.class_subbook[j][k] + 1) as u32, 8);
        }
    }
    w.write((f.mult - 1) as u32, 2);
    let rangebits = ov_ilog((maxposit - 1) as u32);
    w.write(rangebits, 4);
    let mut k = 0usize;
    let mut count = 0usize;
    for j in 0..f.partitions as usize {
        count += f.class_dim[f.partitionclass[j] as usize] as usize;
        while k < count {
            w.write(f.postlist[k + 2] as u32, rangebits);
            k += 1;
        }
    }
}

fn res0_pack(w: &mut BitWriter, r: &Residue) {
    w.write(r.begin as u32, 24);
    w.write(r.end as u32, 24);
    w.write((r.grouping - 1) as u32, 24);
    w.write((r.partitions - 1) as u32, 6);
    w.write(r.groupbook as u32, 8);
    let mut acc = 0usize;
    for j in 0..r.partitions as usize {
        let ss = r.secondstages[j] as u32;
        if ov_ilog(ss) > 3 {
            w.write(ss, 3);
            w.write(1, 1);
            w.write(ss >> 3, 5);
        } else {
            w.write(ss, 4);
        }
        acc += ss.count_ones() as usize;
    }
    for j in 0..acc {
        w.write(r.booklist[j] as u32, 8);
    }
}

fn mapping0_pack(w: &mut BitWriter, m: &Mapping, channels: usize) {
    if m.submaps > 1 {
        w.write(1, 1);
        w.write((m.submaps - 1) as u32, 4);
    } else {
        w.write(0, 1);
    }
    if m.coupling_steps > 0 {
        w.write(1, 1);
        w.write((m.coupling_steps - 1) as u32, 8);
        let bits = ov_ilog(channels as u32 - 1);
        for i in 0..m.coupling_steps as usize {
            w.write(m.coupling_mag[i] as u32, bits);
            w.write(m.coupling_ang[i] as u32, bits);
        }
    } else {
        w.write(0, 1);
    }
    w.write(0, 2);
    if m.submaps > 1 {
        for i in 0..channels {
            w.write(m.chmuxlist[i] as u32, 4);
        }
    }
    for i in 0..m.submaps as usize {
        w.write(0, 8);
        w.write(m.floorsubmap[i] as u32, 8);
        w.write(m.residuesubmap[i] as u32, 8);
    }
}

/// `_vorbis_pack_books`.
fn pack(s: &Setup, channels: usize) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write(0x05, 8);
    for &c in b"vorbis" {
        w.write(c as u32, 8);
    }
    w.write(s.books.len() as u32 - 1, 8);
    for &id in &s.books {
        copy_packed_book(&mut w, id);
    }
    w.write(0, 6);
    w.write(0, 16);
    w.write(s.floors.len() as u32 - 1, 6);
    for f in &s.floors {
        w.write(1, 16);
        floor1_pack(&mut w, f);
    }
    w.write(s.residues.len() as u32 - 1, 6);
    for r in &s.residues {
        let r = r.as_ref().expect("residue number never set up");
        w.write(r.res_type as u32, 16);
        res0_pack(&mut w, r);
    }
    w.write(s.maps.len() as u32 - 1, 6);
    for m in &s.maps {
        w.write(0, 16);
        mapping0_pack(&mut w, m, channels);
    }
    w.write(s.modes.len() as u32 - 1, 6);
    for m in &s.modes {
        w.write(m.blockflag as u32, 1);
        w.write(m.windowtype as u32, 16);
        w.write(m.transformtype as u32, 16);
        w.write(m.mapping as u32, 8);
    }
    w.write(1, 1);
    w.into_bytes()
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Builds the Vorbis setup header libvorbis 1.3.7 emits for VBR mode at
/// `rate` (40–50 kHz), `channels` (1 or 2), given `hi.base_setting` and
/// `hi.lowpass_kHz`, plus the encode-only floor/residue parameters.
pub(crate) fn build_setup(
    rate: u32,
    channels: usize,
    base_setting: f64,
    lowpass_khz: f64,
) -> GeneratedSetup {
    let s = plan(rate, channels, base_setting, lowpass_khz);
    let packet = pack(&s, channels);
    GeneratedSetup {
        packet,
        blocksizes: [s.blocksizes[0] as u32, s.blocksizes[1] as u32],
        params: EncodeParams {
            floors: s.floors.iter().map(|f| f.enc).collect(),
            residues: s
                .residues
                .iter()
                .map(|r| r.as_ref().expect("residue set").enc.clone())
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_blob::{
        Q5_SETUP_MONO44, Q5_SETUP_MONO48, Q5_SETUP_STEREO44, Q5_SETUP_STEREO48,
    };

    const COMBOS: [(u32, usize); 4] = [(44100, 1), (48000, 1), (44100, 2), (48000, 2)];

    fn q5_blob(rate: u32, ch: usize) -> &'static [u8] {
        match (rate, ch) {
            (44100, 1) => Q5_SETUP_MONO44,
            (48000, 1) => Q5_SETUP_MONO48,
            (44100, 2) => Q5_SETUP_STEREO44,
            (48000, 2) => Q5_SETUP_STEREO48,
            _ => unreachable!(),
        }
    }

    fn generate(rate: u32, ch: usize, q: f32) -> GeneratedSetup {
        let (base, lp) = vbr_setting(rate, ch, q);
        build_setup(rate, ch, base, lp)
    }

    #[test]
    fn q5_matches_committed_blobs() {
        for (rate, ch) in COMBOS {
            let g = generate(rate, ch, 0.5);
            assert!(
                g.packet == q5_blob(rate, ch),
                "setup mismatch for {rate} Hz {ch} ch ({} vs {} bytes)",
                g.packet.len(),
                q5_blob(rate, ch).len()
            );
            assert_eq!(g.blocksizes, [256, 2048]);
        }
    }

    #[test]
    fn q5_base_setting_and_lowpass() {
        // Sanity: q=0.5 lands just past template 6.
        let (base, lp) = vbr_setting(44100, 2, 0.5);
        assert_eq!(base as usize, 6);
        assert!(base > 6.0 && base < 6.0001, "{base}");
        assert!((lp - 20.1).abs() < 1e-3, "{lp}");
    }

    /// The encode-only parameters setup.rs inferred before they came from
    /// the template (partition count / postlist[1] heuristics), restricted
    /// to the entries the encoder reads.
    #[test]
    fn q5_encode_params_match_legacy_inference() {
        for (rate, ch) in COMBOS {
            let g = generate(rate, ch, 0.5);
            let blob_setup =
                crate::setup::unpack_q5_setup(q5_blob(rate, ch), ch, &g.params).expect("parse");
            assert_eq!(g.params.floors.len(), 2);
            for (fs, p) in blob_setup.floor_states.iter().zip(&g.params.floors) {
                let legacy_twofit = if fs.vi.postlist[1] == 128 { 1.0 } else { 3.0 };
                assert_eq!(
                    *p,
                    FloorEncodeParams {
                        maxover: 60.0,
                        maxunder: 30.0,
                        maxerr: 500.0,
                        twofitweight: legacy_twofit,
                        twofitatten: 18.0,
                    }
                );
            }
            let coupled = ch == 2;
            assert_eq!(g.params.residues.len(), 2);
            for (rs, p) in blob_setup.residue_setups.iter().zip(&g.params.residues) {
                let (cm1, cm2): (&[i32], &[i32]) = match (rs.partitions, coupled) {
                    (10, true) => (
                        &[0, 1, 1, 2, 2, 4, 8, 16, 32, 0],
                        &[0, 0, 999, 0, 999, 4, 8, 16, 32, 0],
                    ),
                    (10, false) => (
                        &[0, 1, 1, 2, 2, 4, 4, 16, 60, 0],
                        &[-1, 30, -1, 50, -1, 80, -1, -1, -1, 0],
                    ),
                    (n, c) => panic!("unexpected Q5 residue partitions={n} coupled={c}"),
                };
                let n = rs.partitions as usize;
                assert_eq!(&p.classmetric1[..n], cm1);
                assert_eq!(&p.classmetric2[..n], cm2);
            }
        }
    }

    /// q-1 (template 0) uses the 256x4 short floor whose twofitweight is
    /// 1.0 even though postlist[1] != 128 — the case the old heuristic
    /// got wrong.
    #[test]
    fn qm1_floor_params_from_template() {
        let g = generate(44100, 2, -0.1);
        assert_eq!(g.blocksizes, [512, 4096]);
        assert_eq!(g.params.floors[0].twofitweight, 1.0);
        let s = crate::setup::unpack_q5_setup(&g.packet, 2, &g.params).expect("parse");
        assert_eq!(s.floor_states[0].vi.postlist[1], 256);
    }

    /// Every quality in range must produce a header the existing parser
    /// accepts, with the right number of encode params.
    #[test]
    fn all_qualities_parse() {
        for (rate, ch) in COMBOS {
            for i in -10..=100 {
                let q = i as f32 / 100.0;
                let g = generate(rate, ch, q);
                let s = crate::setup::unpack_q5_setup(&g.packet, ch, &g.params)
                    .unwrap_or_else(|e| panic!("q={q} {rate}/{ch}: {e:?}"));
                assert_eq!(s.floor_states.len(), g.params.floors.len());
                assert_eq!(s.residue_setups.len(), g.params.residues.len());
            }
        }
    }

    #[cfg(feature = "oracle")]
    mod oracle {
        use super::*;
        use std::io::Write;
        use std::process::{Command, Stdio};

        fn oracle_bin() -> String {
            std::env::var("LEWTOFF_ORACLE")
                .unwrap_or_else(|_| "./tools/oracle-encoder/oracle-encoder".to_string())
        }

        fn ogg_packets(data: &[u8]) -> Vec<Vec<u8>> {
            let mut out = Vec::new();
            let mut cur = Vec::new();
            let mut pos = 0;
            while pos < data.len() {
                assert_eq!(&data[pos..pos + 4], b"OggS");
                let nseg = data[pos + 26] as usize;
                let segs = &data[pos + 27..pos + 27 + nseg];
                let mut body = pos + 27 + nseg;
                for &s in segs {
                    cur.extend_from_slice(&data[body..body + s as usize]);
                    body += s as usize;
                    if s < 255 {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                pos = body;
            }
            out
        }

        fn oracle_setup(rate: u32, ch: usize, q: &str) -> Vec<u8> {
            let mut child = Command::new(oracle_bin())
                .args([rate.to_string(), ch.to_string(), q.to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| {
                    panic!(
                        "{}: {e} — build it with ./tools/oracle-encoder/build.sh",
                        oracle_bin()
                    )
                });
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&vec![0u8; 4096 * ch * 2])
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "oracle failed for q={q}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let p = ogg_packets(&out.stdout).swap_remove(2);
            assert_eq!(&p[..7], b"\x05vorbis");
            p
        }

        fn qualities() -> Vec<String> {
            let mut qs: Vec<String> = (-2..=20)
                .map(|i| format!("{:.2}", i as f64 * 0.05))
                .collect();
            qs.extend(
                [
                    // fractional values
                    "0.123", "0.37", "0.4567", "0.618", "0.77", "0.8333", "-0.0333", "0.999",
                    // template-boundary neighbors
                    "-0.0999", "-0.0001", "0.0001", "0.0999", "0.1001", "0.2999", "0.3001",
                    "0.4999", "0.5001", "0.6999", "0.7001", "0.8999", "0.9001", "0.9999",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            qs
        }

        #[test]
        fn oracle_setup_header_sweep() {
            let mut bad = Vec::new();
            let mut n = 0;
            let mut distinct = std::collections::HashSet::new();
            for q in qualities() {
                let qf: f32 = q.parse().unwrap();
                for (rate, ch) in COMBOS {
                    let want = oracle_setup(rate, ch, &q);
                    let got = generate(rate, ch, qf).packet;
                    n += 1;
                    distinct.insert(want.clone());
                    if got != want {
                        bad.push(format!(
                            "q={q} {rate}/{ch}: got {} want {} bytes",
                            got.len(),
                            want.len()
                        ));
                    }
                }
            }
            assert!(
                bad.is_empty(),
                "{} of {n} mismatched:\n{}",
                bad.len(),
                bad.join("\n")
            );
            // Guard against a degenerate sweep (e.g. an oracle that ignores
            // the quality argument). Above q~0.6 the lowpass exceeds nyquist,
            // so only ~78 of the 180 points are distinct headers.
            assert!(
                distinct.len() > n / 4,
                "only {} distinct headers",
                distinct.len()
            );
        }
    }
}
