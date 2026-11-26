#!/usr/bin/env bash
# Regenerates every committed file derived from the C oracles and fails if
# anything differs from what is committed. Used by CI on Linux/gcc and
# macOS/clang (`just check-fresh` locally). Expects the oracles to be built
# (build.sh, build_quality_dumpers.sh, build_dsp_oracle.sh).
#
#   tools/oracle-encoder/dsp_manifest.txt   dsp_oracle manifest
#   src/tables/quality.rs                   dump_quality_tables
#   src/tables/{trig,drft,window}.rs        dsp_oracle tables + gen-tables
#   src/setup_books.bin, src/setup_templates.rs   tools/gen-setup-templates
#   tests/vectors/headers/*, tests/vectors/ogg/*  tools/gen-header-vectors
#   tests/vectors/mdct/*                    tools/gen-vectors-mdct
#
# tests/vectors/golden/*.ogg are checked by the oracle-gated test
# `golden_vectors_are_fresh` instead.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

./tools/oracle-encoder/dsp_oracle manifest > tools/oracle-encoder/dsp_manifest.txt
./tools/oracle-encoder/dump_quality_tables > src/tables/quality.rs
mkdir -p "$TMP/dsp"
./tools/oracle-encoder/dsp_oracle tables "$TMP/dsp" > /dev/null
cargo run -q -p gen-tables -- "$TMP/dsp" > /dev/null
./tools/gen-setup-templates/run.sh
cargo run -q -p gen-header-vectors > /dev/null
./tools/gen-vectors-mdct/run.sh > /dev/null
cargo fmt --all

git diff --stat --exit-code -- \
    tools/oracle-encoder/dsp_manifest.txt src/tables/ \
    src/setup_books.bin src/setup_templates.rs tests/vectors/ \
    || { echo "check_fresh.sh: committed oracle-derived files are stale (see diff above)" >&2; exit 1; }
echo "check_fresh.sh: all oracle-derived files are up to date"
