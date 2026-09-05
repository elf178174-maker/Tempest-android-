#!/usr/bin/env bash
#
# Build PRoot and its ELF loaders for arm64 Android, from source.
#
# WHY THIS EXISTS
#
# An Android app targeting API 29 or later may not execve() a file in its own
# data directory — only files in nativeLibraryDir, which the package installer
# populates from the APK and keeps read-only. That rule stops the usual
# "download a binary and run it" approach dead.
#
# PRoot sidesteps it without defeating it. It ptrace()s its children and
# rewrites path-related syscalls so an extracted Linux filesystem appears at /.
# When a guest program is started, PRoot does not hand the guest binary to
# execve(): it execve()s its own small loader, and the loader maps the guest ELF
# itself. Mapping executable pages out of app data is exactly what dlopen() does
# and is permitted; only execve() is not.
#
# That only holds if the loader is a *separate file*. By default PRoot embeds
# the loader in its own binary and extracts it to a temporary file at runtime —
# which on Android would land in app data and be unexecutable. So this build
# sets PROOT_UNBUNDLE_LOADER, which makes PRoot read PROOT_LOADER and
# PROOT_LOADER_32 from the environment instead.
#
# Three files are produced, named lib*.so so the installer extracts them into
# nativeLibraryDir:
#
#   libproot.so           the tracer
#   libproot-loader.so    the 64-bit ELF loader
#   libproot-loader32.so  the 32-bit ELF loader
#
# TALLOC
#
# PRoot links against Samba's talloc, which has no Android build and whose waf
# build system needs a configure run that cannot cross-compile cleanly. talloc
# is one C file, so it is compiled directly against a small stand-in for
# libreplace (see replace.h below): everything libreplace would detect at
# configure time is simply asserted, which is correct for any modern toolchain.
# It is linked statically, so no shared library has to be shipped under a name
# Android would refuse to extract.
#
# LICENCES
#
# PRoot is GPL-2.0-or-later, talloc is LGPL-3.0-or-later. Linking them makes the
# resulting binary effectively GPL-3.0 — which is what every distribution's
# proot package already is. That binary is an independent program executed as a
# subprocess, never linked into Tempest, so shipping it in the same APK is mere
# aggregation. Sources, revisions and full licence texts: THIRD_PARTY_LICENSES.md
set -euo pipefail

PROOT_REPO="${PROOT_REPO:-https://github.com/termux/proot.git}"
# Pinned so builds are reproducible and the recorded provenance stays true.
PROOT_REF="${PROOT_REF:-v5.1.107.92}"

# talloc lives in the Samba tree. samba.org's own tarballs are used when
# reachable; the git mirror is the fallback, because some build environments
# can reach github.com and nothing else.
TALLOC_VERSION="${TALLOC_VERSION:-2.4.3}"
SAMBA_REPO="${SAMBA_REPO:-https://github.com/samba-team/samba.git}"

API="${ANDROID_API:-29}"
ARCH="${ANDROID_ARCH:-aarch64}"
OUT_DIR="${1:?usage: build-proot.sh <output jniLibs/arm64-v8a dir>}"

: "${ANDROID_NDK_HOME:?ANDROID_NDK_HOME must point at an Android NDK}"

TOOLCHAIN="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64"
if [ ! -d "${TOOLCHAIN}" ]; then
    echo "error: NDK toolchain not found at ${TOOLCHAIN}" >&2
    exit 1
fi

export CC="${TOOLCHAIN}/bin/${ARCH}-linux-android${API}-clang"
export AR="${TOOLCHAIN}/bin/llvm-ar"
export STRIP="${TOOLCHAIN}/bin/llvm-strip"
export OBJCOPY="${TOOLCHAIN}/bin/llvm-objcopy"
export OBJDUMP="${TOOLCHAIN}/bin/llvm-objdump"

for tool in "${CC}" "${AR}" "${STRIP}" "${OBJCOPY}" "${OBJDUMP}"; do
    if [ ! -x "${tool}" ]; then
        echo "error: ${tool} is missing from the NDK" >&2
        exit 1
    fi
