#!/usr/bin/env bash
# Build liboxivorbis in release mode and install it under PREFIX (Unix):
#
#     ffi/pkg/install.sh [PREFIX]        # default /usr/local; or `just ffi-install PREFIX`
#
# Installs
#     PREFIX/include/oxivorbis.h
#     PREFIX/lib/liboxivorbis.{so,dylib} and liboxivorbis.a
#     PREFIX/lib/pkgconfig/oxivorbis.pc
#     PREFIX/lib/cmake/oxivorbis/oxivorbisConfig.cmake
#
# On macOS the installed dylib's install name is set to its absolute path
# (PREFIX/lib/liboxivorbis.dylib) so programs linked with pkg-config find
# it without an rpath; the build-tree dylib keeps @rpath/liboxivorbis.dylib
# for bundling into apps. Extra cargo flags (e.g. --features parallel) can
# be passed in CARGO_FLAGS.
set -euo pipefail

prefix="${1:-/usr/local}"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$root"

# shellcheck disable=SC2086
cargo build --release -p oxivorbis-ffi ${CARGO_FLAGS:-}
target="${CARGO_TARGET_DIR:-$root/target}/release"
version="$(cargo pkgid -p oxivorbis-ffi | sed 's/.*[#@]//')"

case "$(uname -s)" in
    Darwin) dylib=liboxivorbis.dylib; private="" ;;
    *) dylib=liboxivorbis.so; private="-lpthread -ldl -lm" ;;
esac

install -d "$prefix/include" "$prefix/lib/pkgconfig" "$prefix/lib/cmake/oxivorbis"
install -m 644 ffi/include/oxivorbis.h "$prefix/include/"
install -m 644 "$target/liboxivorbis.a" "$prefix/lib/"
install -m 755 "$target/$dylib" "$prefix/lib/"
if [ "$(uname -s)" = Darwin ]; then
    install_name_tool -id "$prefix/lib/$dylib" "$prefix/lib/$dylib" 2>/dev/null
    # install_name_tool invalidates the (ad-hoc) signature; arm64 needs one.
    codesign --force --sign - "$prefix/lib/$dylib"
fi
sed -e "s|@PREFIX@|$prefix|" -e "s|@VERSION@|$version|" -e "s|@LIBS_PRIVATE@|$private|" \
    ffi/pkg/oxivorbis.pc.in > "$prefix/lib/pkgconfig/oxivorbis.pc"
sed -e "s|@VERSION@|$version|" \
    ffi/pkg/oxivorbisConfig.cmake.in > "$prefix/lib/cmake/oxivorbis/oxivorbisConfig.cmake"
echo "installed oxivorbis $version C library under $prefix"
