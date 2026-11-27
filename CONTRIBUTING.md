# Contributing to oxivorbis

oxivorbis's one hard requirement is byte-for-byte parity with libvorbis
1.3.7. Any change to the encoder has to keep every parity test green on
every platform. This document covers how that is checked and how to
work on the code.

## Setup

```sh
brew install just vorbis-tools ffmpeg hyperfine   # macOS
sudo apt install just vorbis-tools ffmpeg         # Debian/Ubuntu
just setup               # installs cargo-nextest and the pre-commit hook
just oracles             # builds the C oracles (needed for --features oracle)
```

`just oracles` runs `tools/oracle-encoder/build.sh`,
`build_quality_dumpers.sh` and `build_dsp_oracle.sh`. They need only a
C compiler: libvorbis and libogg are compiled from the vendored sources,
so no system libogg or libvorbis is involved.

The pre-commit hook (`.githooks/pre-commit`) runs `cargo fmt --check`
and `cargo clippy --all-targets -D warnings` whenever a commit touches
`.rs` files. `just verify` runs fmt, clippy and the tests.

## The oracle

`tools/oracle-encoder/oracle-encoder` is a small C program
(`tools/oracle-encoder/main.c`). It is statically built from two
pristine upstream trees:

| Directory                | Contents | Source tarball (sha256) |
|--------------------------|----------|-------------------------|
| `tools/libvorbis-1.3.7/` | `lib/**/*.{c,h}` (incl. `modes/`, `books/`), `include/`, `COPYING` | `libvorbis-1.3.7.tar.gz` (`0e982409a9c3fc82ee06e08205b1355e5c6aa4c36bca58146ef399621b0ce5ab`) |
| `tools/libogg-1.3.5/`    | `src/{framing,bitwise}.c`, `src/crctable.h`, `include/ogg/{ogg,os_types}.h`, `COPYING` | `libogg-1.3.5.tar.xz` (`c4d91be36fc8e54deae7575241e03f4211eb102afb3fc0775fbbc1b740016705`) |

Both trees are unmodified. Each has a `SHA256SUMS` manifest of its
files. `tools/oracle-encoder/common.sh`, which all the oracle build
scripts share, checks the manifests and rejects stray files before
compiling anything. libogg is pinned along with libvorbis because page
boundaries come from libogg's `ogg_stream_pageout`.

`build.sh` compiles with `-O0 -ffp-contract=off`. That rules out FMA
contraction and makes the output bit-deterministic across hosts. The
only deviations from upstream are applied from outside the trees:

- `-include stable_qsort.h` redirects `qsort` to a stable merge sort.
  libc `qsort` breaks ties differently on glibc and macOS, and
  libvorbis's noise normalization (q0 to q3) sorts with ties.
- `tools/oracle-encoder/include/ogg/config_types.h` stands in for the
  header that libogg's `configure` would generate.

The oracle reads s16le (or f32 LE with the `f32` argument) from stdin
and takes `<rate> <channels> [base_quality [s16|f32 [comment]]]`. It
feeds libvorbis in 64-frame `vorbis_analysis_wrote` chunks and drains
blocks after every call. oxivorbis's output is defined to match this. The
oracle writes nothing to stderr unless it fails.

`dump_params`, `dump_quality_tables`, `dsp_oracle`,
`tools/gen-setup-templates` and `tools/gen-vectors-mdct` are built from
the same trees with the same flags.

### Instrumented copy (debugging only)

`tools/debug-libvorbis-dump/vendored-libvorbis/` is a separate,
modified copy of libvorbis. It adds debug `fprintf`s, dumps to
`/tmp/oxivorbis-debug`, `getenv` hooks and a few post-1.3.7 upstream fixes
(psy.c's `inthalfoc` clamp and `<<16` to `*(1<<16)`, floor1.c's `val<<1`
to `val*2`, and small sharedbook.c, os.h and smallft.c changes). Only
the `debug-*` tools use it. It is never used to build an oracle.

### Known upstream issue: the psy.c `inthalfoc` read

