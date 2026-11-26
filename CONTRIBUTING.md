# Contributing to lewtoff

lewtoff's one hard requirement is byte-for-byte parity with libvorbis
1.3.7. Any change to the encoder has to keep every parity test green on
every platform. This document covers how that is checked and how to
work on the code.

## Setup

```sh
brew install just libogg vorbis-tools ffmpeg hyperfine   # macOS
sudo apt install just libogg-dev vorbis-tools ffmpeg     # Debian/Ubuntu
just setup               # installs cargo-nextest and the pre-commit hook
./tools/oracle-encoder/build.sh                  # the libvorbis oracle (needed for parity tests)
./tools/oracle-encoder/build_quality_dumpers.sh  # needed for quality_oracle_grid
```

The pre-commit hook (`.githooks/pre-commit`) runs `cargo fmt --check`
and `cargo clippy --all-targets -D warnings` whenever a commit touches
`.rs` files. `just verify` runs fmt, clippy and the tests.

## The oracle

`tools/oracle-encoder/oracle-encoder` is a small C program
(`tools/oracle-encoder/main.c`). It statically links the libvorbis 1.3.7
sources vendored under `tools/debug-libvorbis-dump/vendored-libvorbis/`.
`build.sh` compiles it with `-O0 -ffp-contract=off`, which rules out FMA
contraction and makes the output bit-deterministic across hosts. It also
redirects `qsort` to a stable merge sort, because libc `qsort` breaks
ties differently on glibc and macOS, and libvorbis's noise
normalization (q0 to q3) sorts with ties. The vendored `.c` files
themselves are unmodified.

The oracle reads s16le (or f32 LE with the `f32` argument) from stdin
and takes `<rate> <channels> <base_quality>`. It feeds libvorbis in
64-frame `vorbis_analysis_wrote` chunks and drains blocks after every
call. lewtoff's output is defined to match this.

## How parity is enforced

`tests/parity.rs` (behind the `oracle` feature) byte-diffs lewtoff's
output against two reference encoders:

- **`oracle_parity_*`**: the authoritative tests, run against the
  in-tree oracle.
- **`parity_*`**: the same comparison against the system `oggenc`
  (vorbis-tools). Only the silence cases are enabled. oggenc uses a
  different chunk size, and packaged libvorbis builds are optimized and
  may use FMA, so tone-rich inputs can legitimately differ. Those cases
  are `#[ignore]`d and point to their `oracle_parity_*` counterparts.
  `parity_oggenc_quality_conversion` pins the oggenc `-q` →
  `base_quality` float conversion against the system `oggenc`.

The quality matrix is
`oracle_parity_q::<quality>::<config>::<signal>`. It covers one
`base_quality` per libvorbis template interval (0.05 … 0.95, plus 0.0,
0.5 and 1.0) and the q-1 range (-0.1, -0.08, -0.05, -0.01, -0.0000001),
crossed with 44.1/48 kHz, mono/stereo, and five signals: silence, sine,
ramp, noise+tone and 10 s of transients.
`oracle_parity_short_input` sweeps input lengths 0..2100 frames, around
the 2048-sample long block, at two qualities.
`oracle_parity_fuzz_repros` replays the minimized fuzzer failures kept
in `tests/vectors/fuzz/`.

`tests/parity_f32.rs` checks `f32` input against the oracle's f32 mode.
It covers several qualities and configs, values beyond ±1.0,
subnormals, `-0.0`, NaN/inf (encoded as 0.0) and odd-sized chunked
writes.

`tests/streaming.rs` checks four things:

- any chunking of `write` / `write_f32` gives the one-shot bytes;
- pages are emitted before `finish`;
- the analysis buffer stays bounded over 10 minutes of input;
- with `parallel`, the batch size never changes the output.

Its long-input tests are `#[ignore]`d in debug builds, so run them with
`--release`.

Inside `src/`, the oracle-gated unit tests are:

