//! The bitrate-managed half of libvorbis 1.3.7 `mapping0_forward`: the
//! extra floor fits for the low and high noise curves, the interpolated
//! fits in between (`floor1_interpolate_fit`), and the per-packetblob
//! storage they go to. `mapping0.rs` calls into this only when the bitrate
//! manager is active, then encodes every packetblob from these posts.

use crate::floor1::{FLOOR1_MAX_POSTS, Floor1State, floor1_fit};
use crate::psy::{PACKETBLOBS, VorbisLookPsy, vp_offset_and_mix};

/// `floor_posts[ch][k]` for every packetblob `k`, stored per blob:
/// `posts[k][ch]`, valid where `used[k][ch]` (libvorbis's non-NULL).
pub(crate) struct BlobPosts {
    pub posts: Vec<Vec<[i32; FLOOR1_MAX_POSTS]>>,
    pub used: Vec<Vec<bool>>,
}

impl BlobPosts {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            posts: vec![vec![[0; FLOOR1_MAX_POSTS]; channels]; PACKETBLOBS],
            used: vec![vec![false; channels]; PACKETBLOBS],
        }
    }
}

/// `floor1_interpolate_fit(A, B, del)` into `out` (both inputs non-NULL).
fn floor1_interpolate_fit(
    posts: usize,
    a: &[i32; FLOOR1_MAX_POSTS],
    b: &[i32; FLOOR1_MAX_POSTS],
    del: i32,
    out: &mut [i32; FLOOR1_MAX_POSTS],
) {
    for i in 0..posts {
        out[i] = ((65536 - del) * (a[i] & 0x7fff) + del * (b[i] & 0x7fff) + 32768) >> 16;
        if a[i] & 0x8000 != 0 && b[i] & 0x8000 != 0 {
            out[i] |= 0x8000;
        }
    }
}

/// The managed part of `mapping0_forward`'s per-channel loop for channel
/// `ch`, after the nominal fit (`mid`, `None` where libvorbis got NULL):
/// fits for noise curve 2 (blob `PACKETBLOBS-1`, higher rate) and 0 (blob
/// 0), then the interpolated blobs. `logmask` and `codes` are scratch;
/// `mdct` is only read (offset selects 0 and 2 leave it alone).
#[allow(clippy::too_many_arguments)]
pub(crate) fn managed_fits(
    bp: &mut BlobPosts,
    ch: usize,
    mid: Option<&[i32; FLOOR1_MAX_POSTS]>,
    psy_look: &VorbisLookPsy,
    noise: &[f32],
    tone: &[f32],
    logmask: &mut [f32],
    mdct: &mut [f32],
    logmdct: &[f32],
    floor: &Floor1State,
    codes: &mut [i32],
) {
    const MID: usize = PACKETBLOBS / 2;
    const TOP: usize = PACKETBLOBS - 1;
    let Some(mid) = mid else {
        // `if(vorbis_bitrate_managed(vb) && floor_posts[i][PACKETBLOBS/2])`
        // fails: every blob's floor is NULL (memset 0).
        for used in &mut bp.used {
            used[ch] = false;
        }
        return;
    };
    bp.posts[MID][ch] = *mid;
    bp.used[MID][ch] = true;

    // higher rate by way of lower noise curve
    vp_offset_and_mix(psy_look, noise, tone, 2, logmask, mdct, logmdct);
    bp.used[TOP][ch] = floor1_fit(floor, logmdct, logmask, &mut bp.posts[TOP][ch], codes);
    // lower rate by way of higher noise curve
    vp_offset_and_mix(psy_look, noise, tone, 0, logmask, mdct, logmdct);
    bp.used[0][ch] = floor1_fit(floor, logmdct, logmask, &mut bp.posts[0][ch], codes);

    // intermediate curves for intermediate rates
    let posts = floor.posts as usize;
    let step = |k: usize| (k * 65536 / MID) as i32;
    for k in 1..MID {
        bp.used[k][ch] = bp.used[0][ch];
        if bp.used[k][ch] {
            let (a, b) = (bp.posts[0][ch], bp.posts[MID][ch]);
            floor1_interpolate_fit(posts, &a, &b, step(k), &mut bp.posts[k][ch]);
        }
    }
    for k in MID + 1..TOP {
        bp.used[k][ch] = bp.used[TOP][ch];
        if bp.used[k][ch] {
            let (a, b) = (bp.posts[MID][ch], bp.posts[TOP][ch]);
            floor1_interpolate_fit(posts, &a, &b, step(k - MID), &mut bp.posts[k][ch]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-checked against floor1.c's formula.
    #[test]
    fn interpolate_fit_rounds_like_c() {
        let mut a = [0i32; FLOOR1_MAX_POSTS];
        let mut b = [0i32; FLOOR1_MAX_POSTS];
        a[..4].copy_from_slice(&[10, 0x8000 | 20, 0x8000 | 30, 100]);
        b[..4].copy_from_slice(&[20, 0x8000 | 40, 31, 0]);
        let mut out = [0i32; FLOOR1_MAX_POSTS];
        floor1_interpolate_fit(4, &a, &b, 3 * 65536 / 7, &mut out);
        // del = 28086: (37450*10 + 28086*20 + 32768) >> 16 = 14
        assert_eq!(out[..4], [14, 0x8000 | 29, 30, 57]);
    }
}
