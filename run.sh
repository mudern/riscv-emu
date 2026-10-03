#!/bin/sh
# 一键启动：./run.sh <minios|torture|linux|alpine> [模拟器额外参数...]
# 产物缺失时自动调用 dist/build-all.sh 拉源码编译
set -e
cd "$(dirname "$0")"
EMU=target/x86_64-unknown-linux-musl/release/riscv-emu
[ -x "$EMU" ] || cargo build --release

FW=dist/fw_dynamic.bin
KERNEL=dist/Image

case "$1" in
    minios)
        shift; exec "$EMU" examples/minios.elf "$@"
        ;;
    torture)
        shift; exec "$EMU" --stats examples/torture.elf "$@"
        ;;
    linux)
        [ -f "$FW" ] && [ -f "$KERNEL" ] || sh dist/build-all.sh
        [ -f initramfs/busybox.cpio ] || {
            GEN=dist/src/linux/build-rv64-emu/usr/gen_init_cpio
            [ -x "$GEN" ] || GEN="$HOME/Code/source/linux/build-rv64-emu/usr/gen_init_cpio"
            "$GEN" initramfs/initramfs.list initramfs/busybox.cpio
        }
        shift; exec "$EMU" --bios "$FW" --kernel "$KERNEL" \
            --initrd initramfs/busybox.cpio --dtb board/virt.dtb "$@"
        ;;
    alpine)
        [ -f "$FW" ] && [ -f "$KERNEL" ] || sh dist/build-all.sh
        [ -f dist/rootfs.cpio ] || board/alpine/build-rootfs.sh dist/rootfs.cpio
        shift; exec "$EMU" --bios "$FW" --kernel "$KERNEL" \
            --initrd dist/rootfs.cpio --dtb board/virt.dtb --mem 512 "$@"
        ;;
    *)
        echo "用法: ./run.sh <minios|torture|linux|alpine> [额外参数...]"
        echo "  minios  裸机迷你 OS（M/S/U + Sv39 演示）"
        echo "  torture 指令集压力测试"
        echo "  linux   OpenSBI + Linux + busybox 交互 shell"
        echo "  alpine  Alpine + musl + tcc，VM 内编译运行 hello world"
        exit 1
        ;;
esac