- `quality_oracle_grid`: setup parameters versus libvorbis, for every
  quality.
- `oracle_setup_header_sweep`: setup headers.
- `dsp_manifest_*`: MDCT, FFT and window outputs against hashes
  produced by `tools/oracle-encoder/dsp_oracle`. These tests always run.

Integration tests reach a few unstable internals (header writers, exact
`base_quality` entry points, some DSP modules) through the undocumented
`__internal` feature. The crate enables that feature for its own tests
through a path-only self dev-dependency, so no extra flags are needed.
Cargo strips that dev-dependency when the crate is packaged.

## Running tests

```sh
cargo nextest run                               # unit + integration (no oracle)
cargo nextest run --release                     # also the long streaming tests
cargo nextest run --features oracle             # everything, incl. parity vs oracle + oggenc
cargo nextest run --features oracle parity_     # just the parity tests
just parity                                     # parity for the serial AND parallel paths
cargo test --doc                                # doctests (incl. README code blocks)
```

## Corpus sweep

Synthetic signals don't catch everything. The corpus sweep runs real
audio. Symlink your audio corpus at `<repo_root>/corpus/` (the directory
is gitignored) and run `just corpus`:

```sh
ln -s /path/to/sounds corpus
just corpus                                # everything, 44100x2, q5
just corpus 100                            # first 100 files (sorted)
just corpus 100 parallel                   # with the `parallel` feature
just corpus "" "" -0.1,0.5,1.0 all         # q-1/q5/q10 in all four configs
```

The sweep walks the directory recursively and picks up `.wav`, `.mp3`,
`.ogg`, `.flac`, `.m4a`, `.aif` and `.aiff` files. It decodes each one
with ffmpeg, so ffmpeg must be on `PATH`. It then byte-compares lewtoff
against the in-tree oracle-encoder, running in parallel across all
cores. The knobs can be set as environment variables or as positional
arguments to `just corpus`:

| Variable         | Example                         | Default    | Meaning |
|------------------|---------------------------------|------------|---------|
| `CORPUS_LIMIT`   | `100`                           | all files  | Test only the first N files (sorted) |
| `CORPUS_QUALITY` | `-0.1,0.5,1.0`                  | `0.5`      | libvorbis `base_quality` list |
| `CORPUS_CONFIGS` | `44100x2,48000x1`, or `all`     | `44100x2`  | Rate × channel configurations |
| `CORPUS_JOBS`    | `4`                             | all cores  | Worker threads |

Positional order: `just corpus <limit> <extra-features> <quality> <configs> <jobs>`.
Use `""` to skip an argument.

Failing inputs are saved under
`target/corpus-failures/<config>-q<q>/<relpath>/`. Each one holds the
s16le input, both `.ogg` outputs, and a descriptor with a
`parity-fuzz --pcm` replay command.

The latest full sweep covered a 6653-file corpus. It was byte-identical
in all four configurations at q-1, q5 and q10 (79,836 encodes), and at
44.1 kHz stereo also at q0, q2, q3 and q8.

## Differential fuzzer

`tools/parity-fuzz` generates seeded synthetic signals and encodes each
one with both lewtoff and the oracle. lewtoff streams each case through
`Encoder` in pseudo-random chunk sizes, so every case also checks that
chunking doesn't change the output. On any divergence or panic, the
fuzzer saves the case and a minimized reproduction under
`target/parity-fuzz/<id>/`.

```sh
cargo run -p parity-fuzz --release -- --iters 10000 --seed 1 --quality 0.5
#   --quality Q          libvorbis base_quality (-0.1..=1.0)
#   --rates 44100,48000  --channels 1,2  --jobs J  --max-secs T  --max-len SECS
#   --keep-going         collect all failures instead of stopping at the first
#   --minimize-per-sig N only minimize the first N failures per signature
#   --no-minimize        --start I  --min-frames N  --out DIR  --oracle PATH

# Replay a saved raw file (one rate / channel count):
cargo run -p parity-fuzz --release -- --pcm input.s16le --rates 44100 --channels 2
```

