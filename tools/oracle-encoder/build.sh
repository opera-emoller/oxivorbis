#!/usr/bin/env bash
# Builds tools/oracle-encoder/oracle-encoder, the parity oracle, from
# main.c plus the pristine libvorbis 1.3.7 and libogg 1.3.5 sources
# vendored under tools/libvorbis-1.3.7/ and tools/libogg-1.3.5/ (checked
# against their SHA256SUMS first; see common.sh). Pinned flags:
#
#     -O0 -ffp-contract=off -std=gnu99
#
# so the output is bit-deterministic across hosts: no FMA contraction and
# no reordering of f32 ops.
#
# Every translation unit is compiled with `-include stable_qsort.h`, which
# redirects qsort() to a stable merge sort (stable_qsort.c). libvorbis's
# noise normalization (psy.c, q0..q3) sorts with ties, and libc qsort tie
# order differs between hosts (glibc: stable merge sort; macOS: not). The
# vendored .c files are not modified.
#
# Requires only a C compiler (clang or gcc); no system libogg/libvorbis.
# Extra flags can be appended with LW_EXTRA_CFLAGS, e.g. an ASan build:
#     LW_EXTRA_CFLAGS="-g -fsanitize=address" ./tools/oracle-encoder/build.sh
# OUT=<path> overrides the output path.

set -euo pipefail
# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

OUT="${OUT:-$ORACLE_DIR/oracle-encoder}"
lw_cc "$OUT" "$ORACLE_DIR/main.c" "${LW_SHIM_SRCS[@]}" \
    "${LW_VORBIS_SRCS[@]}" "$VEND/vorbisenc.c" "${LW_OGG_SRCS[@]}"
echo "build.sh: built $OUT"
