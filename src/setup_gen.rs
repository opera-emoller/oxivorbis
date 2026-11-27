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

/// `vorbis_residue_template`; indices into the generated tables. The
/// `_managed` books replace `book_aux` / `books_base` when the setup is
/// bitrate managed (`hi->managed`).
pub(crate) struct ResidueTemplate {
    pub res_type: i32,
    pub limit_type: i32,
    pub grouping: i32,
    pub res: u16,
    pub book_aux: u16,
    pub books_base: u16,
    pub book_aux_managed: u16,
    pub books_base_managed: u16,
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
    /// Quality -> base_setting mapping; consumed by `quality.rs` (via
    /// `tables::quality`), kept here for template completeness.
    #[allow(dead_code)]
    pub quality_mapping: &'static [f64],
    /// Template selection happens in `quality::select_template` (from
    /// `tables::quality::SETUP_LIST`, the same list in the same order);
    /// these are kept to cross-check the two generated lists in tests.
    #[allow(dead_code)]
    pub coupling_restriction: i32,
    #[allow(dead_code)]
    pub samplerate_min_restriction: i64,
    #[allow(dead_code)]
    pub samplerate_max_restriction: i64,
    pub blocksize_short: &'static [i32],
    pub blocksize_long: &'static [i32],
    /// Lowpass per template; `quality.rs` derives `lowpass_khz` from it.
    #[allow(dead_code)]
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
    /// The static book id ([`T::BOOKS`]) of each codebook, in header order.
    pub books: Vec<u16>,
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

#[allow(clippy::too_many_arguments)]
fn vorbis_encode_residue_setup(
    s: &mut Setup,
    number: usize,
    block: usize,
    rt: &ResidueTemplate,
    rate: u32,
    channels: usize,
    lowpass_khz: f64,
    managed: bool,
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

    // `if(ci->hi.managed)`: the two branches differ only in the books.
    let (book_aux, books_base) = if managed {
        (rt.book_aux_managed, rt.books_base_managed)
    } else {
        (rt.book_aux, rt.books_base)
    };
    let bb = &T::BOOKBLOCKS[books_base as usize];
    for i in 0..r.partitions as usize {
        for k in 0..4 {
            if bb[i][k] != NB {
                r.secondstages[i] |= 1 << k;
            }
        }
    }
    r.groupbook = book_dup_or_new(&mut s.books, book_aux);
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
    // limit_type 1 (point-stereo) / 2 (LFE) only occur in the 5.1
    // template; no 1-2 channel template uses them.
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

fn plan(
    template: usize,
    rate: u32,
    channels: usize,
    base_setting: f64,
    lowpass_khz: f64,
    managed: bool,
) -> Setup {
    assert!((1..=2).contains(&channels), "1 or 2 channels supported");
    let t = T::SETUP_LIST[template];
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
                managed,
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
    w.write_bits_from(bytes, bits as usize);
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

/// Builds the Vorbis setup header libvorbis 1.3.7 emits for VBR mode with
/// template `template` (the index `get_setup_template` picked, see
/// `quality::select_template`) at `rate`, `channels` (1 or 2), given
/// `hi.base_setting` and `hi.lowpass_kHz`, plus the encode-only
/// floor/residue parameters.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_setup(
    template: usize,
    rate: u32,
    channels: usize,
    base_setting: f64,
    lowpass_khz: f64,
) -> GeneratedSetup {
    build_setup_managed(template, rate, channels, base_setting, lowpass_khz, false)
}

/// [`build_setup`] for a setup that is bitrate managed (`hi->managed`) or
/// not; managed setups take the templates' `_managed` residue books.
pub(crate) fn build_setup_managed(
    template: usize,
    rate: u32,
    channels: usize,
    base_setting: f64,
    lowpass_khz: f64,
    managed: bool,
) -> GeneratedSetup {
    let s = plan(template, rate, channels, base_setting, lowpass_khz, managed);
    let packet = pack(&s, channels);
    GeneratedSetup {
        packet,
        blocksizes: [s.blocksizes[0] as u32, s.blocksizes[1] as u32],
        params: EncodeParams {
            books: s.books.clone(),
            floors: s.floors.iter().map(|f| f.enc).collect(),
            residues: s
                .residues
                .iter()
                .map(|r| r.as_ref().expect("residue set").enc.clone())
                .collect(),
        },
    }
}

/// Test-only helpers shared by the unit tests of the setup-header consumers
/// (codebook, floor1, residue): the q=0.5 (oggenc -q5) setup header and its
/// parsed form, built exactly the way the encoder builds them.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{GeneratedSetup, build_setup};

    /// The q=0.5 setup header for `rate` Hz, `channels` channels.
    pub(crate) fn q5_generated(rate: u32, channels: usize) -> GeneratedSetup {
        let c = crate::quality::quality_config(0.5, rate, channels);
        build_setup(c.template, rate, channels, c.base_setting, c.lowpass_khz)
    }

    /// The q=0.5, 44.1 kHz mono setup header packet (starts with
    /// `\x05vorbis`).
    pub(crate) fn q5_mono44_packet() -> Vec<u8> {
        q5_generated(44100, 1).packet
    }

    /// The q=0.5, 44.1 kHz mono setup header, parsed.
    pub(crate) fn q5_mono44_setup() -> crate::setup::CodecSetup {
        let g = q5_generated(44100, 1);
        crate::setup::unpack_setup(&g.packet, 1, &g.params).expect("q5 setup parses")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMBOS: [(u32, usize); 4] = [(44100, 1), (48000, 1), (44100, 2), (48000, 2)];

    /// A rate per template family plus range edges and odd rates; every
    /// one is swept mono and stereo.
    const SWEEP_RATES: [u32; 26] = [
        1, 1000, 7999, 8000, 8999, 9000, 11025, 14999, 15000, 16000, 18999, 19000, 22050, 25999,
        26000, 32000, 37800, 39999, 40000, 44100, 48000, 50000, 50001, 96000, 192000, 200000,
    ];

    fn sweep_combos() -> impl Iterator<Item = (u32, usize)> {
        SWEEP_RATES.iter().flat_map(|&r| [(r, 1), (r, 2)])
    }

    /// Length and SHA-256 of the q=0.5 setup headers libvorbis 1.3.7 emits
    /// (formerly committed as src/setup_blob_{mono,stereo}{44,48}.bin).
    fn q5_golden(rate: u32, ch: usize) -> (usize, &'static str) {
        match (rate, ch) {
            (44100, 1) => (
                3771,
                "00830733c1983d78a0c4310d04ce6d80ec6077c73ff9464a86d10e1e97a49a06",
            ),
            (48000, 1) => (
                3771,
                "b31b8ab438221970929ce5efe2b1d3e09dd923b23d2c2599573b48fb674f35ca",
            ),
            (44100, 2) => (
                4225,
                "936eace142731fecb7dff30e8eb7d4f858fa7d1be96675338d65bca0e289effe",
            ),
            (48000, 2) => (
                4225,
                "e29696dc86b52d72b5611c41a93605302dd96b1e8781ac48ab180fff4cc093fd",
            ),
            _ => unreachable!(),
        }
    }

    /// Minimal SHA-256 (FIPS 180-4), test-only, so the golden check needs
    /// no extra dev-dependency.
    fn sha256_hex(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        for chunk in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, b) in chunk.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *x = x.wrapping_add(y);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    #[test]
    fn sha256_known_answers() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    fn generate(rate: u32, ch: usize, q: f32) -> GeneratedSetup {
        let c = crate::quality::quality_config(q, rate, ch);
        build_setup(c.template, rate, ch, c.base_setting, c.lowpass_khz)
    }

    /// build_setup at q=0.5 must reproduce libvorbis's q5 setup headers
    /// byte for byte (the headers the encoder originally shipped as
    /// committed blobs).
    #[test]
    fn q5_matches_libvorbis_golden_hashes() {
        for (rate, ch) in COMBOS {
            let g = generate(rate, ch, 0.5);
            let (len, hash) = q5_golden(rate, ch);
            assert_eq!(g.packet.len(), len, "setup length, {rate} Hz {ch} ch");
            assert_eq!(sha256_hex(&g.packet), hash, "setup hash, {rate} Hz {ch} ch");
            assert_eq!(g.blocksizes, [256, 2048]);
        }
    }

    #[test]
    fn q5_base_setting_and_lowpass() {
        // Sanity: q=0.5 lands just past template 6.
        let c = crate::quality::quality_config(0.5, 44100, 2);
        let (base, lp) = (c.base_setting, c.lowpass_khz);
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
            let blob_setup = crate::setup::unpack_setup(&g.packet, ch, &g.params).expect("parse");
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
        let s = crate::setup::unpack_setup(&g.packet, 2, &g.params).expect("parse");
        assert_eq!(s.floor_states[0].vi.postlist[1], 256);
    }

    /// Every quality in range must produce a header the existing parser
    /// accepts, with the right number of encode params.
    #[test]
    fn all_qualities_parse() {
        for (rate, ch) in sweep_combos() {
            for i in -10..=100 {
                let q = i as f32 / 100.0;
                let g = generate(rate, ch, q);
                let s = crate::setup::unpack_setup(&g.packet, ch, &g.params)
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
            std::env::var("OXIVORBIS_ORACLE")
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
                    // 16/22 kHz template boundary (0.05)
                    "0.0499", "0.0501",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            qs
        }

        #[test]
        fn oracle_setup_header_sweep() {
            let qs = qualities();
            let combos: Vec<(u32, usize)> = sweep_combos().collect();
            // One thread per (rate, channels): ~2400 oracle runs.
            let results: Vec<(Vec<String>, Vec<Vec<u8>>)> = std::thread::scope(|sc| {
                let handles: Vec<_> = combos
                    .iter()
                    .map(|&(rate, ch)| {
                        let qs = &qs;
                        sc.spawn(move || {
                            let mut bad = Vec::new();
                            let mut wants = Vec::new();
                            for q in qs {
                                let qf: f32 = q.parse().unwrap();
                                let want = oracle_setup(rate, ch, q);
                                let got = generate(rate, ch, qf).packet;
                                if got != want {
                                    bad.push(format!(
                                        "q={q} {rate}/{ch}: got {} want {} bytes",
                                        got.len(),
                                        want.len()
                                    ));
                                }
                                wants.push(want);
                            }
                            (bad, wants)
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let n = qs.len() * combos.len();
            let bad: Vec<String> = results.iter().flat_map(|r| r.0.clone()).collect();
            let distinct: std::collections::HashSet<&Vec<u8>> =
                results.iter().flat_map(|r| &r.1).collect();
            assert!(
                bad.is_empty(),
                "{} of {n} mismatched:\n{}",
                bad.len(),
                bad.join("\n")
            );
            eprintln!(
                "oracle_setup_header_sweep: {n} headers identical, {} distinct",
                distinct.len()
            );
            // Guard against a degenerate sweep (e.g. an oracle that ignores
            // the quality argument). Once the lowpass exceeds nyquist the
            // header stops changing with quality (above q~0.6 at 44.1 kHz,
            // almost everywhere at the lowest rates), and rates of one
            // family share headers, so many points repeat. 284 today.
            assert!(
                distinct.len() > 250,
                "only {} distinct headers",
                distinct.len()
            );
            for (&(rate, ch), (_, wants)) in combos.iter().zip(&results) {
                let d: std::collections::HashSet<&Vec<u8>> = wants.iter().collect();
                assert!(
                    d.len() >= 2,
                    "{rate}/{ch}: only {} distinct headers",
                    d.len()
                );
            }
        }
    }
}
