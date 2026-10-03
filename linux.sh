#!/bin/sh
# 基础 Linux 环境：OpenSBI + Linux + busybox 交互 shell
# 产物缺失时自动拉源码编译（源码进 dist/src，产物进 dist/，均已 gitignore）
# 用法: ./linux.sh [模拟器额外参数，如 --stats]
set -e
cd "$(dirname "$0")"

EMU=target/x86_64-unknown-linux-musl/release/riscv-emu
[ -x "$EMU" ] || cargo build --release

# 1. 固件 + 内核（缺失则克隆源码并编译）
[ -f dist/fw_dynamic.bin ] && [ -f dist/Image ] || sh dist/build-all.sh

# 2. busybox initramfs（用内核构建产出的 gen_init_cpio 生成）
if [ ! -f initramfs/busybox.cpio ]; then
    GEN=dist/src/linux/build-rv64-emu/usr/gen_init_cpio
    [ -x "$GEN" ] || GEN="$HOME/Code/source/linux/build-rv64-emu/usr/gen_init_cpio"
    [ -x "$GEN" ] || { echo "需要 gen_init_cpio（先完成内核构建）"; exit 1; }
    "$GEN" -o initramfs/busybox.cpio initramfs/initramfs.list
    echo "✓ initramfs/busybox.cpio"
fi

# 3. 启动
exec "$EMU" --bios dist/fw_dynamic.bin --kernel dist/Image \
    --initrd initramfs/busybox.cpio --dtb board/virt.dtb "$@"
