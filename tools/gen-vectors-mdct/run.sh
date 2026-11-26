#!/usr/bin/env bash
# Regenerates tests/vectors/mdct/*.bin: forward-MDCT (n=2048) inputs and
# outputs from the pristine vendored libvorbis 1.3.7 mdct.c, compiled with
# the oracle flags (tools/oracle-encoder/common.sh). Invoked by
# `just regen-mdct-vectors` and tools/oracle-encoder/check_fresh.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../oracle-encoder/common.sh
source "$ROOT/tools/oracle-encoder/common.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
lw_cc "$TMP/harness" "$SCRIPT_DIR/harness.c" "$VEND/mdct.c" "${LW_SHIM_SRCS[@]}"
mkdir -p "$ROOT/tests/vectors/mdct"
(cd "$ROOT/tests/vectors/mdct" && "$TMP/harness")