done

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# --------------------------------------------------------------------------
# talloc
# --------------------------------------------------------------------------
echo "==> Fetching talloc ${TALLOC_VERSION}"
TALLOC_DIR="${WORK}/talloc"
mkdir -p "${TALLOC_DIR}"

if curl -fsSL --max-time 120 \
       "https://www.samba.org/ftp/talloc/talloc-${TALLOC_VERSION}.tar.gz" \
       -o "${WORK}/talloc.tar.gz" 2>/dev/null; then
    tar xzf "${WORK}/talloc.tar.gz" -C "${WORK}"
    cp "${WORK}/talloc-${TALLOC_VERSION}/talloc.c" \
       "${WORK}/talloc-${TALLOC_VERSION}/talloc.h" "${TALLOC_DIR}/"
    cp "${WORK}/talloc-${TALLOC_VERSION}/LICENSE" "${TALLOC_DIR}/" 2>/dev/null || true
    echo "    from samba.org"
else
    echo "    samba.org unreachable; using the git mirror"
    git clone --quiet --depth 1 --filter=blob:none --sparse "${SAMBA_REPO}" "${WORK}/samba"
    git -C "${WORK}/samba" sparse-checkout set lib/talloc >/dev/null
    cp "${WORK}/samba/lib/talloc/talloc.c" \
       "${WORK}/samba/lib/talloc/talloc.h" "${TALLOC_DIR}/"
    cp "${WORK}/samba/lib/talloc/LICENSE" "${TALLOC_DIR}/" 2>/dev/null || true
fi

# talloc.c includes "replace.h" and expects libreplace's configure results.
# Everything below is true of any modern C11 toolchain, Bionic included.
cat > "${TALLOC_DIR}/replace.h" <<'REPLACE_H'
/* Minimal stand-in for Samba's libreplace, sufficient for talloc.c. */
#ifndef _TALLOC_REPLACE_H
#define _TALLOC_REPLACE_H

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdbool.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <limits.h>
#include <sys/types.h>

#ifndef MIN
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#endif
#ifndef MAX
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#endif

#define HAVE_CONSTRUCTOR_ATTRIBUTE 1
#define HAVE_DESTRUCTOR_ATTRIBUTE 1
#define HAVE_VA_COPY 1
#define HAVE_STRNLEN 1
#define HAVE_STRNDUP 1
#define HAVE_VASPRINTF 1
#define HAVE_VSNPRINTF 1
#define HAVE_SNPRINTF 1
#define HAVE_MEMMOVE 1

#ifndef PRINTF_ATTRIBUTE
#define PRINTF_ATTRIBUTE(a, b) __attribute__((__format__(__printf__, a, b)))
#endif
#ifndef _PUBLIC_
#define _PUBLIC_ __attribute__((visibility("default")))
#endif
#ifndef _DEPRECATED_
#define _DEPRECATED_ __attribute__((deprecated))
#endif
#ifndef _UNUSED_
#define _UNUSED_ __attribute__((unused))
#endif
#ifndef discard_const_p
#define discard_const_p(type, ptr) ((type *)((intptr_t)(ptr)))
#endif

/* C23's memset_explicit; libreplace supplies it where the libc does not. */
#if !defined(__STDC_LIB_EXT1__) && (!defined(__STDC_VERSION__) || __STDC_VERSION__ < 202311L)
static inline void *talloc_memset_explicit(void *s, int c, size_t n)
{
    volatile unsigned char *p = (volatile unsigned char *)s;
    while (n--) {
        *p++ = (unsigned char)c;
    }
    return s;
}
#define memset_explicit(s, c, n) talloc_memset_explicit((s), (c), (n))
#endif

#endif /* _TALLOC_REPLACE_H */
REPLACE_H