Pristine 1.3.7 `_vp_psy_init` reads `noiseoff[j][inthalfoc+1]` with
`inthalfoc == P_BANDS-1` for the top bins at 44.1 and 48 kHz. Upstream
later clamped this. The read is out of bounds of the inner `float[17]`
array, but stays inside `vorbis_info_psy`: it lands on
`noiseoff[j+1][0]`, or on `noisecompand[0]` for the last curve. Those
are finite values, and their interpolation weight `del` is exactly 0,
so the result is bit-identical to the clamped version. An
AddressSanitizer build of the oracle runs the whole parity suite and
the fuzzer clean. UBSan's `-fsanitize=array-bounds` does flag the
intra-object index, as expected. UBSan also reports psy.c:319,
`((lo-1)<<16)` for the lowest bins, where `lo-1` is negative, as a left
shift of a negative value. Upstream later wrote it `*(1<<16)`; gcc and
clang compute the same two's-complement value either way. CI runs a
fuzz smoke against an ASan oracle (see below). To build one locally:

```sh
OUT=/tmp/oracle-asan LW_EXTRA_CFLAGS="-g -fsanitize=address" ./tools/oracle-encoder/build.sh
cargo run -p parity-fuzz --release -- --oracle /tmp/oracle-asan --iters 2000 --quality -0.1
```

## How parity is enforced

`tests/parity.rs` (behind the `oracle` feature) byte-diffs oxivorbis's
output against two reference encoders:

- **`oracle_parity_*`**: the authoritative tests, run against the
  in-tree oracle.
- **`parity_*`**: the same comparison against the system `oggenc`
  (vorbis-tools). Only the silence cases are enabled. oggenc uses a
  different chunk size, and packaged libvorbis builds are optimized and
  may use FMA, so tone-rich inputs can legitimately differ. Those cases
  are `#[ignore]`d and point to their `oracle_parity_*` counterparts.
  `parity_oggenc_quality_conversion` pins the oggenc `-q` →
  `base_quality` float conversion against the system `oggenc` (CI
  checks for vorbis-tools 1.4.x and libvorbis 1.3.x).

The quality matrix is
`oracle_parity_q::<quality>::<config>::<signal>`. It covers one
`base_quality` per libvorbis template interval (0.05 … 0.95, plus 0.0,
0.5 and 1.0) and the q-1 range (-0.1, -0.08, -0.05, -0.01, -0.0000001),
crossed with 44.1/48 kHz, mono/stereo, and five signals: silence, sine,
ramp, noise+tone and 10 s of transients.

`oracle_parity_rate::*` covers the other libvorbis template families
with the same five signals, mono and stereo:

- `family::<rate>`: one rate per family (4, 8, 11.025, 16, 22.05, 32 and
  96 kHz) at eight qualities spanning each family's quality mapping,
  ends included (`base_quality` -0.1, -0.05, 0, 0.05, 0.25, 0.5, 0.75, 1).
- `edge::<rate>`: both ends of every template's rate range and the rates
  either side of them (7999/8001, 8999/9000/9001, 14999/15000/15001, …,
  49999/50000/50001, 199999/200000), 1, 100 and 1000 Hz, and odd rates
  (37800, 44056, 88200, 192000), at q-1, q5 and q10.

`oracle_parity_long::cases::*` covers long inputs: 30 to 120 s of
never-silent noise+tone, transients and 20 Hz–20 kHz chirps, across all
four configs at q-1, q0, q2, q5 and q10. `oracle_parity_long_silence_*`
runs 100 to 120 s of silence, which crosses libogg's 255-segment forced
flushes.

`oracle_parity_short_input::*` sweeps short inputs. At q5 and q2 it
covers 0..2100 frames, around the 2048-sample long block. In the q-1
range (512/4096 blocks; `base_quality` -0.1 mono 44.1 kHz and -0.05
stereo 48 kHz) it covers 0..8400 frames, with ±1 around every block and
chunk boundary (4095–4097, 4159–4161, 8191–8193, …). The same sweeps run
for the single-block templates (512/512 at 8 and 11.025 kHz, 1024/1024 at
16 kHz q-1) and the 512/1024 ones (16 and 22.05 kHz).

`oracle_parity_fuzz_repros` replays the minimized fuzzer failures kept
in `tests/vectors/fuzz/`. File names are parity-fuzz case ids,
`<id>-q<quality>[-f32]-<rate>-<ch>ch.{s16le,f32le}`. An untagged name
fails the test unless it is in `LEGACY_UNTAGGED_REPROS`, which lists
the three pre-tagging repros (q5).

