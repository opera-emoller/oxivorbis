/*
 * Force-included (cc -include) into every vendored libvorbis translation
 * unit by build.sh. Redirects qsort() to a stable merge sort so tie order
 * is deterministic and identical on every libc.
 *
 * Why: psy.c's noise normalization (_vp_noise_normalize, active at
 * q0..q3 via normal_p) sorts magnitude pointers with qsort + apsort, and
 * equal magnitudes are common. qsort's tie order is unspecified: glibc
 * uses a merge sort (stable) for these sizes, macOS libc does not. With
 * this shim the oracle behaves like a stable sort everywhere, which is
 * what lewtoff mirrors.
 *
 * <stdlib.h> is included first so the system qsort prototype is declared
 * under its real name; the macro only rewrites later call sites.
 */
#ifndef LW_STABLE_QSORT_H
#define LW_STABLE_QSORT_H

#include <stddef.h>
#include <stdlib.h>

void lw_stable_qsort(void *base, size_t nmemb, size_t size,
                     int (*compar)(const void *, const void *));

#define qsort lw_stable_qsort

#endif
