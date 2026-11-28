#!/usr/bin/env bash
# Build and package the release binaries for one target (used by
# .github/workflows/release.yml; also runs locally):
#
#     tools/release/package.sh <rust-target> <out-dir>
#
# Writes two archives to <out-dir> (.tar.gz, or .zip on Windows):
#
#   oxivorbis-cli-<version>-<target>   the `oxivorbis` command-line encoder,
#                                      built with --features parallel
#   liboxivorbis-<version>-<target>    the C library: include/oxivorbis.h,
#                                      the shared and static libraries, a
#                                      pkg-config file (Unix) and a CMake
#                                      package; default (serial) features
#
# Both are relocatable. The pkg-config file uses ${pcfiledir}; the macOS
# dylib keeps its @rpath install name, as in the build tree.
set -euo pipefail

target="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

# The release profile keeps line tables for profiling; shipped binaries
# don't need them.
export CARGO_PROFILE_RELEASE_DEBUG=false CARGO_PROFILE_RELEASE_STRIP=debuginfo

version="$(cargo pkgid -p oxivorbis | sed 's/.*[#@]//')"
cargo build --release --locked --target "$target" -p oxivorbis-cli --features parallel
cargo build --release --locked --target "$target" -p oxivorbis-ffi
bin="${CARGO_TARGET_DIR:-$root/target}/$target/release"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

archive() { # <dir name>
    if [[ "$target" == *windows* ]]; then
        (cd "$stage" && 7z a -tzip -bso0 "$out/$1.zip" "$1")
    else
        tar -C "$stage" -czf "$out/$1.tar.gz" "$1"
    fi
}

# CLI.
cli="oxivorbis-cli-$version-$target"
mkdir -p "$stage/$cli"
cp LICENSE NOTICE "$stage/$cli/"
cp tools/oxivorbis-cli/README.md "$stage/$cli/README.md"
if [[ "$target" == *windows* ]]; then
    cp "$bin/oxivorbis.exe" "$stage/$cli/"
else
    cp "$bin/oxivorbis" "$stage/$cli/"
fi
archive "$cli"

# C library.
lib="liboxivorbis-$version-$target"
d="$stage/$lib"
mkdir -p "$d/include" "$d/lib/cmake/oxivorbis"
cp LICENSE NOTICE "$d/"
cp ffi/README.md "$d/README.md"
cp ffi/include/oxivorbis.h "$d/include/"
sed -e "s|@VERSION@|$version|" ffi/pkg/oxivorbisConfig.cmake.in \
    > "$d/lib/cmake/oxivorbis/oxivorbisConfig.cmake"
case "$target" in
    *windows-msvc*)
        mkdir -p "$d/bin"
        cp "$bin/oxivorbis.dll" "$d/bin/"
        cp "$bin/oxivorbis.dll.lib" "$bin/oxivorbis.lib" "$d/lib/"
        ;;
    *)
        case "$target" in
            *apple*) dylib=liboxivorbis.dylib; private="" ;;
            *) dylib=liboxivorbis.so; private="-lpthread -ldl -lm" ;;
        esac
        cp "$bin/$dylib" "$bin/liboxivorbis.a" "$d/lib/"
        mkdir -p "$d/lib/pkgconfig"
        sed -e 's|^prefix=@PREFIX@|prefix=${pcfiledir}/../..|' \
            -e "s|@VERSION@|$version|" -e "s|@LIBS_PRIVATE@|$private|" \
            ffi/pkg/oxivorbis.pc.in > "$d/lib/pkgconfig/oxivorbis.pc"
        ;;
esac
archive "$lib"

ls -l "$out"