# The waf build passes the version through as three defines; read them out of
# the header so a talloc bump does not silently produce a mismatched magic.
read_version() {
    local name="$1"
    local value
    value="$(sed -n "s/^#define TALLOC_VERSION_${name}[[:space:]]\\+\\([0-9]\\+\\).*/\\1/p" \
             "${TALLOC_DIR}/talloc.h" | head -1)"
    echo "${value:-0}"
}
TV_MAJOR="$(read_version MAJOR)"
TV_MINOR="$(read_version MINOR)"
# Only MAJOR and MINOR appear in talloc.h; waf supplies the release digit. It
# feeds nothing but talloc's internal magic value, which only has to be
# consistent within one build, so defaulting it to 0 is safe.
TV_RELEASE="$(read_version RELEASE)"
echo "    talloc ${TALLOC_VERSION} (header reports ${TV_MAJOR}.${TV_MINOR})"

echo "==> Building talloc for ${ARCH}-linux-android${API}"
"${CC}" -c -O2 -fPIC -I"${TALLOC_DIR}" \
    -DTALLOC_BUILD_VERSION_MAJOR="${TV_MAJOR}" \
    -DTALLOC_BUILD_VERSION_MINOR="${TV_MINOR}" \
    -DTALLOC_BUILD_VERSION_RELEASE="${TV_RELEASE}" \
    "${TALLOC_DIR}/talloc.c" -o "${TALLOC_DIR}/talloc.o"
"${AR}" rcs "${TALLOC_DIR}/libtalloc.a" "${TALLOC_DIR}/talloc.o"

# --------------------------------------------------------------------------
# PRoot
# --------------------------------------------------------------------------
echo "==> Cloning PRoot ${PROOT_REF}"
if ! git clone --quiet --depth 1 --branch "${PROOT_REF}" "${PROOT_REPO}" "${WORK}/proot" 2>/dev/null; then
    # Some refs are commits rather than tags.
    git clone --quiet "${PROOT_REPO}" "${WORK}/proot"
    git -C "${WORK}/proot" checkout --quiet "${PROOT_REF}"
fi
PROOT_REVISION="$(git -C "${WORK}/proot" rev-parse HEAD)"

# --------------------------------------------------------------------------
# One upstream fix
# --------------------------------------------------------------------------
#
# extension/ashmem_memfd/ashmem_memfd.c is compiled only under __ANDROID__, and
# it calls strcmp() and memset() without including <string.h>. Nobody noticed
# because until clang 16 an implicit declaration was a warning; the NDK's clang
# makes it an error, so the file no longer builds at all.
#
# The fix is applied here rather than with a compiler flag on purpose. A global
# `-include string.h` breaks two other things in this tree: the assembly source
# cannot be preprocessed with a C header, and loader/loader.c deliberately
# avoids libc headers and defines its own static basename(). And
# `-Wno-implicit-function-declaration` would hide the same class of bug
# everywhere else, which is worse than fixing the one real instance.
ASHMEM="${WORK}/proot/src/extension/ashmem_memfd/ashmem_memfd.c"
if [ -f "${ASHMEM}" ] && ! grep -q '#include <string.h>' "${ASHMEM}"; then
    echo "==> Adding the missing <string.h> include to ashmem_memfd.c"
    sed -i 's|#include <stdlib.h>|#include <stdlib.h>\n#include <string.h> /* strcmp, memset — missing upstream */|' "${ASHMEM}"
    grep -q '#include <string.h>' "${ASHMEM}" || {
        echo "error: could not apply the ashmem_memfd fix" >&2
        exit 1
    }
fi

echo "==> Building PRoot"
# These are exported rather than passed on the command line: the makefile uses
# `+=` on both, and a command-line assignment would replace its own -I and
# -ltalloc rather than adding to them.
export CPPFLAGS="-I${TALLOC_DIR}"
export CFLAGS="-O2 -fPIC"
export LDFLAGS="-L${TALLOC_DIR} -pie"

# PROOT_UNBUNDLE_LOADER is the whole point: it makes PRoot honour PROOT_LOADER
# instead of extracting an embedded loader into a directory Android will not
# let it execute from. The value is only the compiled-in default; Tempest always
# sets the environment variables explicitly.
make -C "${WORK}/proot/src" -j"$(nproc)" V=0 \
    PROOT_UNBUNDLE_LOADER=/data/local/tmp \
    proot loader/loader

