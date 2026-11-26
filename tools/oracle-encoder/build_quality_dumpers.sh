#!/usr/bin/env bash
# Builds the two quality-parameter tools against the vendored libvorbis,
# with the same pinned flags as build.sh (-O0 -ffp-contract=off):
#
#   dump_quality_tables  prints src/tables/quality.rs (mode-template tables)
#   dump_params          prints the psy/global/ID-header params that
#                        vorbis_encode_init_vbr derives for given qualities
#
# dump_quality_tables #includes vorbisenc.c, so it links every other vendored
# source but not vorbisenc.c itself.
#
# Usage: ./tools/oracle-encoder/build_quality_dumpers.sh
# See also: `just regen-quality-tables`, and the oracle-gated test
# `quality::tests::quality_oracle_grid` (cargo nextest run --features oracle).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VEND="$SCRIPT_DIR/../debug-libvorbis-dump/vendored-libvorbis"

CC="${CC:-cc}"
CFLAGS="${CFLAGS:--O0 -ffp-contract=off -std=gnu99 -Wno-implicit-function-declaration -Wno-unused-function -Wno-unused-variable}"

OGG_INC=""
OGG_LIB=""
for prefix in /opt/homebrew/opt/libogg /usr/local/opt/libogg; do
    if [ -d "$prefix/include/ogg" ]; then
        OGG_INC="-I$prefix/include"
        OGG_LIB="-L$prefix/lib"
        break
    fi
done

LIB_SRCS=(
    analysis.c bitrate.c block.c codebook.c debug_dump.c envelope.c floor0.c
    floor1.c info.c lookup.c lpc.c lsp.c mapping0.c mdct.c psy.c registry.c
    res0.c sharedbook.c smallft.c synthesis.c window.c
)
LIB_PATHS=()
for s in "${LIB_SRCS[@]}"; do LIB_PATHS+=("$VEND/$s"); done

build() {
    local out="$1"
    shift
    echo "build_quality_dumpers.sh: $CC $CFLAGS -> $out"
    # shellcheck disable=SC2086
    $CC $CFLAGS -I"$VEND/include" -I"$VEND" $OGG_INC -o "$out" "$@" \
        "${LIB_PATHS[@]}" $OGG_LIB -logg -lm
}

build "$SCRIPT_DIR/dump_quality_tables" "$SCRIPT_DIR/dump_quality_tables.c"
build "$SCRIPT_DIR/dump_params" "$SCRIPT_DIR/dump_params.c" "$VEND/vorbisenc.c"
