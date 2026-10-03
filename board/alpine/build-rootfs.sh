#!/bin/sh
# 搭建 Alpine riscv64 + musl + tcc 的 initramfs（在宿主机执行，无需 root）
# 用法: board/alpine/build-rootfs.sh <输出.cpio>
set -e
OUT="${1:-/tmp/rootfs.cpio}"
DL="/tmp/alpine-dl"
M=https://mirrors.tuna.tsinghua.edu.cn/alpine/v3.22
mkdir -p "$DL"

# 1. 下载（minirootfs + tcc + tcc-libs-static + musl-dev）
test -f "$DL/minirootfs.tar.gz" || curl -s -o "$DL/minirootfs.tar.gz" \
  "$M/releases/riscv64/alpine-minirootfs-3.22.0-riscv64.tar.gz"
test -f "$DL/tcc.apk" || curl -s -o "$DL/tcc.apk" "$M/community/riscv64/tcc-0.9.27_git20250106-r0.apk"
test -f "$DL/tcc-libs.apk" || curl -s -o "$DL/tcc-libs.apk" "$M/community/riscv64/tcc-libs-0.9.27_git20250106-r0.apk"
test -f "$DL/tcc-libs-static.apk" || curl -s -o "$DL/tcc-libs-static.apk" "$M/community/riscv64/tcc-libs-static-0.9.27_git20250106-r0.apk"
test -f "$DL/musl-dev.apk" || curl -s -o "$DL/musl-dev.apk" "$M/main/riscv64/musl-dev-1.2.5-r12.apk"

# 2. 解包（apk 为拼接 gzip 流，单次 tar 即可；非 root 跳过设备节点）
ROOT=/tmp/stage4/rootfs
rm -rf "$ROOT"; mkdir -p "$ROOT"
tar -xzf "$DL/minirootfs.tar.gz" -C "$ROOT" 2>/dev/null
tar -xzf "$DL/tcc.apk" -C "$ROOT" 2>/dev/null
tar -xzf "$DL/tcc-libs.apk" -C "$ROOT" 2>/dev/null      # libtcc.so（tcc 二进制的依赖）
tar -xzf "$DL/tcc-libs-static.apk" -C "$ROOT" 2>/dev/null  # libtcc1.a（浮点运行时）
tar -xzf "$DL/musl-dev.apk" -C "$ROOT" 2>/dev/null

# 3. /init 与 hello.c
install -m 755 "$(dirname "$0")/init.tmpl" "$ROOT/init"
install -m 644 "$(dirname "$0")/hello.c" "$ROOT/root/hello.c"

# 4. 生成 spec（根目录文件 + 递归；dev 节点手工定义，免 root）
cd "$ROOT"
{
  echo "dir /dev 0755 0 0"
  echo "nod /dev/console 0600 0 0 c 5 1"
  echo "nod /dev/null 0666 0 0 c 1 3"
  echo "nod /dev/ttyS0 0600 0 0 c 4 64"
  echo "dir /proc 0755 0 0"
  echo "dir /sys 0755 0 0"
  echo "dir /tmp 0755 0 0"
  echo "dir /run 0755 0 0"
  python3 - <<'PY'
import os
base = '.'
for dirpath, dirnames, filenames in os.walk(base):
    dirnames.sort()
    rel = os.path.relpath(dirpath, base)
    if rel == '.':
        prefix = ''
    else:
        if rel.split('/')[0] in ('dev', 'proc', 'sys', 'tmp', 'run'):
            continue
        print(f"dir /{rel} 0755 0 0")
        prefix = '/' + rel
    for f in sorted(filenames):
        p = os.path.join(dirpath, f)
        if os.path.islink(p):
            print(f"slink {prefix}/{f} {os.readlink(p)} 0777 0 0")
        else:
            mode = oct(os.stat(p).st_mode & 0o777)[2:]
            print(f"file {prefix}/{f} {os.path.abspath(p)} {mode} 0 0")
PY
} > /tmp/stage4/initramfs.spec

# 5. 生成 cpio（内核 usr/gen_init_cpio）
GEN=~/Code/source/linux/build-rv64-emu/usr/gen_init_cpio
test -x "$GEN" || (cd ~/Code/source/linux && make O=build-rv64-emu ARCH=riscv LLVM=1 usr/gen_init_cpio >/dev/null 2>&1)
"$GEN" /tmp/stage4/initramfs.spec > "$OUT"
echo "initramfs: $OUT ($(stat -c%s "$OUT") 字节)"