SRC="${WORK}/proot/src"

# The 32-bit loader only exists on architectures that can run 32-bit guests.
# The Hangover stack is entirely 64-bit, so its absence is not fatal — but
# PROOT_LOADER_32 must still point at something real.
if make -C "${SRC}" V=0 PROOT_UNBUNDLE_LOADER=/data/local/tmp loader/loader-m32 2>/dev/null; then
    HAVE_M32=1
else
    echo "    note: no 32-bit loader for this architecture"
    HAVE_M32=0
fi

# --------------------------------------------------------------------------
# Install
# --------------------------------------------------------------------------
mkdir -p "${OUT_DIR}"

install_as() {
    local src="$1" dest="$2"
    if [ ! -f "${src}" ]; then
        echo "error: expected build output ${src} is missing" >&2
        exit 1
    fi
    cp "${src}" "${OUT_DIR}/${dest}"
    "${STRIP}" --strip-unneeded "${OUT_DIR}/${dest}" 2>/dev/null || true
    echo "    ${dest}  $(stat -c%s "${OUT_DIR}/${dest}") bytes"
}

echo "==> Installing into ${OUT_DIR}"
install_as "${SRC}/proot" "libproot.so"
install_as "${SRC}/loader/loader" "libproot-loader.so"
if [ "${HAVE_M32}" = "1" ] && [ -f "${SRC}/loader/loader-m32" ]; then
    install_as "${SRC}/loader/loader-m32" "libproot-loader32.so"
else
    cp "${OUT_DIR}/libproot-loader.so" "${OUT_DIR}/libproot-loader32.so"
fi

# --------------------------------------------------------------------------
# Verify
# --------------------------------------------------------------------------
echo "==> Verifying"
fail=0

describe() { file -b "$1" 2>/dev/null || echo unknown; }

# The tracer and the 64-bit loader must be aarch64 Android binaries.
for f in libproot.so libproot-loader.so; do
    info="$(describe "${OUT_DIR}/${f}")"
    case "${info}" in
        *aarch64*) ;;
        *)
            echo "::error::${f} should be an aarch64 binary but is: ${info}"
            fail=1
            ;;
    esac
    echo "    ${f}: ${info}"
done

# The 32-bit loader is *supposed* to be a 32-bit ARM binary: it exists to map
# 32-bit guest ELFs. Requiring aarch64 here would be wrong. It is only ever
# executed for a 32-bit guest, which the Hangover stack never produces, so if
# the build fell back to copying the 64-bit loader that is fine too.
info="$(describe "${OUT_DIR}/libproot-loader32.so")"
case "${info}" in
    *"ARM, EABI"*|*aarch64*) ;;
    *)
        echo "::error::libproot-loader32.so is neither 32-bit ARM nor aarch64: ${info}"
        fail=1
        ;;
esac
echo "    libproot-loader32.so: ${info}"

# libproot.so must be a *dynamically linked* Android executable — it needs
# Bionic's linker at /system/bin/linker64.
if ! describe "${OUT_DIR}/libproot.so" | grep -q "dynamically linked"; then
    echo "::error::libproot.so is not dynamically linked; it will not start on Android"
    fail=1
fi

# A dynamic dependency on talloc would mean the static link silently did not
# happen, and the app would fail at runtime with a missing-library error.
if "${TOOLCHAIN}/bin/llvm-readelf" -d "${OUT_DIR}/libproot.so" 2>/dev/null | grep -qi talloc; then
    echo "::error::libproot.so has a dynamic dependency on talloc; it must be static"
    fail=1
fi

[ "${fail}" = "0" ] || exit 1

echo "${PROOT_REVISION}" > "${OUT_DIR}/../proot-revision.txt"
echo "==> Done (proot ${PROOT_REF} @ ${PROOT_REVISION})"
