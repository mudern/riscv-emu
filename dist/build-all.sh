#!/bin/sh
# 拉取源码并构建 OpenSBI + Linux（产物输出到 dist/，已 gitignore）
#
# 用法: dist/build-all.sh [opensbi|linux]
# 环境变量:
#   SRC_ROOT  源码存放目录（默认 dist/src；若 ~/Code/source 下已有同名
#             目录则优先复用）
#   JOBS      并行编译数（默认 nproc）
set -e
cd "$(dirname "$0")"
mkdir -p src
SRC=${SRC_ROOT:-src}
JOBS=${JOBS:-$(nproc)}

# 若用户源码根下已有现成 clone，直接复用
[ -d "$SRC/opensbi" ] || [ ! -d "$HOME/Code/source/opensbi" ] || SRC="$HOME/Code/source"
[ -d "$SRC/linux" ] || [ ! -d "$HOME/Code/source/linux" ] || SRC="$HOME/Code/source"
echo "源码根: $SRC"

WANT_OPENSBI=1
WANT_LINUX=1
case "${1:-all}" in
    opensbi) WANT_LINUX=0 ;;
    linux)   WANT_OPENSBI=0 ;;
esac

# ---------- OpenSBI v1.5 ----------
if [ "$WANT_OPENSBI" = 1 ] && [ ! -f fw_dynamic.bin ]; then
    if [ ! -d "$SRC/opensbi" ]; then
        echo "== clone OpenSBI v1.5 =="
        git clone --depth 1 --branch v1.5 \
            https://github.com/riscv-software-src/opensbi.git "$SRC/opensbi"
    fi
    echo "== build OpenSBI =="
    # clang 23 将 unused-but-set 视为错误（-Werror），补 -Wno
    grep -q Wno-unused-but-set-variable "$SRC/opensbi/Makefile" || \
        sed -i 's/-Wall -Werror/-Wall -Wno-unused-but-set-variable -Werror/' \
            "$SRC/opensbi/Makefile"
    make -C "$SRC/opensbi" O=build PLATFORM=generic LLVM=1 FW_PEXT=imac -j"$JOBS"
    cp "$SRC/opensbi/build/platform/generic/firmware/fw_dynamic.bin" .
    echo "✓ dist/fw_dynamic.bin"
fi

# ---------- Linux v6.19-rc2 ----------
if [ "$WANT_LINUX" = 1 ] && [ ! -f Image ]; then
            if [ ! -d "$SRC/linux" ]; then
                echo "== clone Linux v6.19-rc2（--depth 1，约 260MB）=="
                git clone --depth 1 --branch v6.19-rc2 \
                    https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git \
                    "$SRC/linux" || \
                git clone --depth 1 --branch v6.19-rc2 \
                    https://github.com/torvalds/linux.git "$SRC/linux"
            fi
            echo "== build Linux =="
            mkdir -p "$SRC/linux/build-rv64-emu"
            cp linux.config "$SRC/linux/build-rv64-emu/.config"
            make -C "$SRC/linux" O=build-rv64-emu ARCH=riscv LLVM=1 olddefconfig
            make -C "$SRC/linux" O=build-rv64-emu ARCH=riscv LLVM=1 -j"$JOBS" Image
            cp "$SRC/linux/build-rv64-emu/arch/riscv/boot/Image" .
            echo "✓ dist/Image"
fi
# gen_init_cpio（busybox initramfs 用，Linux 构建附带产出）
GEN="$SRC/linux/build-rv64-emu/usr/gen_init_cpio"
if [ "$WANT_LINUX" = 1 ] && [ ! -x "$GEN" ]; then
    make -C "$SRC/linux" O=build-rv64-emu ARCH=riscv LLVM=1 usr/gen_init_cpio
fi
if [ -x "$GEN" ] && [ ! -f ../initramfs/busybox.cpio ]; then
    "$GEN" -o ../initramfs/busybox.cpio ../initramfs/initramfs.list
    echo "✓ initramfs/busybox.cpio"
fi
echo "构建完成"