`tests/parity_f32.rs` checks `f32` input against the oracle's f32 mode.
It covers several qualities and configs, values beyond ±1.0,
subnormals, `-0.0`, NaN/inf (encoded as 0.0) and odd-sized chunked
writes.

`tests/golden.rs` compares oxivorbis against committed oracle output
(`tests/vectors/golden/*.ogg`) for six integer-generated signals,
including q-1, q10 and f32 input. It needs no oracle at run time, so it
runs in every `cargo test` and on wasm. With `--features oracle`,
`golden_vectors_are_fresh` also checks the vectors against a fresh
oracle run. `just regen-golden-vectors` rewrites them.

`tests/streaming.rs` checks four things:

- any chunking of `write` / `write_f32` gives the one-shot bytes;
- pages are emitted before `finish`;
- the analysis buffer stays bounded over 10 minutes of input;
- with `parallel`, the batch size never changes the output.

Its long-input tests (`chunking_invariance_10s`,
`streaming_emits_pages_before_finish`,
`buffer_is_bounded_over_10_minutes`) are `#[ignore]`d in debug builds.
`just test-release` runs them, and so does CI.

Inside `src/`, the oracle-gated unit tests are:

- `quality_oracle_grid`: setup parameters versus libvorbis, for every
  quality, at 45 rates covering every template and range edge, mono and
  stereo (about 110,000 configurations).
- `oracle_setup_header_sweep`: setup headers at 26 rates, mono and
  stereo.
- `dsp_manifest_*`: MDCT, FFT and window outputs against hashes
  produced by `tools/oracle-encoder/dsp_oracle`. These tests always run.

`psy::tests::psy_setup_robust_to_libm_ulps` (always runs) shifts every
`atan`/`log`/`exp` result the psy setup uses by ±2 ulps and checks that
the ATH, octave, bark-window, noise-offset and tone-curve geometry stays
bit-identical, at ~240 rates. Its `#[ignore]`d sibling
`psy_setup_robust_to_libm_ulps_every_rate` does the same (±1 ulp) for
every rate from 1 to 200000 Hz in about a minute (release):
`cargo nextest run --release --lib --run-ignored only psy_setup_robust`.

`tests/psy.rs`'s `psy_compute_mask_determinism` is a oxivorbis snapshot,
not an oracle vector. It pins the mask bits across hosts (including
wasm), and the build fails if the snapshot file is missing. Re-bless it
deliberately with `OXIVORBIS_BLESS=1`.

Integration tests reach a few unstable internals (header writers, exact
`base_quality` entry points, some DSP modules, test hooks) through the
undocumented `__internal` feature. The crate enables that feature for
its own tests through a path-only self dev-dependency, so no extra
flags are needed. Cargo strips that dev-dependency when the crate is
packaged.

## Running tests

```sh
cargo nextest run                               # unit + integration (no oracle)
cargo nextest run --features oracle             # everything, incl. parity vs oracle + oggenc
cargo nextest run --features oracle parity_     # just the parity tests
just parity                                     # parity for the serial AND parallel paths
just test-release                               # the release-only streaming tests
cargo test --doc                                # doctests (incl. README code blocks)
just wasm-test                                  # wasm32-wasip1 under wasmtime
```

`.config/nextest.toml` sets the timeouts. Tests are killed after 60 s by
default. The long parity tests and short-input sweeps get 4 minutes,
the chunking-invariance tests 20 minutes, and the manual corpus sweep
has no limit.

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
with ffmpeg, so ffmpeg must be on `PATH`. It then byte-compares oxivorbis
against the in-tree oracle-encoder, running in parallel across all
cores. If there is no corpus, it prints a loud warning that nothing was
tested and passes. Set `CORPUS_REQUIRE=1` to make that an error. CI
doesn't run the sweep. The knobs can be set as environment variables or
as positional arguments to `just corpus`:

| Variable         | Example                         | Default    | Meaning |
|------------------|---------------------------------|------------|---------|
| `CORPUS_LIMIT`   | `100`                           | all files  | Test only the first N files (sorted) |
| `CORPUS_QUALITY` | `-0.1,0.5,1.0`                  | `0.5`      | libvorbis `base_quality` list |
| `CORPUS_CONFIGS` | `44100x2,22050x1`, or `all`     | `44100x2`  | Rate × channel configurations (any rate 1..=200000; `all` = 44.1/48 kHz × mono/stereo) |
| `CORPUS_JOBS`    | `4`                             | all cores  | Worker threads |
| `CORPUS_REQUIRE` | `1`                             | unset      | Fail instead of warning when there is no corpus |

