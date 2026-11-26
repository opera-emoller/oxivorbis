# Common dev commands for lewtoff.
# Install just via `brew install just` or `cargo install just`.

default:
    @just --list

# Full verify: fmt + clippy + tests.
verify: fmt-check clippy test

# One-time dev setup: install dev tools and git hooks. Safe to re-run.
setup: install-tools install-hooks

install-tools:
    cargo install cargo-nextest@0.9.128 --locked

install-hooks:
    git config core.hooksPath .githooks
    @echo "hooks installed: .githooks/pre-commit will run before each commit"
    @echo "bypass with 'git commit --no-verify' (avoid)"

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings
    cargo clippy --all-targets --features parallel -- -D warnings

# All unit + integration tests. Uses cargo-nextest if available
# (~40% faster wall-clock and ~99% less output on green runs).
test:
    #!/usr/bin/env bash
    if command -v cargo-nextest >/dev/null 2>&1; then
      cargo nextest run --status-level=fail --no-tests=warn
    else
      echo "cargo-nextest not installed; falling back to cargo test"
      echo "install: cargo install cargo-nextest@0.9.128 --locked"
      cargo test
    fi

test-verbose:
    cargo nextest run --no-tests=warn

# Oracle parity: encodes the same input via lewtoff and via the in-tree
# oracle-encoder (libvorbis 1.3.7, deterministic flags), byte-diffs the
# output. Runs both serial and parallel paths so a regression in either
# is caught locally.
parity:
    cargo nextest run --features oracle --no-tests=warn parity_
    cargo nextest run --features oracle,parallel --no-tests=warn parity_

# Corpus parity sweep: walks <repo_root>/corpus/ recursively and asserts
# byte-identical parity for every audio file (wav/mp3/ogg/flac/m4a/aif/aiff)
# against the in-tree oracle-encoder. Symlink your corpus there first, e.g.
#     ln -s /path/to/sounds corpus
# Positional args (all optional; "" = default):
#     just corpus                         # everything, 44100x2, q5
#     just corpus 100                     # first 100 files (sorted)
#     just corpus 100 parallel            # with the rayon `parallel` feature
#     just corpus 100 "" 0.3              # libvorbis base_quality 0.3 (= q3)
#     just corpus "" "" -0.1,0.5,1.0      # several qualities in one run
#     just corpus "" "" 0.5 all           # 44100x2,44100x1,48000x2,48000x1
#     just corpus "" "" "" 48000x1,44100x1 4   # 4 worker threads
# Each arg falls back to its env var when empty: CORPUS_LIMIT,
# CORPUS_QUALITY, CORPUS_CONFIGS, CORPUS_JOBS (default: all cores).
# Files are processed in parallel; each one shells out to ffmpeg and the
# -O0 oracle. The test is built with an optimized profile that keeps
# debug assertions and overflow checks, so panics behave as in `cargo test`.
# Failing inputs (s16le + both .ogg outputs + descriptor with a
# parity-fuzz replay command) land in
#     target/corpus-failures/<rate>x<ch>-q<q>/<relpath>/
#
# Corpus parity sweep (args: limit features quality-list configs jobs).
corpus limit="" extra_features="" quality="" configs="" jobs="":
    #!/usr/bin/env bash
    set -euo pipefail
    FEATS="oracle"
    if [ -n "{{extra_features}}" ]; then
        FEATS="oracle,{{extra_features}}"
    fi
    if [ -n "{{limit}}" ]; then export CORPUS_LIMIT="{{limit}}"; fi
    if [ -n "{{quality}}" ]; then export CORPUS_QUALITY="{{quality}}"; fi
    if [ -n "{{configs}}" ]; then export CORPUS_CONFIGS="{{configs}}"; fi
    if [ -n "{{jobs}}" ]; then export CORPUS_JOBS="{{jobs}}"; fi
    cargo nextest run --features "$FEATS" --no-tests=warn \
        --cargo-profile corpus \
        --config 'profile.corpus.inherits="release"' \
        --config 'profile.corpus.debug-assertions=true' \
        --config 'profile.corpus.overflow-checks=true' \
        --no-capture \
        --run-ignored=only -E 'test(corpus_parity_sweep)'

