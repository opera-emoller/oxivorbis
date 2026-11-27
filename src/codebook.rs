//! Codebook unpack + encode, ported from libvorbis lib/sharedbook.c and
//! lib/codebook.c (libvorbis 1.3.7).

#![allow(clippy::manual_clamp)]
#![allow(clippy::explicit_counter_loop)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::collapsible_if)]

use std::sync::{Arc, OnceLock};

use crate::bitpack::{BitReader, BitWriter};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodebookError {
    BadSync,
    Eof,
    OverpopulatedTree,
    BadMaptype,
}

// ---------------------------------------------------------------------------
// Codebook struct
// ---------------------------------------------------------------------------

/// Runtime representation of one Vorbis codebook, encode-side only.
///
/// The tables are shared (`Arc`), so cloning a book is cheap: every setup
/// that uses a static book shares one copy (see [`static_book`]).
#[derive(Clone)]
pub(crate) struct Codebook {
    pub entries: usize,
    pub dim: usize,
    /// Huffman codeword for each entry (LSB-first, bit-reversed per libvorbis
    /// convention). u32::MAX means the entry is unused / has zero length.
    pub codewords: Arc<[u32]>,
    pub codeword_lengths: Arc<[u8]>,
    pub maptype: u8,
    /// Used by floor/residue encode for lattice VQ codebooks.
    pub quantvals: usize,
    pub minval: i32,
    pub delta: i32,
}

use crate::bitpack::ov_ilog;

// ---------------------------------------------------------------------------
// _float32_unpack: port of libvorbis _float32_unpack
// ---------------------------------------------------------------------------

const VQ_FMAN: i32 = 21;
const VQ_FEXP_BIAS: i32 = 768;

#[allow(non_snake_case)]
fn _float32_unpack(val: u32) -> f32 {
    let mant = (val & 0x1f_ffff) as f64;
    let sign = val & 0x8000_0000;
    let exp = ((val & 0x7fe0_0000) >> VQ_FMAN as u32) as i32;
    let mant = if sign != 0 { -mant } else { mant };
    let exp = exp - (VQ_FMAN - 1) - VQ_FEXP_BIAS;
    let exp = exp.max(-63).min(63);
    (mant * (2f64.powi(exp))) as f32
}

// ---------------------------------------------------------------------------
// _book_maptype1_quantvals: port of libvorbis _book_maptype1_quantvals
// ---------------------------------------------------------------------------

