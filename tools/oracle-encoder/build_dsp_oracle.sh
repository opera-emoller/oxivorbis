#!/usr/bin/env bash
# Builds tools/oracle-encoder/dsp_oracle (see dsp_oracle.c) against the
# vendored libvorbis mdct.c / smallft.c / window.c with the same pinned
# flags as build.sh (-O0 -ffp-contract=off).
#
#   ./tools/oracle-encoder/build_dsp_oracle.sh
#   ./tools/oracle-encoder/dsp_oracle tables /tmp/lewtoff-dsp-tables   # for gen-tables
#   ./tools/oracle-encoder/dsp_oracle manifest > tools/oracle-encoder/dsp_manifest.txt
#
# See also `just regen-tables` and `just regen-dsp-manifest`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VEND="$SCRIPT_DIR/../debug-libvorbis-dump/vendored-libvorbis"

CC="${CC:-cc}"
CFLAGS="${CFLAGS:--O0 -ffp-contract=off -std=gnu99 -Wno-implicit-function-declaration}"

OGG_INC=""
for prefix in /opt/homebrew/opt/libogg /usr/local/opt/libogg; do
    if [ -d "$prefix/include/ogg" ]; then
        OGG_INC="-I$prefix/include"
        break
    fi
done

OUT="$SCRIPT_DIR/dsp_oracle"
echo "build_dsp_oracle.sh: $CC $CFLAGS -> $OUT"
# shellcheck disable=SC2086
$CC $CFLAGS -I"$VEND/include" -I"$VEND" $OGG_INC -o "$OUT" \
    "$SCRIPT_DIR/dsp_oracle.c" "$VEND/mdct.c" "$VEND/smallft.c" "$VEND/window.c" -lm