# Encode-throughput benchmark vs libvorbis (oggenc from vorbis-tools) using
# hyperfine. Decodes <input> once to s16le 44.1kHz stereo, then times both
# encoders end-to-end (stdin → ogg on stdout, dropped to /dev/null).
#
# Pass `parallel` as the second arg to bench the Rayon-parallel build:
#     just bench                                       # serial lewtoff
#     just bench corpus/path/to/file.wav               # serial on file
#     just bench corpus/path/to/file.wav parallel      # parallel on file
#     just bench corpus/beach_bro/snd_neon_nights.mp3 parallel
#
# Pass an oggenc-scale quality (-1..10, default 5) as the third arg:
#     just bench corpus/path/to/file.wav "" 2.5
bench input="corpus/beach_bro/snd_neon_nights.mp3" features="" quality="5":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v hyperfine >/dev/null 2>&1; then
        echo "hyperfine not on PATH — install it via \`brew install hyperfine\` (macOS) or \`cargo install hyperfine\`" >&2
        exit 2
    fi
    if ! command -v oggenc >/dev/null 2>&1; then
        echo "oggenc not on PATH — install it via \`brew install vorbis-tools\` (macOS) or \`apt install vorbis-tools\` (Debian/Ubuntu)" >&2
        exit 2
    fi
    : ${TMPDIR:=/tmp/}
    PCM="${TMPDIR%/}/lewtoff-bench.s16le"
    if [ ! -f "{{input}}" ]; then
        echo "input not found: {{input}}" >&2
        exit 2
    fi
    echo "decoding {{input}} → s16le 44100Hz stereo..."
    ffmpeg -hide_banner -loglevel error -y -i "{{input}}" \
        -f s16le -ar 44100 -ac 2 "$PCM"
    SIZE=$(wc -c < "$PCM")
    SAMPLES=$((SIZE / 4))
    SECS=$(awk "BEGIN{printf \"%.2f\", $SAMPLES/44100}")
    echo "input: $SAMPLES frames (${SECS}s of audio, $SIZE bytes raw)"
    BUILD_FLAGS=""
    LW_NAME="lewtoff"
    if [ -n "{{features}}" ]; then
        BUILD_FLAGS="--features {{features}}"
        LW_NAME="lewtoff ({{features}})"
    fi
    cargo build --release -q -p lewtoff-cli $BUILD_FLAGS
    LWBIN="$PWD/target/release/lewtoff"
    echo
    hyperfine --warmup 1 \
        --command-name "$LW_NAME" \
        "$LWBIN 44100 stereo {{quality}} < $PCM > /dev/null" \
        --command-name 'oggenc (libvorbis)' \
        "oggenc -Q -q {{quality}} -R 44100 -B 16 -C 2 --raw - -o - < $PCM > /dev/null"

# Profile a single lewtoff encode end-to-end with samply, opening the
# resulting profile in a browser (Firefox profiler UI). Same input
# semantics as `bench`.
#
#     just profile                          # uses corpus/beach_bro/snd_neon_nights.mp3
#     just profile corpus/path/to/file.wav  # use a specific file
profile input="corpus/beach_bro/snd_neon_nights.mp3":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v samply >/dev/null 2>&1; then
        echo "samply not on PATH — install via \`cargo install --locked samply\` or \`brew install samply\`" >&2
        exit 2
    fi
    : ${TMPDIR:=/tmp/}
    PCM="${TMPDIR%/}/lewtoff-bench.s16le"
    if [ ! -f "{{input}}" ]; then
        echo "input not found: {{input}}" >&2
        exit 2
    fi
    echo "decoding {{input}} → s16le 44100Hz stereo..."
    ffmpeg -hide_banner -loglevel error -y -i "{{input}}" \
        -f s16le -ar 44100 -ac 2 "$PCM"
    cargo build --release -q -p lewtoff-cli
    LWBIN="$PWD/target/release/lewtoff"
    echo "recording samply profile (browser will open when encode finishes)..."
    samply record -- "$LWBIN" 44100 stereo < "$PCM" > /dev/null

# Per-chunk diff helper for parity failures.
# Usage: just parity-diff input.s16le 44100 mono
parity-diff input rate channels:
    cargo run --bin parity-diff -- {{input}} {{rate}} {{channels}}

# Regenerate the embedded Q5 setup-header blob by extracting it from a
# fresh ffmpeg-libvorbis encode of a 1-sample silence file.
regen-setup-blob:
    cargo run -p gen-setup-blob

# Regenerate src/setup_books.bin (every reachable static codebook, packed
# by libvorbis's vorbis_staticbook_pack) and src/setup_templates.rs (the
# floor/residue/mapping/setup templates) from the vendored libvorbis.
# Requires a C compiler and libogg headers.
regen-setup-templates:
    ./tools/gen-setup-templates/run.sh

# Regenerate tests/vectors/headers/*.bin reference files from a fresh
# ffmpeg-libvorbis encode for each supported (rate × channels) combo.
regen-header-vectors:
    cargo run -p gen-header-vectors
    cp src/setup_blob.bin tests/vectors/headers/setup.bin

