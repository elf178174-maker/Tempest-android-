#!/usr/bin/env bash
#
# Build PRoot and its ELF loaders for arm64 Android, from source.
#
# WHY THIS EXISTS
#
# An Android app targeting API 29 or later may not execve() a file in its own
# data directory — only files in nativeLibraryDir, which the package installer
# populates from the APK and keeps read-only. That rule is what stops the usual
# "download a binary and run it" approach dead.
#
# PRoot sidesteps it without defeating it. It ptrace()s its children and
# rewrites path-related syscalls so an extracted Linux filesystem appears at /.
# When a guest program is started, PRoot does not hand the guest binary to
# execve(): it execve()s its own tiny loader — which lives in nativeLibraryDir
# and is therefore executable — and the loader maps the guest ELF itself.
# Mapping executable pages out of app data is exactly what dlopen() does and is
# permitted; only execve() is not.
#
# So three files must ship inside the APK, named lib*.so so the installer
# extracts them:
#
#   libproot.so           the tracer
#   libproot-loader.so    the 64-bit ELF loader
#   libproot-loader32.so  the 32-bit ELF loader
#
# LICENCE
#
# PRoot is GPL-2.0-or-later. It is a separate program invoked as a subprocess,
# not linked into Tempest, so shipping it alongside an MIT/Apache-2.0 app is
# mere aggregation. The full licence text and the exact source revision are
# recorded in THIRD_PARTY_LICENSES.md, which is what GPL-2.0 section 3 asks for.
set -euo pipefail

PROOT_REPO="${PROOT_REPO:-https://github.com/termux/proot.git}"
# Pinned so a build is reproducible and the recorded provenance stays true.
PROOT_REF="${PROOT_REF:-v5.1.107-63}"
API="${ANDROID_API:-29}"
OUT_DIR="${1:?usage: build-proot.sh <output-jniLibs/arm64-v8a dir>}"

: "${ANDROID_NDK_HOME:?ANDROID_NDK_HOME must point at an Android NDK}"

TOOLCHAIN="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64"
if [ ! -d "${TOOLCHAIN}" ]; then
    echo "error: NDK toolchain not found at ${TOOLCHAIN}" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

echo "==> Cloning PRoot ${PROOT_REF}"
git clone --depth 1 --branch "${PROOT_REF}" "${PROOT_REPO}" "${WORK}/proot" 2>/dev/null \
    || {
        # Some refs are commits rather than tags; fall back to a full clone.
        git clone "${PROOT_REPO}" "${WORK}/proot"
        git -C "${WORK}/proot" checkout "${PROOT_REF}"
    }

SRC="${WORK}/proot/src"
export CC="${TOOLCHAIN}/bin/aarch64-linux-android${API}-clang"
export LD="${TOOLCHAIN}/bin/ld"
export AR="${TOOLCHAIN}/bin/llvm-ar"
export OBJCOPY="${TOOLCHAIN}/bin/llvm-objcopy"
export STRIP="${TOOLCHAIN}/bin/llvm-strip"

# Termux's PRoot expects these; they select the Android-flavoured code paths.
export CPPFLAGS="-DANDROID -DUSE_LOADER_32BIT=1 -I${SRC}"
export CFLAGS="-O2 -fPIC -Wno-error"
export LDFLAGS="-pie"

echo "==> Building for aarch64-linux-android${API}"
make -C "${SRC}" -j"$(nproc)" V=1 proot loader loader-m32 \
    || make -C "${SRC}" -j"$(nproc)" V=1

mkdir -p "${OUT_DIR}"

install_as() {
    local src="$1" dest="$2"
    if [ ! -f "${src}" ]; then
        echo "error: expected build output ${src} is missing" >&2
        return 1
    fi
    "${STRIP}" --strip-unneeded "${src}" 2>/dev/null || true
    cp "${src}" "${OUT_DIR}/${dest}"
    echo "    ${dest}  $(stat -c%s "${OUT_DIR}/${dest}") bytes"
}

echo "==> Installing into ${OUT_DIR}"
install_as "${SRC}/proot" "libproot.so"
install_as "${SRC}/loader/loader" "libproot-loader.so"
# The 32-bit loader is only needed for 32-bit guest binaries. The Hangover stack
# is entirely 64-bit, so a missing loader-m32 is not fatal — but PROOT_LOADER_32
# must then point at something, so fall back to the 64-bit loader rather than
# leaving the variable dangling.
if [ -f "${SRC}/loader/loader-m32" ]; then
    install_as "${SRC}/loader/loader-m32" "libproot-loader32.so"
else
    echo "    note: no 32-bit loader was built; reusing the 64-bit one"
    cp "${OUT_DIR}/libproot-loader.so" "${OUT_DIR}/libproot-loader32.so"
fi

echo "==> Recording provenance"
git -C "${WORK}/proot" rev-parse HEAD > "${OUT_DIR}/../proot-revision.txt" 2>/dev/null || true

echo "==> Done"
ls -la "${OUT_DIR}"
