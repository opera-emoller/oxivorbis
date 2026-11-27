//! Linker settings for the shared library's identity.
//!
//! - macOS / iOS: install name `@rpath/liboxivorbis.dylib` (rustc would
//!   otherwise embed the absolute build path), so an app links it from its
//!   own rpath (e.g. `-Wl,-rpath,@executable_path/../Frameworks`).
//! - ELF (Linux, Android, BSDs): soname `liboxivorbis.so`, so programs
//!   record that name rather than the build path. Not versioned while the
//!   ABI is 0.x; distributions that want `liboxivorbis.so.N` can relink with
//!   their own `-Wl,-soname`.
//!
//! Only the cdylib is affected; the static library has no identity.

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match os.as_str() {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/liboxivorbis.dylib");
        }
        "linux" | "android" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" | "illumos" => {
            println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,liboxivorbis.so");
        }
        _ => {}
    }
    println!("cargo:rerun-if-changed=build.rs");
}