# Regenerate src/tables/quality.rs (libvorbis 44.1/48 kHz mode-template
# tables for quality-dependent setup) by compiling the vendored headers.
# Also builds tools/oracle-encoder/dump_params, used by the oracle-gated
# `quality_oracle_grid` test.
regen-quality-tables:
    ./tools/oracle-encoder/build_quality_dumpers.sh
    ./tools/oracle-encoder/dump_quality_tables > src/tables/quality.rs
    cargo fmt --all

# Regenerate src/tables/{trig,drft,window}.rs (MDCT, FFT and window tables
# for 128/256/512/2048/4096) by dumping libvorbis's runtime tables from the
# vendored sources (tools/oracle-encoder/dsp_oracle) and embedding them
# bit-for-bit. No Rust-side cos/sin, so the result is host-independent.
regen-tables:
    ./tools/oracle-encoder/build_dsp_oracle.sh
    mkdir -p /tmp/lewtoff-dsp-tables
    ./tools/oracle-encoder/dsp_oracle tables /tmp/lewtoff-dsp-tables
    cargo run -p gen-tables -- /tmp/lewtoff-dsp-tables
    cargo fmt --all

# Regenerate tools/oracle-encoder/dsp_manifest.txt: libvorbis hashes of the
# DSP tables and of mdct/drft/window outputs on seeded input, checked by the
# always-on `dsp_manifest_*` unit tests.
regen-dsp-manifest:
    ./tools/oracle-encoder/build_dsp_oracle.sh
    ./tools/oracle-encoder/dsp_oracle manifest > tools/oracle-encoder/dsp_manifest.txt

# Regenerate tests/vectors/mdct/*.bin from libvorbis 1.3.7.
# Requires ~/Documents/src/libvorbis cloned and clang on PATH.
regen-mdct-vectors:
    mkdir -p tests/vectors/mdct
    clang -O2 -ffp-contract=off -o tools/gen-vectors-mdct/harness \
        tools/gen-vectors-mdct/harness.c \
        ~/Documents/src/libvorbis/lib/mdct.c \
        -I$HOME/Documents/src/libvorbis/lib \
        -I$HOME/Documents/src/libvorbis/include \
        -I/opt/homebrew/include \
        -lm
    cd tests/vectors/mdct && $PWD/../../../tools/gen-vectors-mdct/harness
    rm tools/gen-vectors-mdct/harness

# Headless wasm parity check (uses wasm-pack + a node runtime).
wasm-test:
    wasm-pack test --node

debug-dump-c:
    #!/usr/bin/env bash
    set -e
    TOOL=tools/debug-libvorbis-dump
    /usr/bin/clang -O0 -g -std=c11 -ffp-contract=off \
      -I$TOOL/vendored-libvorbis \
      -I$TOOL/vendored-libvorbis/include \
      -I/opt/homebrew/include \
      $TOOL/harness.c \
      $TOOL/vendored-libvorbis/debug_dump.c \
      $TOOL/vendored-libvorbis/analysis.c \
      $TOOL/vendored-libvorbis/bitrate.c \
      $TOOL/vendored-libvorbis/block.c \
      $TOOL/vendored-libvorbis/codebook.c \
      $TOOL/vendored-libvorbis/envelope.c \
      $TOOL/vendored-libvorbis/floor0.c \
      $TOOL/vendored-libvorbis/floor1.c \
      $TOOL/vendored-libvorbis/info.c \
      $TOOL/vendored-libvorbis/lookup.c \
      $TOOL/vendored-libvorbis/lpc.c \
      $TOOL/vendored-libvorbis/lsp.c \
      $TOOL/vendored-libvorbis/mapping0.c \
      $TOOL/vendored-libvorbis/mdct.c \
      $TOOL/vendored-libvorbis/misc.c \
      $TOOL/vendored-libvorbis/psy.c \
      $TOOL/vendored-libvorbis/registry.c \
      $TOOL/vendored-libvorbis/res0.c \
      $TOOL/vendored-libvorbis/sharedbook.c \
      $TOOL/vendored-libvorbis/smallft.c \
      $TOOL/vendored-libvorbis/synthesis.c \
      $TOOL/vendored-libvorbis/vorbisenc.c \
      $TOOL/vendored-libvorbis/window.c \
      -L/opt/homebrew/lib -logg -lm \
      -o $TOOL/harness
    $TOOL/harness
    echo "dumps in /tmp/lewtoff-debug/c_*"

debug-dump-rust:
    LEWTOFF_DEBUG_DUMP=1 cargo run --bin debug-rust-dump
    echo "dumps in /tmp/lewtoff-debug/r_*"

debug-diff: debug-dump-rust
    cargo run --bin debug-diff

clean:
    cargo clean
