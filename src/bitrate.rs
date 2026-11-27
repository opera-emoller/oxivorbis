//! Bitrate management: a literal port of libvorbis 1.3.7 `lib/bitrate.c`
//! (`vorbis_bitrate_init`, `vorbis_bitrate_addblock`,
//! `vorbis_bitrate_flushpacket`).
//!
//! In a managed stream `mapping0_forward` encodes every block as
//! [`PACKETBLOBS`] candidate packets, from the smallest (blob 0: highest
//! noise curve, narrowest stereo) to the largest (blob 14). The manager
//! picks one per block from its two reservoirs: the average reservoir
//! steers a slowly moving "floater" towards the average bitrate, and the
//! min/max reservoir enforces the hard limits, padding the packet with
//! zero bytes or truncating it when even the extreme blob misses them.
//!
//! The state carries from block to block, so blocks must be added in
//! stream order. C `long` is 64 bits on every platform the oracle runs on
//! (LP64); `i64` here.

use crate::psy::PACKETBLOBS;

/// `bitrate_manager_info` (`ci->bi`), set by `vorbis_encode_setup_init`
/// from the high-level setup when `hi->managed`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BitrateInfo {
    pub avg_rate: i64,
    pub min_rate: i64,
    pub max_rate: i64,
    pub reservoir_bits: i64,
    pub reservoir_bias: f64,
    pub slew_damp: f64,
}

/// `bitrate_manager_state`.
#[derive(Clone, Debug)]
pub(crate) struct BitrateManager {
    managed: bool,
    avg_reservoir: i64,
    minmax_reservoir: i64,
    avg_bitsper: i64,
    min_bitsper: i64,
    max_bitsper: i64,
    short_per_long: i64,
    avgfloat: f64,
    bi: BitrateInfo,
    rate: i64,
    /// `ci->blocksizes[W] >> 1`.
    half: [i64; 2],
}

/// C `rint` in the default rounding mode.
#[inline]
fn rint(x: f64) -> f64 {
    x.round_ties_even()
}

impl BitrateManager {
    /// `vorbis_bitrate_init`. Management is active only with a positive
    /// reservoir (`bi->reservoir_bits > 0`); `bi = None` is an unmanaged
    /// setup (all zeros in C).
    pub(crate) fn new(bi: Option<BitrateInfo>, rate: u32, blocksizes: [usize; 2]) -> Self {
        let half = [(blocksizes[0] >> 1) as i64, (blocksizes[1] >> 1) as i64];
        let mut bm = BitrateManager {
            managed: false,
            avg_reservoir: 0,
            minmax_reservoir: 0,
            avg_bitsper: 0,
            min_bitsper: 0,
            max_bitsper: 0,
            short_per_long: 0,
            avgfloat: 0.0,
            bi: bi.unwrap_or(BitrateInfo {
                avg_rate: 0,
                min_rate: 0,
                max_rate: 0,
                reservoir_bits: 0,
                reservoir_bias: 0.0,
                slew_damp: 0.0,
            }),
            rate: rate as i64,
            half,
        };
        let bi = bm.bi;
        if bi.reservoir_bits > 0 {
            let ratesamples = rate as i64;
            let halfsamples = half[0];
            bm.short_per_long = (blocksizes[1] / blocksizes[0]) as i64;
            bm.managed = true;
            let bitsper =
                |r: i64| rint(1. * r as f64 * halfsamples as f64 / ratesamples as f64) as i64;
            bm.avg_bitsper = bitsper(bi.avg_rate);
            bm.min_bitsper = bitsper(bi.min_rate);
            bm.max_bitsper = bitsper(bi.max_rate);
            bm.avgfloat = (PACKETBLOBS / 2) as f64;
            let desired_fill = bm.desired_fill();
            bm.minmax_reservoir = desired_fill;
            bm.avg_reservoir = desired_fill;
        }
        bm
    }

    /// `vorbis_bitrate_managed`: does `mapping0_forward` encode every
    /// packetblob?
    pub(crate) fn managed(&self) -> bool {
        self.managed
    }

    /// `long desired_fill = bi->reservoir_bits * bi->reservoir_bias`.
    fn desired_fill(&self) -> i64 {
        (self.bi.reservoir_bits as f64 * self.bi.reservoir_bias) as i64
    }

