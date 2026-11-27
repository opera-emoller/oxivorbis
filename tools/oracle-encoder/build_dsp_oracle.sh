#!/usr/bin/env bash
# Builds tools/oracle-encoder/dsp_oracle (see dsp_oracle.c) against the
# pristine vendored libvorbis 1.3.7 mdct.c / smallft.c / window.c with the
# same pinned flags as build.sh (see common.sh).
#
#   ./tools/oracle-encoder/build_dsp_oracle.sh
#   ./tools/oracle-encoder/dsp_oracle tables /tmp/oxivorbis-dsp-tables   # for gen-tables
#   ./tools/oracle-encoder/dsp_oracle manifest > tools/oracle-encoder/dsp_manifest.txt
#
# See also `just regen-tables` and `just regen-dsp-manifest`.

set -euo pipefail
# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

lw_cc "$ORACLE_DIR/dsp_oracle" "$ORACLE_DIR/dsp_oracle.c" \
    "$VEND/mdct.c" "$VEND/smallft.c" "$VEND/window.c"
