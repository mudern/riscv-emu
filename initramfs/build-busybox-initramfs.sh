#!/bin/sh
# 生成 busybox 基础 initramfs（linux.sh 用）：
# 从 Debian 下载 busybox-static + gen_init_cpio 打包（无需 root）
# 用法: initramfs/build-busybox-initramfs.sh [gen_init_cpio 路径]
set -e
cd "$(dirname "$0")"
GEN="${1:-../dist/src/linux/build-rv64-emu/usr/gen_init_cpio}"

# 1. busybox 静态二进制（Debian riscv64 busybox-static，无动态依赖）。
#    注意：Alpine 的 busybox 是动态链接 musl 的，不能单独使用。
if [ ! -f busybox ]; then
    M=https://mirrors.kernel.org/debian/pool/main/b/busybox
    DEB=$(curl -sL "$M/" | grep -oE 'busybox-static_[^"]*_riscv64\.deb' | sort -u | tail -1)
    echo "下载 $DEB"
    curl -sL -o busybox-static.deb "$M/$DEB"
    ar x busybox-static.deb data.tar.xz
    # 只提取 busybox 本体（deb 元数据不落目录；路径可能带 ./ 前缀）
    tar -xJf data.tar.xz usr/bin/busybox ./usr/bin/busybox 2>/dev/null || \
        tar -xJf data.tar.xz
    mv usr/bin/busybox . && rm -rf usr data.tar.xz busybox-static.deb
fi
chmod +x busybox

# 2. gen_init_cpio 打包
SPEC=$(mktemp)
{
    echo "dir /proc 0755 0 0"
    echo "dir /sys 0755 0 0"
    echo "dir /bin 0755 0 0"
    echo "dir /dev 0755 0 0"
    echo "nod /dev/console 0600 0 0 c 5 1"
    echo "file /init $(pwd)/init 0755 0 0"
    echo "file /bin/busybox $(pwd)/busybox 0755 0 0"
    echo "file /bin/setwinsize $(pwd)/setwinsize 0755 0 0"
    echo "slink /bin/sh /bin/busybox 0777 0 0"
} > "$SPEC"
if [ ! -x "$GEN" ]; then
    echo "gen_init_cpio 不存在: $GEN（先运行内核构建，或传路径参数）" >&2
    exit 1
fi
"$GEN" -o busybox.cpio "$SPEC"
rm -f "$SPEC"
echo "✓ initramfs/busybox.cpio ($(stat -c%s busybox.cpio) 字节)"
