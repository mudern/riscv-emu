#!/bin/sh
# Alpine + musl + tcc 基本操作系统环境：VM 内自动编译并运行 hello world
# 产物缺失时自动下载/构建（Alpine rootfs ~34MB，源码与产物均已 gitignore）
# 用法: ./alpine.sh [模拟器额外参数，如 --stats]
set -e
cd "$(dirname "$0")"

EMU=target/x86_64-unknown-linux-musl/release/riscv-emu
[ -x "$EMU" ] || cargo build --release

# 1. 固件 + 内核（与 linux.sh 共用构建器）
[ -f dist/fw_dynamic.bin ] && [ -f dist/Image ] || sh dist/build-all.sh

# 2. Alpine rootfs（minirootfs + busybox + musl-dev + tcc）
[ -f dist/rootfs.cpio ] || board/alpine/build-rootfs.sh dist/rootfs.cpio

# 3. 启动（VM 内 /init 自动 tcc 编译 hello.c 并运行）
exec "$EMU" --bios dist/fw_dynamic.bin --kernel dist/Image \
    --initrd dist/rootfs.cpio --dtb board/virt.dtb --mem 512 "$@"