    /// `vorbis_bitrate_addblock` followed by `vorbis_bitrate_flushpacket`
    /// for a managed block: chooses one of the block's [`PACKETBLOBS`]
    /// encodings (`blobs[k]` holds packetblob `k`'s bytes), pads or
    /// truncates it in place as libvorbis does, updates the reservoirs and
    /// returns its index. `long_block` is `vb->W`.
    pub(crate) fn choose(&mut self, long_block: bool, blobs: &mut [Vec<u8>]) -> usize {
        debug_assert!(self.managed);
        debug_assert_eq!(blobs.len(), PACKETBLOBS);
        let bi = self.bi;
        let w = long_block as usize;
        // `oggpack_bytes(vbi->packetblob[k]) * 8`. The floater keeps `k` in
        // 0..PACKETBLOBS (it moves towards a valid choice and never past
        // the half-way point beyond it); the clamp only guards against
        // libvorbis's out-of-bounds read should rounding ever reach 14.5.
        let bytes8 = |blobs: &[Vec<u8>], k: i32| {
            blobs[k.clamp(0, PACKETBLOBS as i32 - 1) as usize].len() as i64 * 8
        };

        let mut choice = rint(self.avgfloat) as i32;
        let mut this_bits = bytes8(blobs, choice);
        let spl = |x: i64| {
            if long_block {
                x * self.short_per_long
            } else {
                x
            }
        };
        let min_target_bits = spl(self.min_bitsper);
        let max_target_bits = spl(self.max_bitsper);
        let samples = self.half[w];
        let desired_fill = self.desired_fill();

        // look ahead for avg floater
        if self.avg_bitsper > 0 {
            let avg_target_bits = spl(self.avg_bitsper);
            let slewlimit = 15. / bi.slew_damp;

            if self.avg_reservoir + (this_bits - avg_target_bits) > desired_fill {
                while choice > 0
                    && this_bits > avg_target_bits
                    && self.avg_reservoir + (this_bits - avg_target_bits) > desired_fill
                {
                    choice -= 1;
                    this_bits = bytes8(blobs, choice);
                }
            } else if self.avg_reservoir + (this_bits - avg_target_bits) < desired_fill {
                while choice + 1 < PACKETBLOBS as i32
                    && this_bits < avg_target_bits
                    && self.avg_reservoir + (this_bits - avg_target_bits) < desired_fill
                {
                    choice += 1;
                    this_bits = bytes8(blobs, choice);
                }
            }

            let mut slew = rint(choice as f64 - self.avgfloat) / samples as f64 * self.rate as f64;
            if slew < -slewlimit {
                slew = -slewlimit;
            }
            if slew > slewlimit {
                slew = slewlimit;
            }
            self.avgfloat += slew / self.rate as f64 * samples as f64;
            choice = rint(self.avgfloat) as i32;
            this_bits = bytes8(blobs, choice);
        }

        // enforce min (if used) on the current floater (if used)
        if self.min_bitsper > 0 && this_bits < min_target_bits {
            // do we need to force the bitrate up?
            while self.minmax_reservoir - (min_target_bits - this_bits) < 0 {
                choice += 1;
                if choice >= PACKETBLOBS as i32 {
                    break;
                }
                this_bits = bytes8(blobs, choice);
            }
        }

        // enforce max (if used) on the current floater (if used)
        if self.max_bitsper > 0 && this_bits > max_target_bits {
            // do we need to force the bitrate down?
            while self.minmax_reservoir + (this_bits - max_target_bits) > bi.reservoir_bits {
                choice -= 1;
                if choice < 0 {
                    break;
                }
                this_bits = bytes8(blobs, choice);
            }
        }

        // Choice of packetblobs now made based on floater, and min/max
        // requirements. Now boundary check extreme choices.
        if choice < 0 {
            // Choosing a smaller packetblob is insufficient to trim
            // bitrate; the frame is truncated (oggpack_writetrunc keeps the
            // first `maxsize` bytes). The reservoir invariant
            // minmax_reservoir <= reservoir_bits keeps maxsize >= 0.
            let maxsize = (max_target_bits + (bi.reservoir_bits - self.minmax_reservoir)) / 8;
            choice = 0;
            let blob = &mut blobs[0];
            if blob.len() as i64 > maxsize {
                blob.truncate(maxsize.max(0) as usize);
            }
            this_bits = blob.len() as i64 * 8;
        } else {
            let mut minsize = (min_target_bits - self.minmax_reservoir + 7) / 8;
            if choice >= PACKETBLOBS as i32 {
                choice = PACKETBLOBS as i32 - 1;
            }
            // prop up bitrate according to demand: pad this frame out with
            // zeroes
            let blob = &mut blobs[choice as usize];
            minsize -= blob.len() as i64;
            if minsize > 0 {
                blob.resize(blob.len() + minsize as usize, 0);
            }
            this_bits = blob.len() as i64 * 8;
        }

        // now we have the final packet and the final packet size; update
        // the min and max reservoir
        if self.min_bitsper > 0 || self.max_bitsper > 0 {
            if max_target_bits > 0 && this_bits > max_target_bits {
                self.minmax_reservoir += this_bits - max_target_bits;
            } else if min_target_bits > 0 && this_bits < min_target_bits {
                self.minmax_reservoir += this_bits - min_target_bits;
            } else if self.minmax_reservoir > desired_fill {
                // in between; take the reservoir toward but not past
                // desired_fill
                if max_target_bits > 0 {
                    self.minmax_reservoir += this_bits - max_target_bits;
                    if self.minmax_reservoir < desired_fill {
                        self.minmax_reservoir = desired_fill;
                    }
                } else {
                    self.minmax_reservoir = desired_fill;
                }
            } else if min_target_bits > 0 {
                self.minmax_reservoir += this_bits - min_target_bits;
                if self.minmax_reservoir > desired_fill {
                    self.minmax_reservoir = desired_fill;
                }
            } else {
                self.minmax_reservoir = desired_fill;
            }
        }

        // avg reservoir
        if self.avg_bitsper > 0 {
            let avg_target_bits = spl(self.avg_bitsper);
            self.avg_reservoir += this_bits - avg_target_bits;
        }

        choice as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(avg: i64, min: i64, max: i64) -> BitrateInfo {
        BitrateInfo {
            avg_rate: avg,
            min_rate: min,
            max_rate: max,
            reservoir_bits: 2 * avg.max(min).max(max),
            reservoir_bias: 0.1,
            slew_damp: 1.5,
        }
    }

    /// Blobs of linearly growing size.
    fn blobs(base: usize, step: usize) -> Vec<Vec<u8>> {
        (0..PACKETBLOBS)
            .map(|k| vec![0xAA; base + k * step])
            .collect()
    }

    #[test]
    fn unmanaged_without_reservoir() {
        let bm = BitrateManager::new(None, 44100, [256, 2048]);
        assert!(!bm.managed());
        let mut bi = info(128_000, 0, 0);
        bi.reservoir_bits = 0;
        let bm = BitrateManager::new(Some(bi), 44100, [256, 2048]);
        assert!(!bm.managed());
    }

    #[test]
    fn init_matches_c() {
        let bm = BitrateManager::new(Some(info(128_000, 0, 0)), 44100, [256, 2048]);
        // rint(128000 * 128 / 44100) = rint(371.52)
        assert_eq!(bm.avg_bitsper, 372);
        assert_eq!(bm.short_per_long, 8);
        assert_eq!(bm.avg_reservoir, 25_600);
        assert_eq!(bm.minmax_reservoir, 25_600);
        assert_eq!(bm.avgfloat, 7.0);
    }

    /// Hard max: with every blob too big, the reservoir drains, then the
    /// smallest blob is chosen and truncated, and the stream never exceeds
    /// what the limit plus the reservoir allows.
    #[test]
    fn max_truncates_when_every_blob_is_too_big() {
        let mut bm = BitrateManager::new(Some(info(0, 0, 64_000)), 44100, [256, 2048]);
        let mut total = 0usize;
        let mut truncated = 0;
        for i in 0..2000 {
            let mut b = blobs(4000, 10);
            let k = bm.choose(true, &mut b);
            if i >= 100 {
                assert_eq!(k, 0, "block {i}");
            }
            truncated += (b[k].len() < 4000) as usize;
            total += b[k].len();
            assert!(bm.minmax_reservoir <= bm.bi.reservoir_bits);
        }
        assert!(truncated > 1900, "{truncated}");
        // 2000 long blocks of 1024 samples at <= 64 kb/s, plus the reservoir.
        let budget = (2000.0 * 1024.0 / 44100.0 * 64_000.0 / 8.0) as usize + 16_000;
        assert!(total <= budget, "{total} > {budget}");
    }

    /// Hard min: once the reservoir is spent, the largest blob is chosen
    /// and padded with zero bytes.
    #[test]
    fn min_pads_with_zeros() {
        let mut bm = BitrateManager::new(Some(info(0, 128_000, 0)), 44100, [256, 2048]);
        let mut padded = 0;
        for i in 0..2000 {
            let mut b = blobs(2, 1);
            let k = bm.choose(false, &mut b);
            if i >= 100 {
                assert_eq!(k, PACKETBLOBS - 1, "block {i}");
            }
            if b[k].len() > 2 + k {
                padded += 1;
                assert!(b[k][2 + k..].iter().all(|&x| x == 0));
            }
        }
        assert!(padded > 1800, "{padded}");
    }
}
