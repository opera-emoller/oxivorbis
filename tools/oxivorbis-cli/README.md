# oxivorbis-cli

`oxivorbis`, a command-line Ogg Vorbis encoder built on the
[`oxivorbis`](../../README.md) library. It takes WAV or raw PCM, and its
audio is byte-identical to libvorbis 1.3.7's for the same samples. The
flags follow `oggenc` where that makes sense.

```sh
oxivorbis song.wav                        # writes song.ogg (q3)
oxivorbis -q 6 -a "Artist" -l "Album" *.wav
oxivorbis -t "One" -t "Two" one.wav two.wav  # n-th -t tags the n-th file
ffmpeg -i in.flac -f wav - | oxivorbis - > out.ogg
oxivorbis --raw -R 48000 -C 2 -F f32le - < pcm.f32 > out.ogg
oxivorbis --managed -b 128 song.wav       # average bitrate 128 kb/s
oxivorbis -q 6 -M 192 song.wav            # quality 6, at most 192 kb/s
```

## Install

```sh
cargo install --path tools/oxivorbis-cli                       # from a checkout
cargo install --path tools/oxivorbis-cli --features parallel   # multi-core per file
```

Once it is published to crates.io, `cargo install oxivorbis-cli` will
install it too. The binary is called `oxivorbis`. The minimum supported
Rust version is 1.85, the same as the library's.

### Shell completions and man page

The binary generates its own completions and man page:

```sh
oxivorbis --generate complete-bash > ~/.local/share/bash-completion/completions/oxivorbis
oxivorbis --generate complete-zsh  > ~/.zfunc/_oxivorbis    # with fpath+=(~/.zfunc)
oxivorbis --generate complete-fish > ~/.config/fish/completions/oxivorbis.fish
oxivorbis --generate man > ~/.local/share/man/man1/oxivorbis.1
```

PowerShell (`complete-powershell`) and Elvish (`complete-elvish`) are
supported as well.

## Behaviour

- **Outputs.** Each input `x.wav` goes to `x.ogg` in the same directory.
  `-o FILE` names the output (single input only), and `-o -` writes to
  stdout. Stdin input (`-`) goes to stdout by default.
- **Overwriting.** An existing output is not overwritten unless `-f` is
  given. Unlike oggenc, which overwrites silently, oxivorbis refuses by
  default. The input file is never overwritten, even with `-f`, and hard
  links and symlinks to it are caught too. Output is written to a
  temporary file and renamed into place when the encode succeeds, so a
  failed encode leaves no partial `.ogg` and keeps any old one. Devices
  such as `/dev/null` are written directly.
- **Quality.** The default is `-q 3`, oggenc's default, so `oxivorbis
  x.wav` and `oggenc x.wav` give files of about the same size and
  quality. (The library's `Options::default()` is q5; pass `-q 5` to match
  it.) Fractional values such as `-q 4.5` work.
- **Bitrate.** `-b`, `-m`, `-M`, `--managed` and
  `--advanced-encode-option` (`bit_reservoir_bits`, `bit_reservoir_bias`,
  `bitrate_average_damping`) mean what they mean to oggenc 1.4.3, and the
  output matches libvorbis set up the way oggenc sets it up: `-b N` alone
  is VBR with the setup for about N kb/s, `--managed -b N` manages the
  average, `-m`/`-M` add hard limits (and imply `--managed`), and with
  `-q` the limits apply to quality mode (`-b` is then ignored, as in
  oggenc). Unlike oggenc, `-q 6 -M 128` works even though q6 averages
  more than 128 kb/s. libvorbis has bitrate setups only for 8 to 50 kHz
  input, each rate with its own range (e.g. 45 to 500 kb/s for 44.1 kHz
  stereo); other requests fail with a message that gives the range.
- **Serial and tags.** The serial defaults to the library's fixed one, so
  output is deterministic (oggenc picks a random one). `--serial N` gives
  the n-th input `N + n`. `-c TAG=VALUE` applies to every file. The
  shortcuts `-t -a -l -N -G -d` (TITLE, ARTIST, ALBUM, TRACKNUMBER, GENRE,
  DATE) tag every file when given once, and when repeated, the n-th value
  tags the n-th input, as in oggenc. No `ENCODER=` comment is added; the
  vendor string already reads `oxivorbis <version>`.
- **Streams.** stdout carries only Ogg data. Progress (on a terminal
  only) and a one-line summary per file go to stderr: duration, rate and
  channels, q, size, kbps and encode speed. `-Q` silences both. Colors are
  used only when stderr is a terminal, and never when `NO_COLOR` is set.
  The encoder refuses to write Ogg data to a terminal.
- **Exit status.** 0 on success. 1 if any input fails (bad or truncated
  input, I/O error); the remaining inputs are still encoded. 2 for usage
  errors. A closed stdout (`oxivorbis x.wav -o - | head -c 10`) stops
  quietly with status 0.
- **Memory.** Input is streamed in 64 KiB reads, so memory stays bounded
  for any input length, stdin included.

## Input formats

WAV (`RIFF`, `RF64`/`BW64`, plain or `WAVE_FORMAT_EXTENSIBLE` with the PCM
or IEEE-float sub-format GUID), mono or stereo, at any sample rate the
library supports. Chunks other than `fmt `/`ds64`/`data` (`LIST`, `fact`,
`JUNK`, ...) are skipped, odd chunk sizes included. A `data` size of 0 or
0xFFFFFFFF, which is what streamed WAVs such as `ffmpeg ... -f wav -`
write, means "read to end of input". A file shorter than its `data` chunk
says is reported as truncated (exit 1, no output file).

| Format | Encoder call | Conversion |
|---|---|---|
| 16-bit PCM | `Encoder::write` (i16) | none (bit-exact with `oxivorbis::encode`) |
| 8-bit unsigned PCM | `write_f32` | `(x - 128) / 128` (exact) |
| 24-bit PCM | `write_f32` | `x / 2^23` (exact: 24 bits fit an f32) |
| 32-bit PCM | `write_f32` | `x / 2^31`, exact in f64, rounded once to f32 |
| 32-bit float | `write_f32` | passed through unchanged |
| 64-bit float | `write_f32` | rounded once to f32 |

Integer samples narrower than their container (for example 20 valid bits
in 24) are left-justified by the WAV spec, so they're scaled by the
container width. `--raw` takes the same formats as headerless
little-endian PCM: `-F u8|s16le|s24le|s32le|f32le|f64le`.

The following are rejected with an error that names what was found:
compressed WAV formats (ADPCM, A-law/mu-law, MP3, ...), other extensible
sub-formats, more than two channels, and unsupported rates. FLAC and MP3
input aren't supported directly; decode them first, for example with
`ffmpeg -i in.flac -f wav - | oxivorbis -`.

The older benchmark form `oxivorbis [--stream] <rate> <channels>
[quality]` (raw s16le stdin to stdout, q5) still works for `just bench`.

## Code layout

`src/codec.rs` is the only module that calls the library API. The other
modules work with plain `u32` rates, `u16` channel counts and byte
slices. `src/wav.rs` is the streaming header parser, `src/pcm.rs` does
the sample conversion, `src/args.rs` holds the clap definition and help
text, and `src/job.rs` covers a single input-to-output encode. The tests
in `tests/cli.rs` run the real binary on generated WAVs in every format.
They check the output byte for byte against the library's
`encode`/`encode_f32` and decode it with lewton.