Positional order: `just corpus <limit> <extra-features> <quality> <configs> <jobs>`.
Use `""` to skip an argument.

Failing inputs are saved under
`target/corpus-failures/<config>-q<q>/<relpath>/`. Each one holds the
s16le input, both `.ogg` outputs, and a descriptor with a
`parity-fuzz --pcm` replay command.

The latest full sweep ran against the pristine oracle. It covered a
6653-file corpus in all four configurations at q-1, q5 and q10, and all
79,836 encodes were byte-identical (about 10 minutes on a 10-core M4).
Earlier sweeps, against the instrumented build, also covered
44.1 kHz stereo at q0, q2, q3 and q8.

## Differential fuzzer

`tools/parity-fuzz` generates seeded synthetic signals and encodes each
one with both oxivorbis and the oracle. oxivorbis streams each case through
`Encoder` in pseudo-random chunk sizes, so every case also checks that
chunking doesn't change the output. On any divergence or panic, the
fuzzer saves the case and a minimized reproduction under
`target/parity-fuzz/<id>/`. Case ids are
`s<seed>-i<iter>-q<quality>[-f32]-<rate>-<ch>ch`.

```sh
cargo run -p parity-fuzz --release -- --iters 10000 --seed 1 --quality 0.5
#   --quality Q          libvorbis base_quality (-0.1..=1.0)
#   --f32                feed f32 (write_f32 vs the oracle's f32 mode), incl.
#                        values up to ±1e4, subnormals and -0.0
#   --rates 44100,48000  --channels 1,2  --jobs J  --max-secs T  --max-len SECS
#   --keep-going         collect all failures instead of stopping at the first
#   --minimize-per-sig N only minimize the first N failures per signature
#   --no-minimize        --start I  --min-frames N  --out DIR  --oracle PATH

# Replay a saved raw file (one rate / channel count). The descriptor.json
# of every saved case contains the exact command, including --quality:
cargo run -p parity-fuzz --release -- --pcm input.s16le --rates 44100 --channels 2 --quality 0.5
```

The generators use no host libm. `tools/parity-fuzz/src/det.rs`
implements sin, cos, exp, ln and pow with plain IEEE arithmetic, so a
seed renders the same PCM on every platform.

Against the pristine oracle, 20,000 s16 cases at each of q-1, q2, q5
and q10, 10,000 f32 cases at each of the same qualities, and 1,500
cases of up to 30 s at each of q-1, q5 and q10 all ran clean. When the
fuzzer finds a failure, copy the minimized `input.*le` into
`tests/vectors/fuzz/` under its case id (for example
`s1-i42-q0.5-44100-2ch.s16le`) so that `oracle_parity_fuzz_repros`
keeps it fixed.

## Debugging a divergence

- `cargo run -p parity-diff -- oxivorbis.ogg oracle.ogg` parses both Ogg
  files strictly and verifies every page CRC. It then compares them page
  by page (flags, granule, serial, sequence number, CRC, lacing, body)
  and packet by packet, and reports the first divergence at each level.
  It exits 0 only if the files are identical, 1 on a difference and 2 on
  a parse or CRC error. `--ignore-serial` skips the serial and CRC
  equality. Use it on the two `.ogg` files saved for a corpus or fuzz
  failure.
- `just debug-diff` builds `tools/debug-libvorbis-dump` (the
  instrumented copy, with `$CC` and the vendored libogg), dumps the
  first short block of a 440 Hz sine to `/tmp/oxivorbis-debug/c_*`, writes
  the matching `/tmp/oxivorbis-debug/r_*` from oxivorbis
  (`OXIVORBIS_DEBUG_DUMP=1`), and compares the two layer by layer
  (windowed PCM, FFT, logfft, mask, floor posts, floor bits). `just
  debug-dump-c` and `just debug-dump-rust` run the halves separately.
- `just bench [input] [parallel] [quality]` times oxivorbis against
  `oggenc` with hyperfine, using `tools/oxivorbis-cli`. `just profile
  [input]` records a samply profile.

The usual culprits are:

- f32 being promoted to f64, or the reverse;
- `rint` versus `.round()`;
- operation order in sums;
- libvorbis's IEEE_FLOAT32 `todB` inline.

The commit log records the precision pitfalls already hit.

## Regenerating committed tables

All tables come from the vendored libvorbis, never from Rust-side
`sin`/`cos`, so they are host-independent. `just check-fresh` (and CI,
on Linux/gcc and macOS/clang) runs `tools/oracle-encoder/check_fresh.sh`.
It regenerates every file in the first six rows below and fails if any
differs from what is committed. The golden vectors are checked by the
`golden_vectors_are_fresh` test instead.

| Recipe                       | Regenerates |
|------------------------------|-------------|
| `just regen-tables`          | `src/tables/{trig,drft,window}.rs` (MDCT, FFT, window tables) via `dsp_oracle` + `gen-tables` |
| `just regen-dsp-manifest`    | `tools/oracle-encoder/dsp_manifest.txt` (hashes checked by `dsp_manifest_*`) |
| `just regen-quality-tables`  | `src/tables/quality.rs` (mode-template tables of every 1–2 channel template) |
| `just regen-setup-templates` | `src/setup_books.bin` and `src/setup_templates.rs` (codebooks, floor/residue/mapping templates) |
| `just regen-header-vectors`  | `tests/vectors/headers/*.bin`, `tests/vectors/ogg/*` (from the oracle-encoder) |
| `just regen-mdct-vectors`    | `tests/vectors/mdct/*.bin` |
| `just regen-golden-vectors`  | `tests/vectors/golden/*.ogg` |

### Updating the vendored sources

Don't edit files under `tools/libvorbis-1.3.7/` or `tools/libogg-1.3.5/`;
the build refuses to run if they don't match their `SHA256SUMS`. To
re-vendor, extract the official tarball, verify its sha256 against the
table above (or Xiph's `SHA256SUMS`), copy the listed files, and
regenerate the manifest from inside the directory:

```sh
find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort | xargs shasum -a 256 > SHA256SUMS
```

## CI

`.github/workflows/ci.yml` runs four jobs. Estimated wall times on
GitHub's standard runners are in brackets.

- **ubuntu** (~20–25 min) installs ffmpeg and vorbis-tools, and checks
  for vorbis-tools 1.4.x and libvorbis 1.3.x (oggenc is only used for
  the `parity_*` tests). It builds the C oracles from the vendored
  sources and runs `check_fresh.sh`. It runs fmt and clippy (default,
  `parallel`, `oracle,parallel` and the whole workspace). A guard step
  asserts a minimum test count for every oracle-gated group, so a cfg
  or rename slip can't silently drop tests. Then come the test suite
  without the oracle and with it, each on both the serial and parallel
  paths; the release-only streaming tests [~2 min, mostly the release
  build]; doctests; and fixed-seed parity-fuzz smokes [~2 min total].
  The smokes are s16 at five qualities, f32 at three, inputs up to 30 s
  at q-1 and q10, and a run against an AddressSanitizer-built oracle.
- **macos** (~15–20 min) does the same build, freshness check, oracle
  suites, release streaming tests and fuzz smokes on Apple silicon with
  clang and macOS libm.
- **msrv** (~2 min) runs `cargo +1.85 check --all-targets --features
  parallel`.
- **wasm** (~5 min) builds for `wasm32-unknown-unknown`. It also runs the
  unit tests, header/MDCT vectors, smoke tests, psy snapshot and
  `tests/golden.rs` on `wasm32-wasip1` under wasmtime, which checks
  parity under wasm's own libm and codegen.

Parity has also been verified by hand on Windows x86_64 (MSVC). Rust
output there is identical.

## Packaging

The published crate contains only `src/`, `examples/`, the DSP manifest
its unit tests `include_str!`, and the top-level docs and licenses (see
`include` in `Cargo.toml`). Check the contents with
`cargo package --list` and `cargo publish --dry-run`.

## Licensing

Contributions are accepted under Apache-2.0. The vendored libvorbis
(`tools/libvorbis-1.3.7/`, `tools/debug-libvorbis-dump/vendored-libvorbis/`)
and libogg (`tools/libogg-1.3.5/`) sources keep their own BSD-3-Clause
`COPYING` files. See `NOTICE`.