#[allow(non_snake_case)]
fn _book_maptype1_quantvals(entries: i64, dim: i64) -> i64 {
    if entries < 1 {
        return 0;
    }
    let mut vals = (entries as f64).powf(1.0 / dim as f64).floor() as i64;
    if vals < 1 {
        vals = 1;
    }
    loop {
        let mut acc: i64 = 1;
        let mut acc1: i64 = 1;
        let mut ok = true;
        for _i in 0..dim {
            if entries / vals < acc {
                ok = false;
                break;
            }
            acc *= vals;
            if i64::MAX / (vals + 1) < acc1 {
                acc1 = i64::MAX;
            } else {
                acc1 *= vals + 1;
            }
        }
        if ok && acc <= entries && acc1 > entries {
            return vals;
        } else if !ok || acc > entries {
            vals -= 1;
        } else {
            vals += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// _make_words: port of libvorbis _make_words (sparsecount=0 path for encode)
// ---------------------------------------------------------------------------

#[allow(non_snake_case)]
fn _make_words(lengths: &[u8], n: usize) -> Option<Vec<u32>> {
    let mut marker = [0u32; 33];
    let mut r = vec![0u32; n];
    let mut count = 0usize;

    for i in 0..n {
        let length = lengths[i] as usize;
        if length > 0 {
            let mut entry = marker[length];

            if length < 32 && (entry >> length) != 0 {
                return None; // overpopulated tree
            }
            r[count] = entry;
            count += 1;

            for j in (1..=length).rev() {
                if marker[j] & 1 != 0 {
                    if j == 1 {
                        marker[1] += 1;
                    } else {
                        marker[j] = marker[j - 1] << 1;
                    }
                    break;
                }
                marker[j] += 1;
            }

            for j in (length + 1)..33 {
                if (marker[j] >> 1) == entry {
                    entry = marker[j];
                    marker[j] = marker[j - 1] << 1;
                } else {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }

    // check for underpopulated tree (allow single-entry codebook exception)
    if !(count == 1 && marker[2] == 2) {
        for i in 1usize..33 {
            let mask = if i < 32 {
                0xffff_ffffu32 >> (32 - i)
            } else {
                0xffff_ffffu32
            };
            if marker[i] & mask != 0 {
                return None;
            }
        }
    }

    // bitreverse the words (LSB endian packer)
    let mut out = vec![0u32; n];
    let mut count = 0usize;
    for i in 0..n {
        let mut temp = 0u32;
        for j in 0..lengths[i] {
            temp <<= 1;
            temp |= (r[count] >> j) & 1;
        }
        out[count] = temp;
        count += 1;
    }

    Some(out)
}

// ---------------------------------------------------------------------------
// unpack_codebook: port of vorbis_staticbook_unpack
// ---------------------------------------------------------------------------

/// Unpack one codebook from the bit stream. Returns the codebook.
///
/// libogg's `oggpack_read` returns -1 on a read past the end, which is how
/// the C code detects EOF (`if(num==-1)goto _eofout;`). [`BitReader`]
/// reports that through [`BitReader::overran`]; an all-ones value read
/// from real data is valid (e.g. a 5-bit length of `0x1F` is length 32).
pub(crate) fn unpack_codebook(r: &mut BitReader) -> Result<Codebook, CodebookError> {
    // sync pattern 0x564342 (24 bits)
    let sync = r.read(24);
    if r.overran() || sync != 0x56_4342 {
        return Err(CodebookError::BadSync);
    }

    let dim = r.read(16) as usize;
    let entries = r.read(24) as usize;
    if r.overran() {
        return Err(CodebookError::Eof);
    }

    if ov_ilog(dim as u32) + ov_ilog(entries as u32) > 24 {
        return Err(CodebookError::Eof);
    }

    // codeword ordering: ordered (1) or unordered (0)?
    let ordered = r.read(1);
    let mut length_list = vec![0u8; entries];

    if ordered == 0 {
        // unordered
        let sparse = r.read(1) != 0;
        for len in length_list.iter_mut() {
            // sparse: each entry tagged with a used bit
            if !sparse || r.read(1) != 0 {
                let num = r.read(5);
                if r.overran() {
                    return Err(CodebookError::Eof);
                }
                *len = (num + 1) as u8;
            }
        }
    } else {
        // ordered: run-length encoded by length
        let mut length = r.read(5) as usize + 1;
        if r.overran() {
            return Err(CodebookError::Eof);
        }
        let mut i = 0usize;
        while i < entries {
            let num = r.read(ov_ilog((entries - i) as u32)) as usize;
            // C also rejects more codewords of one length than that many
            // bits can hold: (num-1) >> (length-1) > 1.
            if r.overran()
                || length > 32
                || num > entries - i
                || (num > 0 && (num - 1) >> (length - 1) > 1)
            {
                return Err(CodebookError::Eof);
            }
            length_list[i..i + num].fill(length as u8);
            i += num;
            length += 1;
        }
    }

    // mapping type (4 bits)
    let maptype = r.read(4) as u8;

    let (quantvals, minval, delta) = match maptype {
        0 => (0, 0, 0),
        1 | 2 => {
            let q_min = r.read(32);
            let q_delta = r.read(32);
            let q_quant = r.read(4) + 1;
            let _q_sequencep = r.read(1);

            let quantvals_count = match maptype {
                1 => _book_maptype1_quantvals(entries as i64, dim as i64) as usize,
                _ => entries * dim,
            };
            // The quantized values themselves only matter for decoding
            // (`_book_unquantize`); the encoder needs just minval / delta.
            for _ in 0..quantvals_count {
                r.read(q_quant);
            }
            if r.overran() {
                return Err(CodebookError::Eof);
            }

            let quantvals = _book_maptype1_quantvals(entries as i64, dim as i64) as usize;
            // libvorbis sharedbook.c uses (int)rint(...) here; rint is half-to-even.
            let minval = _float32_unpack(q_min).round_ties_even() as i32;
            let delta = _float32_unpack(q_delta).round_ties_even() as i32;
            (quantvals, minval, delta)
        }
        _ => return Err(CodebookError::BadMaptype),
    };

    let codewords = make_codewords(&length_list, entries);
    Ok(Codebook {
        entries,
        dim,
        codewords: codewords.into(),
        codeword_lengths: length_list.into(),
        maptype,
        quantvals,
        minval,
        delta,
    })
}

/// Static codebook `id` of [`crate::setup_templates::BOOKS`], unpacked (and
/// its codewords built) once per process. Books are shared across
/// qualities, channel counts and rates, so a setup at a new quality only
/// pays for books no earlier setup used. The result is exactly what
/// [`unpack_codebook`] returns for the same packed bits inside a setup
/// header: the packet copies them verbatim (`setup_gen::copy_packed_book`).
pub(crate) fn static_book(id: u16) -> &'static Codebook {
    use crate::setup_templates as T;
    static CACHE: OnceLock<Box<[OnceLock<Codebook>]>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| T::BOOKS.iter().map(|_| OnceLock::new()).collect());
    cache[id as usize].get_or_init(|| {
        let (offset, bits) = T::BOOKS[id as usize];
        let bytes = &T::BOOKS_PACKED[offset as usize..][..bits.div_ceil(8) as usize];
        unpack_codebook(&mut BitReader::new(bytes)).expect("static codebook failed to unpack")
    })
}

/// Build the codeword table for encode (identical to vorbis_book_init_encode's
/// codelist, which is _make_words with sparsecount=0).
fn make_codewords(lengths: &[u8], n: usize) -> Vec<u32> {
    match _make_words(lengths, n) {
        Some(words) => words,
        None => {
            // overpopulated or other error — fill with u32::MAX (unused marker)
            vec![u32::MAX; n]
        }
    }
}

// ---------------------------------------------------------------------------
// Codebook::encode: port of vorbis_book_encode
// ---------------------------------------------------------------------------

impl Codebook {
    /// Emit the Huffman codeword for `entry` into `w`.
    /// Returns the number of bits written.
    pub(crate) fn encode(&self, entry: usize, w: &mut BitWriter) -> usize {
        if entry >= self.entries {
            return 0;
        }
        let len = self.codeword_lengths[entry] as u32;
        if len == 0 {
            return 0;
        }
        w.write(self.codewords[entry], len);
        len as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack a synthetic codebook using BitWriter (same format as vorbis_staticbook_pack)
    /// and verify our unpack recovers the correct fields.
    fn pack_codebook_no_map_ordered(w: &mut BitWriter) {
        // sync
        w.write(0x564342, 24);
        // dim = 1
        w.write(1, 16);
        // entries = 4
        w.write(4, 24);
        // ordered = 1
        w.write(1, 1);
        // initial length: 1 - 1 = 0 (so first length is 1)
        w.write(0, 5);
        // ordered encoding: how many entries at each length
        // length 1: 1 entry -> write count=1 using ov_ilog(4-0)=3 bits
        w.write(1, 3); // entries at length 1: 1
        // now i=1, length=2; ov_ilog(4-1)=2 bits
        w.write(1, 2); // entries at length 2: 1
        // now i=2, length=3; ov_ilog(4-2)=2 bits
        w.write(2, 2); // entries at length 3: 2
        // maptype = 0 (no mapping)
        w.write(0, 4);
    }

    #[test]
    fn synthetic_codebook_ordered_no_map_roundtrip() {
        let mut w = BitWriter::new();
        pack_codebook_no_map_ordered(&mut w);
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        let book = unpack_codebook(&mut r).expect("unpack failed");

        assert_eq!(book.entries, 4);
        assert_eq!(book.dim, 1);
        assert_eq!(book.maptype, 0);
        assert_eq!(*book.codeword_lengths, [1, 2, 3, 3]);
        // codewords should be valid (not all-max)
        assert_ne!(book.codewords[0], u32::MAX);
    }

    #[test]
    fn synthetic_codebook_unordered_sparse_no_map() {
        let mut w = BitWriter::new();
        // sync
        w.write(0x564342, 24);
        // dim = 2
        w.write(2, 16);
        // entries = 3
        w.write(3, 24);
        // ordered = 0
        w.write(0, 1);
        // unused = 1 (sparse)
        w.write(1, 1);
        // entry 0: used=1, length=2-1=1
        w.write(1, 1);
        w.write(1, 5);
        // entry 1: used=1, length=3-1=2
        w.write(1, 1);
        w.write(2, 5);
        // entry 2: used=0
        w.write(0, 1);
        // maptype = 0
        w.write(0, 4);

        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let book = unpack_codebook(&mut r).expect("unpack failed");

        assert_eq!(book.entries, 3);
        assert_eq!(book.dim, 2);
        assert_eq!(book.maptype, 0);
        assert_eq!(book.codeword_lengths[0], 2);
        assert_eq!(book.codeword_lengths[1], 3);
        assert_eq!(book.codeword_lengths[2], 0); // unused
    }

    #[test]
    fn bad_sync_returns_error() {
        let mut w = BitWriter::new();
        w.write(0xDEADBE, 24); // wrong sync
        w.write(1, 16);
        w.write(4, 24);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert!(
            matches!(unpack_codebook(&mut r), Err(CodebookError::BadSync)),
            "expected BadSync error"
        );
    }

    #[test]
    fn q5_setup_unpacks_all_codebooks() {
        // The q5 44.1 kHz mono setup header: `\x05vorbis`, then
        // [codebook_count - 1] in 8 bits, then the codebooks back to back
        // (Vorbis I §4.2.4).
        let packet = crate::setup_gen::test_support::q5_mono44_packet();
        assert_eq!(&packet[0..7], b"\x05vorbis");
        let mut r = BitReader::new(&packet[7..]);
        let count = r.read(8) as usize + 1;
        let books: Vec<Codebook> = (0..count)
            .map(|i| unpack_codebook(&mut r).unwrap_or_else(|e| panic!("book {i}: {e:?}")))
            .collect();
        assert!(
            books.len() >= 10,
            "expected >=10 codebooks, got {}",
            books.len()
        );
        // All books must have at least 1 entry
        for (i, b) in books.iter().enumerate() {
            assert!(b.entries > 0, "codebook {} has 0 entries", i);
            assert!(b.dim > 0, "codebook {} has 0 dim", i);
        }
    }

    #[test]
    fn encode_emits_correct_bits() {
        // Single-entry codebook: length=1, codeword=0
        let book = Codebook {
            entries: 1,
            dim: 1,
            codewords: vec![0].into(),
            codeword_lengths: vec![1].into(),
            maptype: 0,
            quantvals: 0,
            minval: 0,
            delta: 0,
        };
        let mut w = BitWriter::new();
        let bits = book.encode(0, &mut w);
        assert_eq!(bits, 1);
        assert_eq!(w.into_bytes(), vec![0x00]);
    }

    #[test]
    fn float32_unpack_roundtrip_known_values() {
        // Test known values from libvorbis test cases:
        // -533200896 and 1611661312 appear in sharedbook.c self-test
        let mindel = _float32_unpack(-533200896i32 as u32);
        let delta = _float32_unpack(1611661312u32);
        assert!((mindel - (-3.0f32)).abs() < 0.01, "mindel={mindel}");
        assert!((delta - 1.0f32).abs() < 0.01, "delta={delta}");
    }

    /// Header of an unordered, dense book with `entries` entries.
    fn unordered_dense(w: &mut BitWriter, entries: u32) {
        w.write(0x564342, 24);
        w.write(1, 16); // dim
        w.write(entries, 24);
        w.write(0, 1); // unordered
        w.write(0, 1); // dense
    }

    /// A 5-bit length field of 0x1F is length 32, not EOF (C only treats
    /// oggpack_read's -1 as EOF).
    #[test]
    fn length_field_0x1f_is_length_32() {
        let mut w = BitWriter::new();
        unordered_dense(&mut w, 2);
        w.write(0, 5); // length 1
        w.write(0x1F, 5); // length 32
        w.write(0, 4); // maptype 0
        let bytes = w.into_bytes();
        let book = unpack_codebook(&mut BitReader::new(&bytes)).expect("valid book");
        assert_eq!(*book.codeword_lengths, [1, 32]);
    }

    /// Running out of data mid-book is EOF, in every branch.
    #[test]
    fn truncated_book_is_eof() {
        // Unordered: the stream ends after the header (and its padding).
        let mut w = BitWriter::new();
        unordered_dense(&mut w, 40);
        let bytes = w.into_bytes();
        let r = unpack_codebook(&mut BitReader::new(&bytes));
        assert!(matches!(r, Err(CodebookError::Eof)), "{:?}", r.err());

        // Ordered: stream ends before the first run count.
        let mut w = BitWriter::new();
        w.write(0x564342, 24);
        w.write(1, 16);
        w.write(1000, 24);
        w.write(1, 1); // ordered
        w.write(4, 5); // starting length 5
        let bytes = w.into_bytes();
        let r = unpack_codebook(&mut BitReader::new(&bytes));
        assert!(matches!(r, Err(CodebookError::Eof)), "{:?}", r.err());

        // Mapped: stream ends inside the quantized value list.
        let mut w = BitWriter::new();
        unordered_dense(&mut w, 1);
        w.write(0, 5);
        w.write(1, 4); // maptype 1
        w.write(0, 32);
        w.write(0, 32);
        w.write(7, 4); // 8-bit values, none written
        w.write(0, 1);
        let bytes = w.into_bytes();
        let r = unpack_codebook(&mut BitReader::new(&bytes));
        assert!(matches!(r, Err(CodebookError::Eof)), "{:?}", r.err());
    }

    /// Ordered books may not claim more codewords of one length than
    /// `length` bits can distinguish: C's `(num-1)>>(length-1) > 1`.
    #[test]
    fn ordered_rejects_overfull_length() {
        let book = |first_len: u32, count: u32| {
            let mut w = BitWriter::new();
            w.write(0x564342, 24);
            w.write(1, 16);
            w.write(count, 24);
            w.write(1, 1); // ordered
            w.write(first_len - 1, 5);
            w.write(count, ov_ilog(count));
            w.write(0, 4); // maptype 0
            let bytes = w.into_bytes();
            unpack_codebook(&mut BitReader::new(&bytes)).map(|b| b.codeword_lengths.to_vec())
        };
        assert_eq!(book(1, 2).unwrap(), vec![1, 1]);
        assert!(matches!(book(1, 3), Err(CodebookError::Eof)));
        assert_eq!(book(2, 4).unwrap(), vec![2; 4]);
        assert!(matches!(book(2, 5), Err(CodebookError::Eof)));
    }
}
