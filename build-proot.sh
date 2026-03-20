#!/bin/bash
# Build proot and proot_loader for Android aarch64.
# Fetches termux/proot source (which has TCGETS2 fix) and talloc,
# cross-compiles with the project's NDK, outputs to libs/arm64-v8a/.
#
# Usage: ./build-proot.sh
# Prerequisites: Android NDK (sourced from .env)

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$DIR/.env"

if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    echo "Error: ANDROID_NDK_HOME not set (check .env)" >&2
    exit 1
fi

# Versions
TALLOC_V="2.4.3"
TALLOC_URL="https://www.samba.org/ftp/talloc/talloc-${TALLOC_V}.tar.gz"
PROOT_SRC="$DIR/patches/proot"

# Build config
API=35
ARCH=aarch64
TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64"
CC="$TOOLCHAIN/bin/${ARCH}-linux-android${API}-clang"
AR="$TOOLCHAIN/bin/llvm-ar"
RANLIB="$TOOLCHAIN/bin/llvm-ranlib"
STRIP="$TOOLCHAIN/bin/llvm-strip"
OBJCOPY="$TOOLCHAIN/bin/llvm-objcopy"
OBJDUMP="$TOOLCHAIN/bin/llvm-objdump"

BUILD_DIR="$DIR/.tmp/proot-build"
STATIC_DIR="$BUILD_DIR/static"
OUTPUT_DIR="$DIR/libs/arm64-v8a"

mkdir -p "$BUILD_DIR" "$STATIC_DIR/include" "$STATIC_DIR/lib" "$OUTPUT_DIR"

# --- Fetch sources ---

if [ ! -d "$PROOT_SRC/src" ]; then
    echo "Error: proot submodule not found at patches/proot" >&2
    echo "Run: git submodule update --init patches/proot" >&2
    exit 1
fi

echo "=== Fetching talloc source ==="
if [ ! -d "$BUILD_DIR/talloc-${TALLOC_V}" ]; then
    curl -L "$TALLOC_URL" | tar xz -C "$BUILD_DIR"
fi

# --- Build talloc (static) ---

echo "=== Building talloc ==="
cd "$BUILD_DIR/talloc-${TALLOC_V}"

export CC AR RANLIB STRIP OBJCOPY OBJDUMP

make distclean 2>/dev/null || true

cat >cross-answers.txt <<EOF
Checking uname sysname type: "Linux"
Checking uname machine type: "dontcare"
Checking uname release type: "dontcare"
Checking uname version type: "dontcare"
Checking simple C program: OK
rpath library support: OK
-Wl,--version-script support: FAIL
Checking getconf LFS_CFLAGS: OK
Checking for large file support without additional flags: OK
Checking for -D_FILE_OFFSET_BITS=64: OK
Checking for -D_LARGE_FILES: OK
Checking correct behavior of strtoll: OK
Checking for working strptime: OK
Checking for C99 vsnprintf: OK
Checking for HAVE_SHARED_MMAP: OK
Checking for HAVE_MREMAP: OK
Checking for HAVE_INCOHERENT_MMAP: OK
Checking for HAVE_SECURE_MKSTEMP: OK
Checking getconf large file support flags work: OK
Checking for HAVE_IFACE_IFCONF: FAIL
EOF

# talloc's configure is broken for cross-compilation, needs a mock
MOCK_DIR="$BUILD_DIR/mock-bin"
mkdir -p "$MOCK_DIR"
cat >"$MOCK_DIR/aarch64-linux-android${API}-clang" <<'MOCKEOF'
#!/bin/sh
exec cc "$@"
MOCKEOF
chmod +x "$MOCK_DIR/aarch64-linux-android${API}-clang"
export PATH="$MOCK_DIR:$PATH"

./configure build "--prefix=$STATIC_DIR" --disable-rpath --disable-python --cross-compile --cross-answers=cross-answers.txt

"$AR" rcs "$STATIC_DIR/lib/libtalloc.a" bin/default/talloc*.o
cp -f talloc.h "$STATIC_DIR/include"

# --- Build proot ---

echo "=== Building proot ==="
cd "$PROOT_SRC/src"

LOADER_DIR="$BUILD_DIR/loader"
mkdir -p "$LOADER_DIR"

export CFLAGS="-I$STATIC_DIR/include"
export LDFLAGS="-L$STATIC_DIR/lib"
export PROOT_UNBUNDLE_LOADER="$LOADER_DIR"

make distclean 2>/dev/null || true
make V=1 "CC=$CC" "STRIP=$STRIP" "OBJCOPY=$OBJCOPY" "OBJDUMP=$OBJDUMP" \
    "PREFIX=$BUILD_DIR/install" install

# --- Copy outputs ---

echo "=== Installing to libs/arm64-v8a/ ==="
cp "$PROOT_SRC/src/proot" "$OUTPUT_DIR/libproot.so"
"$STRIP" "$OUTPUT_DIR/libproot.so"

if [ -f "$LOADER_DIR/loader" ]; then
    cp "$LOADER_DIR/loader" "$OUTPUT_DIR/libproot_loader.so"
    "$STRIP" "$OUTPUT_DIR/libproot_loader.so"
fi

echo "=== Done ==="
ls -la "$OUTPUT_DIR/libproot.so" "$OUTPUT_DIR/libproot_loader.so"
echo ""
echo "Verify TCGETS2 fix:"
"$TOOLCHAIN/bin/llvm-strings" "$OUTPUT_DIR/libproot.so" | grep -i version | head -3
