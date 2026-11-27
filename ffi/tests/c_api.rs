//! Drives the C API from C and C++ through the built library, as real
//! callers do: compiles `tests/c/test_oxivorbis.c` against
//! `include/oxivorbis.h` and liboxivorbis (shared, then static), runs it,
//! and checks every stream it wrote is byte-identical to `oxivorbis::encode`
//! / `encode_f32` on the same input and options. Also compiles and runs
//! `tests/c/smoke.cpp` with `-Wall -Wextra -Werror -pedantic`.
//!
//! Environment:
//!
//! - `CC`, `CXX`: compilers (default `cc`, `c++`; `clang`, `clang++` on
//!   Windows, where the MSVC-targeting clang driver accepts these flags).
//! - `OXV_FFI_SANITIZE`: e.g. `address` or `address,undefined`; builds the
//!   shared-library C test with `-fsanitize=<value>` (CI uses `address`).
//! - `OXV_FFI_LEAK_CHECK`: a command prefix to run the (unsanitized) C test
//!   under, e.g. `leaks --atExit --` on macOS or
//!   `valgrind --error-exitcode=1 --leak-check=full --errors-for-leak-kinds=definite,indirect`.
//!
//! Unix and Windows (MSVC target, with clang as the driver). Sanitizers and
//! the leak check are opt-in and only wired up for Unix.
#![cfg(any(unix, windows))]

use std::path::{Path, PathBuf};
use std::process::Command;
use vorbis::{Channels, Options, Quality, SampleRate};

const STEREO_FRAMES: usize = 44100 + 4321;
const MONO_FRAMES: usize = 15000;
const EDGE_FRAMES: usize = 3000;

/// Mirror of `gen_i16` in test_oxivorbis.c.
fn gen_i16(frames: usize, channels: usize, seed: u32) -> Vec<i16> {
    let mut state = seed;
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for c in 0..channels {
            let square: i32 = if (i / (50 + 17 * c)) % 2 == 1 {
                6000
            } else {
                -6000
            };
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let noise = ((state >> 16) & 0x3fff) as i32 - 0x2000;
            out.push((square + noise) as i16);
        }
    }
    out
}

/// Mirror of `to_f32` in test_oxivorbis.c.
fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|&s| s as f32 / 32768.0 * 1.1).collect()
}

/// Mirror of `options_a` in test_oxivorbis.c.
fn options_a() -> Options {
    Options::default()
        .with_quality(Quality::new(-1.0).unwrap())
        .with_serial(42)
        .with_vendor("c-test vendor")
        .with_comment("TITLE", "Test=Title")
        .with_comment("A=B", "v")
        .with_comment("", "skipped")
}

/// Mirror of `options_b` in test_oxivorbis.c.
fn options_b() -> Options {
    Options::default()
        .with_quality(Quality::new(7.5).unwrap())
        .with_serial(0xDEAD_BEEF)
        .with_comment("ARTIST", "Ünïcödé")
}

/// Every stream the C test writes, with the bytes Rust produces for it.
fn expected() -> Vec<(&'static str, Vec<u8>)> {
    let stereo = gen_i16(STEREO_FRAMES, 2, 1);
    let stereo_f = to_f32(&stereo);
    let mono_f = to_f32(&gen_i16(MONO_FRAMES, 1, 7));
    let edge = gen_i16(EDGE_FRAMES, 1, 99);
    let (s, d, a, b) = (
        Channels::Stereo,
        Options::default(),
        options_a(),
        options_b(),
    );
    let hz = |r| SampleRate::new(r).unwrap();
    let enc = |pcm: &[i16], r, c, o: &Options| vorbis::encode(pcm, hz(r), c, o);
    let enc_f = |pcm: &[f32], r, c, o: &Options| vorbis::encode_f32(pcm, hz(r), c, o);
    vec![
        ("oneshot_i16", enc(&stereo, 44100, s, &d)),
        ("oneshot_f32", enc_f(&mono_f, 22050, Channels::Mono, &a)),
        ("rate_min", enc(&edge, 1, Channels::Mono, &d)),
        ("rate_max", enc(&edge, 200_000, Channels::Mono, &d)),
        ("empty", enc(&[], 48000, s, &d)),
        ("encoder_i16", enc(&stereo, 44100, s, &b)),
        ("encoder_f32", enc_f(&stereo_f, 44100, s, &d)),
        ("encoder_planar_f32", enc_f(&stereo_f, 44100, s, &a)),
        ("encoder_planar_i16", enc(&stereo, 44100, s, &a)),
        ("writer_i16", enc(&stereo, 44100, s, &b)),
        ("writer_f32", enc_f(&stereo_f, 44100, s, &b)),
        ("writer_planar_f32", enc_f(&stereo_f, 44100, s, &b)),
        ("writer_planar_i16", enc(&stereo, 44100, s, &b)),
    ]
}

