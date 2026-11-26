#!/usr/bin/env bash
# Builds and runs the setup-template generator against the pristine
# vendored libvorbis 1.3.7 / libogg 1.3.5 sources (same flags and checksum
# check as the oracles; see tools/oracle-encoder/common.sh), writing
# src/setup_books.bin and src/setup_templates.rs. Invoked by
# `just regen-setup-templates` and by CI's freshness check.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../oracle-encoder/common.sh
source "$ROOT/tools/oracle-encoder/common.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# gen.c #includes vorbisenc.c, so it is not listed separately.
lw_cc "$TMP/gen" "$SCRIPT_DIR/gen.c" \
    "${LW_SHIM_SRCS[@]}" "${LW_VORBIS_SRCS[@]}" "${LW_OGG_SRCS[@]}"
"$TMP/gen" "$ROOT/src"
RUSTFMT="${RUSTFMT:-$(command -v rustfmt || echo "$HOME/.cargo/bin/rustfmt")}"
"$RUSTFMT" --edition 2024 "$ROOT/src/setup_templates.rs"
