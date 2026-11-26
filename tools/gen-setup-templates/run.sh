#!/usr/bin/env bash
# Builds and runs the setup-template generator against the vendored
# libvorbis 1.3.7 sources, writing src/setup_books.bin and
# src/setup_templates.rs. Invoked by `just regen-setup-templates`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$SCRIPT_DIR/../.."
VEND="$ROOT/tools/debug-libvorbis-dump/vendored-libvorbis"
CC="${CC:-cc}"
CFLAGS="${CFLAGS:--O0 -ffp-contract=off -std=gnu99 -Wno-implicit-function-declaration -Wno-unused-function}"

OGG_INC=""
OGG_LIB=""
for prefix in /opt/homebrew/opt/libogg /usr/local/opt/libogg; do
    if [ -d "$prefix/include/ogg" ]; then
        OGG_INC="-I$prefix/include"
        OGG_LIB="-L$prefix/lib"
        break
    fi
done

# gen.c #includes vorbisenc.c, so it is not listed separately.
SRCS=(
    "$SCRIPT_DIR/gen.c"
    "$VEND/analysis.c" "$VEND/bitrate.c" "$VEND/block.c" "$VEND/codebook.c"
    "$VEND/debug_dump.c" "$VEND/envelope.c" "$VEND/floor0.c" "$VEND/floor1.c"
    "$VEND/info.c" "$VEND/lookup.c" "$VEND/lpc.c" "$VEND/lsp.c"
    "$VEND/mapping0.c" "$VEND/mdct.c" "$VEND/psy.c" "$VEND/registry.c"
    "$VEND/res0.c" "$VEND/sharedbook.c" "$VEND/smallft.c" "$VEND/synthesis.c"
    "$VEND/window.c"
)

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
$CC $CFLAGS -I"$VEND/include" -I"$VEND" $OGG_INC -o "$TMP/gen" "${SRCS[@]}" $OGG_LIB -logg -lm
"$TMP/gen" "$ROOT/src"
"${RUSTFMT:-$HOME/.cargo/bin/rustfmt}" --edition 2024 "$ROOT/src/setup_templates.rs"
