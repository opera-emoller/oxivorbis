#!/usr/bin/env bash
# Builds tools/debug-libvorbis-dump/harness: the instrumented libvorbis copy
# in vendored-libvorbis/ (debug fprintf, /tmp/lewtoff-debug dumps, getenv
# hooks and a few post-1.3.7 upstream fixes) plus harness.c, which encodes
# 1 s of a 440 Hz sine and dumps the first short block's intermediate
# buffers to /tmp/lewtoff-debug/c_*.
#
# This is a debugging aid only. It is NOT the parity oracle: the oracles
# are built from the pristine tools/libvorbis-1.3.7 tree by
# tools/oracle-encoder/build.sh.
#
# Portable: uses $CC (default cc) and the vendored libogg 1.3.5 sources,
# so no system libogg or Homebrew prefix is needed.
#
#   ./tools/debug-libvorbis-dump/build.sh && ./tools/debug-libvorbis-dump/harness
#   (or `just debug-dump-c`)

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLS="$(cd "$DIR/.." && pwd)"
V="$DIR/vendored-libvorbis"
OGG="$TOOLS/libogg-1.3.5"
CC="${CC:-cc}"

SRCS=("$DIR/harness.c")
for s in debug_dump.c analysis.c bitrate.c block.c codebook.c envelope.c \
         floor0.c floor1.c info.c lookup.c lpc.c lsp.c mapping0.c mdct.c \
         misc.c psy.c registry.c res0.c sharedbook.c smallft.c synthesis.c \
         vorbisenc.c window.c; do
    SRCS+=("$V/$s")
done
SRCS+=("$OGG/src/framing.c" "$OGG/src/bitwise.c")

OUT="${OUT:-$DIR/harness}"
echo "debug-libvorbis-dump/build.sh: $CC -> $OUT"
"$CC" -O0 -g -std=gnu99 -ffp-contract=off \
    -Wno-implicit-function-declaration -Wno-unused-function -Wno-unused-variable \
    -include "$TOOLS/oracle-encoder/stable_qsort.h" \
    -I"$V" -I"$V/include" -I"$OGG/include" -I"$TOOLS/oracle-encoder/include" \
    -o "$OUT" "${SRCS[@]}" "$TOOLS/oracle-encoder/stable_qsort.c" -lm
