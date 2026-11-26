# Shared build settings for the C oracles. Sourced (not executed) by
# build.sh, build_quality_dumpers.sh, build_dsp_oracle.sh and
# tools/gen-setup-templates/run.sh.
#
# Every oracle is compiled from two pristine upstream trees:
#
#   tools/libvorbis-1.3.7/  lib/**/*.{c,h} (incl. modes/, books/), include/
#                           and COPYING, from libvorbis-1.3.7.tar.gz
#                           sha256 0e982409a9c3fc82ee06e08205b1355e5c6aa4c36bca58146ef399621b0ce5ab
#   tools/libogg-1.3.5/     src/{framing,bitwise}.c, src/crctable.h,
#                           include/ogg/{ogg,os_types}.h and COPYING,
#                           from libogg-1.3.5.tar.xz
#                           sha256 c4d91be36fc8e54deae7575241e03f4211eb102afb3fc0775fbbc1b740016705
#
# Each tree has a SHA256SUMS manifest of its files. It is checked here
# before anything is compiled, so an edit to a vendored file (or a stray
# extra file) fails the build. Two things deviate from upstream, both
# applied from outside the vendored trees:
#   - `-include stable_qsort.h` redirects qsort() to a stable merge sort
#     (see stable_qsort.h);
#   - include/ogg/config_types.h replaces libogg's configure-generated
#     header.
# No system libogg or libvorbis is used.
#
# The instrumented copy under tools/debug-libvorbis-dump/ is only for
# the debug-* tools and is never used to build an oracle.

ORACLE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLS_DIR="$(cd "$ORACLE_DIR/.." && pwd)"
VORBIS="$TOOLS_DIR/libvorbis-1.3.7"
VEND="$VORBIS/lib"
OGG="$TOOLS_DIR/libogg-1.3.5"

lw_sha256_check() {
    # $1 = directory containing SHA256SUMS (paths relative to it).
    local dir="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$dir" && sha256sum --quiet -c SHA256SUMS)
    else
        (cd "$dir" && shasum -a 256 --quiet -c SHA256SUMS)
    fi || {
        echo "$(basename "$0"): checksum mismatch in $dir: vendored sources must be pristine upstream" >&2
        exit 1
    }
    local listed actual
    listed="$(awk '{print $2}' "$dir/SHA256SUMS" | LC_ALL=C sort)"
    actual="$(cd "$dir" && find . -type f ! -name SHA256SUMS ! -name .DS_Store | sed 's|^\./||' | LC_ALL=C sort)"
    if [ "$listed" != "$actual" ]; then
        echo "$(basename "$0"): file list of $dir differs from its SHA256SUMS:" >&2
        diff <(echo "$listed") <(echo "$actual") >&2 || true
        exit 1
    fi
}

lw_sha256_check "$VORBIS"
lw_sha256_check "$OGG"

CC="${CC:-cc}"
# -O0 -ffp-contract=off: no FMA contraction and no reordering of f32 ops,
# so the output is bit-deterministic across hosts and compilers.
# gnu99 (not strict c99) so libvorbis can call alloca() without an
# explicit <alloca.h> include on glibc.
CFLAGS="${CFLAGS:--O0 -ffp-contract=off -std=gnu99 -Wno-implicit-function-declaration -Wno-unused-function -Wno-unused-variable}"
# Extra flags appended to CFLAGS, e.g. LW_EXTRA_CFLAGS="-g -fsanitize=address".
read -r -a LW_EXTRA <<< "${LW_EXTRA_CFLAGS:-}"
read -r -a LW_CFLAGS <<< "$CFLAGS"
LW_CFLAGS+=("${LW_EXTRA[@]+"${LW_EXTRA[@]}"}")

# Vendored libvorbis, vendored libogg, then our config_types.h. -I paths
# take precedence over any system <ogg/...> headers.
LW_INCLUDES=(
    -include "$ORACLE_DIR/stable_qsort.h"
    -I"$VORBIS/include"
    -I"$VEND"
    -I"$OGG/include"
    -I"$ORACLE_DIR/include"
)

# libvorbis encoder sources. vorbisenc.c is listed separately because some
# tools #include it to reach its static tables.
LW_VORBIS_SRCS=()
for s in analysis.c bitrate.c block.c codebook.c envelope.c floor0.c \
         floor1.c info.c lookup.c lpc.c lsp.c mapping0.c mdct.c psy.c \
         registry.c res0.c sharedbook.c smallft.c synthesis.c window.c; do
    LW_VORBIS_SRCS+=("$VEND/$s")
done
LW_OGG_SRCS=("$OGG/src/framing.c" "$OGG/src/bitwise.c")
LW_SHIM_SRCS=("$ORACLE_DIR/stable_qsort.c")

# lw_cc <out> <sources...>: compile and link one oracle binary.
lw_cc() {
    local out="$1"
    shift
    echo "$(basename "$0"): $CC ${LW_CFLAGS[*]} -> $out"
    "$CC" "${LW_CFLAGS[@]}" "${LW_INCLUDES[@]}" -o "$out" "$@" -lm
}