The fuzzer has run more than 100k cases clean. When it finds a failure,
copy the minimized reproduction into `tests/vectors/fuzz/` so that
`oracle_parity_fuzz_repros` keeps it fixed.

## Debugging a divergence

- `cargo run -p parity-diff -- lewtoff.ogg oracle.ogg` parses two Ogg
  files and reports the first divergence at page, packet and byte
  granularity, ignoring the serial number. Use it on the two `.ogg`
  files saved for a corpus or fuzz failure.
- `just debug-dump-c` builds `tools/debug-libvorbis-dump`, a copy of the
  vendored libvorbis instrumented to dump intermediate buffers to
  `/tmp/lewtoff-debug/c_*`. `just debug-dump-rust`
  (`LEWTOFF_DEBUG_DUMP=1`) writes the matching `/tmp/lewtoff-debug/r_*`
  from lewtoff. `just debug-diff` compares the two to find the first
  layer where they diverge.
- `just bench [input] [parallel] [quality]` times lewtoff against
  `oggenc` with hyperfine, using `tools/lewtoff-cli`. `just profile
  [input]` records a samply profile.

The usual culprits are:

- f32 being promoted to f64, or the reverse;
- `rint` versus `.round()`;
- operation order in sums;
- libvorbis's IEEE_FLOAT32 `todB` inline.

The commit log records the precision pitfalls already hit.

## Regenerating committed tables

All tables come from the vendored libvorbis, never from Rust-side
`sin`/`cos`, so they are host-independent. CI regenerates the
oracle-derived files on Linux/gcc and macOS/clang, and fails if either
differs from what is committed.

| Recipe                       | Regenerates |
|------------------------------|-------------|
| `just regen-tables`          | `src/tables/{trig,drft,window}.rs` (MDCT, FFT, window tables) via `dsp_oracle` + `gen-tables` |
| `just regen-dsp-manifest`    | `tools/oracle-encoder/dsp_manifest.txt` (hashes checked by `dsp_manifest_*`) |
| `just regen-quality-tables`  | `src/tables/quality.rs` (44.1/48 kHz mode-template tables) |
| `just regen-setup-templates` | `src/setup_books.bin` and `src/setup_templates.rs` (codebooks, floor/residue/mapping templates) |
| `just regen-header-vectors`  | `tests/vectors/headers/*.bin` |
| `just regen-mdct-vectors`    | `tests/vectors/mdct/*.bin` |

## CI

`.github/workflows/ci.yml` runs three jobs:

- **ubuntu** installs ffmpeg, vorbis-tools and libogg-dev, and checks
  that the system libvorbis is 1.3.x. It builds the C oracles from the
  vendored sources and regenerates the oracle-derived tables, failing on
  any diff. It runs fmt, clippy (default, `parallel`, `oracle,parallel`
  and the whole workspace), and a guard that the oracle-gated test
  groups still exist. It then runs the test suite without the oracle and
  with it, each on both the serial and parallel paths. Last comes a
  fixed-seed parity-fuzz smoke run at five qualities.
- **macos** builds the same oracles, does the same table check, and
  runs the same oracle suites and fuzz smoke on Apple silicon with
  clang and macOS libm.
- **wasm** builds for `wasm32-unknown-unknown`.

Parity has also been verified by hand on Windows x86_64 (MSVC). Rust
output there is identical.

## Packaging

The published crate contains only `src/`, `examples/`, the DSP manifest
its unit tests `include_str!`, and the top-level docs and licenses (see
`include` in `Cargo.toml`). Check the contents with
`cargo package --list` and `cargo publish --dry-run`.

## Licensing

Contributions are accepted under Apache-2.0. The vendored libvorbis
source under `tools/debug-libvorbis-dump/vendored-libvorbis/` keeps its
own BSD-3-Clause `COPYING`.