fn ffi_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `target/<profile>` with an up-to-date liboxivorbis in it.
///
/// `cargo test` compiles this package's library only into the test
/// harness, not as the cdylib/staticlib, so build those with the same
/// cargo, target dir, profile and features (a no-op when fresh). Once per
/// test binary.
fn lib_dir() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let exe = std::env::current_exe().unwrap();
            // target/<profile>/deps/c_api-<hash>
            let dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
            let mut cargo = Command::new(env!("CARGO"));
            cargo
                .args(["build", "-p", "oxivorbis-ffi", "--target-dir"])
                .arg(dir.parent().unwrap());
            if dir.file_name().is_some_and(|n| n == "release") {
                cargo.arg("--release");
            }
            if cfg!(feature = "parallel") {
                cargo.args(["--features", "parallel"]);
            }
            run(&mut cargo);
            let dylib = dir.join(if cfg!(windows) {
                "oxivorbis.dll"
            } else if cfg!(target_os = "macos") {
                "liboxivorbis.dylib"
            } else {
                "liboxivorbis.so"
            });
            assert!(dylib.exists(), "{} was not built", dylib.display());
            dir
        })
        .clone()
}

fn tool(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

fn cc() -> String {
    tool("CC", if cfg!(windows) { "clang" } else { "cc" })
}

fn cxx() -> String {
    tool("CXX", if cfg!(windows) { "clang++" } else { "c++" })
}

/// Link `cmd` against the shared library: `-loxivorbis` plus an rpath on
/// Unix; the import library on Windows, with the DLL copied next to the
/// executable in `exe_dir` (Windows has no rpath).
fn link_shared(cmd: &mut Command, libs: &Path, exe_dir: &Path) {
    if cfg!(windows) {
        cmd.arg(libs.join("oxivorbis.dll.lib"));
        std::fs::copy(libs.join("oxivorbis.dll"), exe_dir.join("oxivorbis.dll")).unwrap();
    } else {
        cmd.arg("-L")
            .arg(libs)
            .arg("-loxivorbis")
            .arg(format!("-Wl,-rpath,{}", libs.display()));
    }
}

fn run(cmd: &mut Command) {
    eprintln!("running {cmd:?}");
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed: {status}");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Native libraries a C program linking the static library needs (from
/// `cargo rustc --crate-type staticlib -- --print native-static-libs`).
fn native_static_libs() -> &'static [&'static str] {
    if cfg!(windows) {
        // MSVC: kernel32 ntdll userenv ws2_32 dbghelp (msvcrt is the default).
        &[
            "-lkernel32",
            "-lntdll",
            "-luserenv",
            "-lws2_32",
            "-ldbghelp",
        ]
    } else if cfg!(target_os = "macos") {
        // libSystem (which rustc lists) is linked by cc anyway.
        &[]
    } else {
        &[
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]
    }
}

/// Build the C test (`static_link`: against liboxivorbis.a instead of the
/// shared library), run it and compare its streams with Rust's.
fn c_test(name: &str, static_link: bool, sanitize: Option<&str>, wrapper: &[String]) {
    let libs = lib_dir();
    let dir = fresh_dir(name);
    let exe = dir.join(format!("test_oxivorbis{}", std::env::consts::EXE_SUFFIX));
    let mut cc = Command::new(cc());
    cc.args([
        "-std=c99",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-pedantic",
        "-g",
        "-O1",
    ])
    .arg("-I")
    .arg(ffi_dir().join("include"))
    .arg(ffi_dir().join("tests/c/test_oxivorbis.c"))
    .arg("-o")
    .arg(&exe);
    if let Some(s) = sanitize {
        cc.arg(format!("-fsanitize={s}"))
            .arg("-fno-omit-frame-pointer");
    }
    if static_link {
        let lib = if cfg!(windows) {
            "oxivorbis.lib"
        } else {
            "liboxivorbis.a"
        };
        cc.arg(libs.join(lib)).args(native_static_libs());
    } else {
        link_shared(&mut cc, &libs, &dir);
    }
    run(&mut cc);

    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let mut cmd = match wrapper.split_first() {
        Some((first, rest)) => {
            let mut c = Command::new(first);
            c.args(rest).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    cmd.arg(&out);
    // Linux ASan also runs LeakSanitizer by default; make it explicit.
    cmd.env(
        "ASAN_OPTIONS",
        "detect_leaks=1:abort_on_error=0:halt_on_error=1",
    );
    if cfg!(target_os = "macos") {
        // Apple's ASan does not support leak detection.
        cmd.env("ASAN_OPTIONS", "halt_on_error=1");
    }
    run(&mut cmd);

    let mut bad = Vec::new();
    for (stream, want) in expected() {
        let path = out.join(format!("{stream}.ogg"));
        let got = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if got != want {
            bad.push(format!(
                "{stream}: C {} bytes, Rust {} bytes",
                got.len(),
                want.len()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "C output differs from oxivorbis::encode:\n{}",
        bad.join("\n")
    );
}

#[test]
fn c_api_shared_library() {
    let sanitize = std::env::var("OXV_FFI_SANITIZE")
        .ok()
        .filter(|s| !s.is_empty());
    c_test("ffi-c-shared", false, sanitize.as_deref(), &[]);
}

#[test]
fn c_api_static_library() {
    c_test("ffi-c-static", true, None, &[]);
}

#[test]
fn c_api_leak_check() {
    let Some(wrapper) = std::env::var("OXV_FFI_LEAK_CHECK")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("OXV_FFI_LEAK_CHECK not set; skipping");
        return;
    };
    let wrapper: Vec<String> = wrapper.split_whitespace().map(str::to_owned).collect();
    c_test("ffi-c-leaks", false, None, &wrapper);
}

#[test]
fn cpp_header_smoke() {
    let libs = lib_dir();
    let dir = fresh_dir("ffi-cpp");
    let exe = dir.join(format!("smoke{}", std::env::consts::EXE_SUFFIX));
    let mut cxx = Command::new(cxx());
    // The header itself is C++11-clean; MSVC's own STL needs C++14.
    let std = if cfg!(windows) {
        "-std=c++14"
    } else {
        "-std=c++11"
    };
    cxx.args([std, "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .arg("-I")
        .arg(ffi_dir().join("include"))
        .arg(ffi_dir().join("tests/c/smoke.cpp"))
        .arg("-o")
        .arg(&exe);
    if cfg!(windows) {
        // Newer MSVC STLs refuse a clang older than they were tested with
        // (STL1000); the smoke test only needs our header to compile.
        cxx.arg("-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH");
    }
    link_shared(&mut cxx, &libs, &dir);
    run(&mut cxx);
    run(&mut Command::new(&exe));
}

/// The header also compiles as strict C89-free C99 with every warning on,
/// standalone (no other includes first).
#[test]
fn header_is_self_contained_c99() {
    let dir = fresh_dir("ffi-header-c99");
    let src = dir.join("only_header.c");
    std::fs::write(
        &src,
        "#include \"oxivorbis.h\"\nint main(void) { return OXV_STATUS_OK; }\n",
    )
    .unwrap();
    run(Command::new(cc())
        .args([
            "-std=c99",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pedantic",
            "-fsyntax-only",
        ])
        .arg("-I")
        .arg(ffi_dir().join("include"))
        .arg(&src));
}
