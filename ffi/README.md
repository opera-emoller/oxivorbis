# oxivorbis C API

`liboxivorbis` exposes the oxivorbis encoder to C, C++ and anything with a
C FFI (Python, C#, Swift, Godot, Unity, ...). Output is byte-identical to
the Rust API, and therefore to libvorbis 1.3.7. The header is
[`include/oxivorbis.h`](include/oxivorbis.h) (C99, usable from C++).

## Build

Prebuilt archives for macOS (arm64, x86_64), Linux (x86_64, arm64; glibc
2.35+) and Windows (x64, MSVC) are on the
[releases page](https://github.com/opera-emoller/oxivorbis/releases):
`include/`, the shared and static libraries, a relocatable pkg-config
file (Unix) and a CMake package (serial build). Or build it yourself:

```sh
cargo build --release -p oxivorbis-ffi                      # serial
cargo build --release -p oxivorbis-ffi --features parallel  # multi-core
```

| Platform | Shared library | Static library |
|---|---|---|
| Linux, BSD | `target/release/liboxivorbis.so` (soname `liboxivorbis.so`) | `liboxivorbis.a` |
| macOS, iOS | `target/release/liboxivorbis.dylib` (install name `@rpath/liboxivorbis.dylib`) | `liboxivorbis.a` |
| Windows (MSVC) | `oxivorbis.dll` + import library `oxivorbis.dll.lib` | `oxivorbis.lib` |
| Windows (GNU) | `oxivorbis.dll` + import library `liboxivorbis.dll.a` | `liboxivorbis.a` |

Or install the header, both libraries, a pkg-config file and a CMake
package under a prefix (Unix):

```sh
just ffi-install /usr/local         # or: ffi/pkg/install.sh /usr/local
```

On macOS the installed copy gets an absolute install name, so programs
linked against it run without an rpath. The build-tree copy keeps
`@rpath/liboxivorbis.dylib` for bundling inside an app. Link such apps
with `-Wl,-rpath,@executable_path/../Frameworks` or wherever the dylib
goes. The soname is unversioned while the ABI is 0.x. Packagers who want
`liboxivorbis.so.N` can relink with their own `-Wl,-soname`.

## Link

```sh
# shared, from the build tree
cc app.c -Iffi/include -Ltarget/release -loxivorbis -Wl,-rpath,"$PWD/target/release"

# static: add the system libraries the Rust standard library needs
cc app.c -Iffi/include target/release/liboxivorbis.a -lpthread -ldl -lm   # Linux
cc app.c -Iffi/include target/release/liboxivorbis.a                      # macOS

# installed
cc app.c $(pkg-config --cflags --libs oxivorbis)
cc app.c $(pkg-config --cflags --libs --static oxivorbis)   # static
```

`cargo rustc -p oxivorbis-ffi --release --crate-type staticlib -- --print native-static-libs`
prints the exact static link line for your target. On MSVC it is
typically `kernel32 advapi32 ntdll userenv ws2_32 dbghelp`.

CMake, after `just ffi-install <prefix>`:

```cmake
find_package(oxivorbis REQUIRED)    # -DCMAKE_PREFIX_PATH=<prefix>
target_link_libraries(app PRIVATE oxivorbis::oxivorbis)        # or oxivorbis::oxivorbis_static
```

## API

| | |
|---|---|
| Versioning | `OXV_ABI_VERSION` (header), `oxv_abi_version()`, `oxv_version()` (`"0.1.0"`) |
| Errors | `oxv_status`: `OXV_STATUS_OK` (0), `_INVALID_ARGUMENT`, `_UNSUPPORTED`, `_IO`, `_INVALID_STATE`, `_PANIC`; `oxv_status_message(status)` |
| Options | `oxv_options_new`, `_set_quality(float)`, `_set_serial(uint32_t)`, `_set_vendor(utf8)`, `_add_comment(key, value)`, `_free` |
| Bitrate modes | `oxv_options_set_bitrate(opts, mode, kbps, min_kbps, max_kbps)`, `oxv_options_set_bitrate_tuning(opts, reservoir_bits, bias, damping)`, `oxv_bitrate_supported_kbps(rate, channels, &min, &max)`, `oxv_options_check(opts, rate, channels, msg, size)` |
| Streaming | `oxv_encoder_new(rate, channels, opts, &enc)`, `oxv_encoder_write_{i16,f32}`, `oxv_encoder_write_planar_{f32,i16}`, `oxv_encoder_finish`, `oxv_encoder_free` |
| One-shot | `oxv_encode_{i16,f32}(pcm, samples, rate, channels, opts, &buf)`, `oxv_buffer_free` |
| Callback sink | `oxv_writer_new(rate, channels, opts, write_fn, user_data, &w)`, `oxv_writer_write_{i16,f32}`, `oxv_writer_write_planar_{f32,i16}`, `oxv_writer_finish`, `oxv_writer_free` |

The header documents each function. A few rules apply throughout:

- Sample rate is a plain `uint32_t` from `OXV_SAMPLE_RATE_MIN` (1) to
  `OXV_SAMPLE_RATE_MAX` (200000). The channel count is a plain `uint32_t`
  from 1 to `OXV_CHANNELS_MAX` (255), in **Vorbis channel order** (5.1:
  front left, center, front right, rear left, rear right, LFE; see the
  [reference](../docs/reference.md#multichannel-and-51)). The library never
  reorders channels; WAV/SMPTE-ordered input needs reordering first.
  Anything else returns `OXV_STATUS_UNSUPPORTED`.
- Interleaved functions count **samples** (frames × channels). A partial
  trailing frame carries over to the next write, as in the Rust API.
  Planar functions take `const T *const *channels`, `channel_count` (which
  must equal the encoder's) and `frames`.
- `NULL` options means the defaults: quality 5, a fixed serial so output is
  deterministic, vendor `"oxivorbis <version>"`, no comments. A NULL
  sample pointer is allowed when the count is 0. Any other NULL argument
  returns `OXV_STATUS_INVALID_ARGUMENT`. So does a NaN or out-of-range
  quality, a string that isn't UTF-8, or a misaligned buffer.
- When a call fails, its out-pointers are reset to NULL / 0 and the
  encoder is unchanged. The exceptions are `OXV_STATUS_IO` and
  `OXV_STATUS_PANIC`, after which the encoder or writer only accepts
  `*_free`. Later calls return `OXV_STATUS_INVALID_STATE`.
- No Rust panic crosses into C. Each entry point catches it and returns
  `OXV_STATUS_PANIC`. That is a backstop only: the encoder is designed
  never to panic.

### Ownership

| Object | Created by | Released by | Notes |
|---|---|---|---|
| `oxv_options *` | `oxv_options_new` | `oxv_options_free` | Copied by every function that takes it, so it can be freed or reused right after |
| `oxv_encoder *` | `oxv_encoder_new` | `oxv_encoder_free` | Always free it, finished or not. `finish` does not free |
| bytes from `oxv_encoder_write_*` | the encoder | (owned by the encoder) | Valid until the next call that takes that encoder |
| bytes from `oxv_encoder_finish` | the encoder | `oxv_encoder_free` | Valid until the encoder is freed |
| `oxv_buffer` from `oxv_encode_*` | the library | `oxv_buffer_free(&buf)` | Never `free()` it; don't modify its fields. Freeing twice is harmless (it resets to `{NULL, 0}`) |
| `oxv_writer *` | `oxv_writer_new` | `oxv_writer_free` | Always free it, finished or not |
| version / status strings | static | never | |

Every `*_free` accepts NULL.

### Threads

Every function is reentrant, with no global state. An `oxv_encoder` or
`oxv_writer` may be created on one thread and used or freed on another,
but must not be used from two threads at once. Lock it yourself if you
share it. Separate encoders run fully in parallel. Options may be read
from several threads at once (e.g. passed to several `oxv_encoder_new`
calls) as long as nobody changes them meanwhile. A writer's callback
runs on whichever thread calls into the writer, and must not call back
into that writer.

## Examples

### C, streaming

```c
#include <oxivorbis.h>
#include <stdio.h>

int encode_file(FILE *out, const int16_t *pcm, size_t frames) {
    oxv_options *opts = oxv_options_new();
    oxv_options_set_quality(opts, 6.0f);
    oxv_options_add_comment(opts, "TITLE", "Example");

    oxv_encoder *enc;
    oxv_status st = oxv_encoder_new(44100, 2, opts, &enc);
    oxv_options_free(opts);                 /* copied by the encoder */
    if (st != OXV_STATUS_OK) {
        fprintf(stderr, "oxivorbis: %s\n", oxv_status_message(st));
        return -1;
    }
    const uint8_t *bytes;
    size_t len;
    for (size_t i = 0; i < frames * 2 && st == OXV_STATUS_OK; i += 4096) {
        size_t n = frames * 2 - i < 4096 ? frames * 2 - i : 4096;
        st = oxv_encoder_write_i16(enc, pcm + i, n, &bytes, &len);
        if (st == OXV_STATUS_OK) fwrite(bytes, 1, len, out);  /* valid until the next call */
    }
    if (st == OXV_STATUS_OK && (st = oxv_encoder_finish(enc, &bytes, &len)) == OXV_STATUS_OK)
        fwrite(bytes, 1, len, out);
    oxv_encoder_free(enc);
    return st == OXV_STATUS_OK ? 0 : -1;
}
```

### C, one-shot and callback

```c
oxv_buffer ogg;
if (oxv_encode_f32(samples, n, 48000, 1, NULL, &ogg) == OXV_STATUS_OK) {
    fwrite(ogg.data, 1, ogg.len, f);
    oxv_buffer_free(&ogg);
}

static int to_file(void *user, const uint8_t *data, size_t len) {
    return fwrite(data, 1, len, (FILE *)user) == len ? 0 : -1;
}
oxv_writer *w;
oxv_writer_new(44100, 2, NULL, to_file, f, &w);
oxv_writer_write_planar_f32(w, (const float *const[]){left, right}, 2, frames);
oxv_writer_finish(w);   /* OXV_STATUS_IO if to_file failed */
oxv_writer_free(w);
```

### Bitrate modes

VBR at the quality is the default. `oxv_options_set_bitrate` selects one
of libvorbis's bitrate-driven setups instead, each as oggenc sets it up
(`OXV_BITRATE_MODE_NOMINAL` = `-b K`, `_AVERAGE` = `--managed -b K [-m]
[-M]`, `_LIMITS` = `--managed -m/-M`, `_QUALITY_LIMITS` = `-q Q -m/-M`;
0 for an unset limit). libvorbis has bitrate setups only for 8 to 50 kHz
(for 6 channels, up to 70 kHz), each for a range of bitrates, so the
request is checked when the encoder, writer or one-shot encode is
created: those return `OXV_STATUS_UNSUPPORTED`, and `oxv_options_check`
explains why. The same options can be used at any rate and channel
count; each is checked for its own.

```c
oxv_options *opts = oxv_options_new();
/* oggenc --managed -b 384: 5.1 at an average 384 kb/s */
oxv_options_set_bitrate(opts, OXV_BITRATE_MODE_AVERAGE, 384, 0, 0);
char why[256];
if (oxv_options_check(opts, 48000, 6, why, sizeof why) != OXV_STATUS_OK) {
    fprintf(stderr, "oxivorbis: %s\n", why);   /* e.g. "no libvorbis setup for ..." */
} else {
    oxv_encoder *enc;
    oxv_encoder_new(48000, 6, opts, &enc);     /* interleaved L C R RL RR LFE */
    /* ... oxv_encoder_write_i16 / finish / free as above ... */
}
oxv_options_free(opts);
```

### Python (ctypes, no dependencies)

[`bindings/python/oxivorbis.py`](bindings/python/oxivorbis.py) is a small
wrapper. Copy it next to your code and point `OXIVORBIS_LIB` at the
library, or keep it in this repository, where it finds `target/`.

```python
import oxivorbis

ogg = oxivorbis.encode_i16(samples, rate=44100, channels=2, quality=5,
                           comments=[("TITLE", "Song")])

with oxivorbis.Encoder(48000, 1, quality=3) as enc, open("out.ogg", "wb") as f:
    for chunk in chunks:                 # lists, array('f'), numpy float32, ...
        f.write(enc.write_f32(chunk))
    f.write(enc.finish())

# oggenc --managed -b 128 -m 128 -M 128 ("CBR"); errors carry the reason
cbr = oxivorbis.Options(bitrate=(oxivorbis.BITRATE_AVERAGE, 128, 128, 128))
ogg = oxivorbis.encode_i16(samples, rate=44100, channels=2, options=cbr)
```

### C# (P/Invoke)

```csharp
using System.Runtime.InteropServices;

static unsafe class Oxv {
    const string Lib = "oxivorbis";   // oxivorbis.dll / liboxivorbis.so / .dylib

    [StructLayout(LayoutKind.Sequential)]
    public struct Buffer { public byte* Data; public nuint Len; }

    [DllImport(Lib)] public static extern int oxv_encode_i16(
        short* pcm, nuint samples, uint rate, uint channels, IntPtr opts, out Buffer buf);
    [DllImport(Lib)] public static extern void oxv_buffer_free(ref Buffer buf);
    [DllImport(Lib)] public static extern IntPtr oxv_status_message(int status);

    public static byte[] Encode(short[] pcm, uint rate, uint channels) {
        fixed (short* p = pcm) {
            int st = oxv_encode_i16(p, (nuint)pcm.Length, rate, channels, IntPtr.Zero, out var buf);
            if (st != 0) throw new Exception(Marshal.PtrToStringAnsi(oxv_status_message(st)));
            try { return new ReadOnlySpan<byte>(buf.Data, (int)buf.Len).ToArray(); }
            finally { oxv_buffer_free(ref buf); }
        }
    }
}
```

Unity loads the same native plugin: put the platform libraries under
`Assets/Plugins/` and use `"oxivorbis"` (or `"__Internal"` for a static
library on iOS). Godot GDExtension and Swift can include
`oxivorbis.h` directly. In Swift, use a module map or a bridging header
and link `liboxivorbis.a`.

## Tests

```sh
just ffi-test        # C (shared + static) and C++ against the library; Python against the CLI
just ffi-sanitize    # the C test under AddressSanitizer, then leaks (macOS) / valgrind (Linux)
just ffi-header      # regenerate include/oxivorbis.h after changing ffi/src (cbindgen 0.29.4)
```

`tests/c_api.rs` builds the library and compiles
[`tests/c/test_oxivorbis.c`](tests/c/test_oxivorbis.c) with
`-std=c99 -Wall -Wextra -Werror -pedantic`. The C test encodes one input
through every encoding function and writes each stream out, and the Rust
side checks each one byte-for-byte against `oxivorbis::encode` /
`encode_f32` (`encode_with_bitrate` for the bitrate modes), including 5.1
streams and managed stereo and 5.1. The C test also walks the error
paths, including one set of bitrate options used at a rate where it is
valid and one where it isn't. CI runs all of it on
Linux and macOS, checks the committed header is fresh, and runs the
Python test.

## ABI policy

`OXV_ABI_VERSION` is bumped on any incompatible change. Adding functions,
options setters or `oxv_status` values is compatible, so treat unknown
non-zero statuses as errors. Every struct except `oxv_buffer` is opaque,
so new settings arrive as new `oxv_options_set_*` functions (as the
bitrate modes did), and channel layouts need no new types.
