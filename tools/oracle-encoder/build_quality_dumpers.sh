#!/usr/bin/env bash
# Builds the two quality-parameter tools against the pristine vendored
# libvorbis 1.3.7 / libogg 1.3.5, with the same pinned flags and stable
# qsort shim as build.sh (see common.sh):
#
#   dump_quality_tables  prints src/tables/quality.rs (mode-template tables)
#   dump_params          prints the psy/global/ID-header params that
#                        vorbis_encode_init_vbr derives for given qualities
#
# dump_quality_tables #includes vorbisenc.c, so it links every other
# libvorbis source but not vorbisenc.c itself.
#
# Usage: ./tools/oracle-encoder/build_quality_dumpers.sh
# See also: `just regen-quality-tables`, and the oracle-gated test
# `quality::tests::quality_oracle_grid` (cargo nextest run --features oracle).

set -euo pipefail
# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

COMMON=("${LW_SHIM_SRCS[@]}" "${LW_VORBIS_SRCS[@]}" "${LW_OGG_SRCS[@]}")
lw_cc "$ORACLE_DIR/dump_quality_tables" "$ORACLE_DIR/dump_quality_tables.c" "${COMMON[@]}"
lw_cc "$ORACLE_DIR/dump_params" "$ORACLE_DIR/dump_params.c" "$VEND/vorbisenc.c" "${COMMON[@]}"
