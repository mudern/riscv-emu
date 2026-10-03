# riscv-emu

[English](README.md)

用 Rust 编写的 RISC-V 全系统模拟器。目标：在模拟器内部启动真实的 Linux
用户态——每一步都与 QEMU 行为做差分验证。

**当前状态：可引导 OpenSBI v1.5 → Linux 6.19-rc2 → BusyBox 交互 shell，
并在 VM 内用 Alpine + musl + TinyCC 现场编译运行 C 程序。**这一层真实
RISC-V 机器所需的一切——特权架构、Sv39 分页、PMP、PLIC、串口——均按
规范（特权架构 v1.13）实现，并与 QEMU 做了差分测试。

## 快速开始

```sh
cargo build --release      # 编译模拟器（只需宿主机 clang，无需交叉工具链）

./linux.sh                 # OpenSBI + Linux + BusyBox 交互 shell
./alpine.sh                # Alpine + musl + tcc：在 VM 内编译 hello.c
```

两个脚本在首次运行时会自动构建缺失的部分（git clone + 编译，约 5–10
分钟；源码放 `dist/src/`，产物放 `dist/`，均已 gitignore）。若
`~/Code/source/{opensbi,linux}` 已存在同名目录则直接复用。之后启动直接
使用缓存产物。

`./alpine.sh` 会在 VM 内执行预设演示（`tcc hello.c -o hello && ./hello`
→ 打印 `Hello, world!`，约 1.4 秒虚拟机时间），随后进入带 busybox +
musl + tcc 的 Alpine shell。编辑 `board/alpine/init.tmpl` 可改变执行的
内容；`poweroff -f` 关机。

## 功能集

- **ISA**：RV64IMAFC + Zicsr + Zifencei + F/D 浮点
  - 完整 F/D 指令集、全部五种舍入模式、精确 `fflags`
    （inexact/underflow 用无误差变换：two-sum / FMA 残差判定）、
    规范 NaN、f32 NaN-boxing
- **特权级**：M/S/U，标准 trap 交付（vectored mtvec/stvec）、mret/sret、
  MPRV/SUM/MXR/TVM/TW/TSR、mstatus.FS 跟踪与 SD 合成
- **Sv39 MMU**：三级走表、大页、U/S 权限与 SUM/MXR、A/D 自动置位、
  直接映射 TLB、保留 PTE 检查、走表访问过 PMP（有效特权级 S，
  拒绝时报原始访问类型）
- **PMP**：16 项、G=0（支持 NA4）、PA 宽 56 位 → pmpaddr 实现 54 位；
  first-match 任意字节重叠语义（§3.7.1.3）；L 位对所有特权级生效；
  锁定 TOR 连带锁定前项 pmpaddr；写入时 WARL 规范化
- **CSR**：对照特权规范 v1.13 逐条审查——mip 可写性与 sip 可见性分离
  （委派子集）、对 mip 的 CSRRS/CSRRC 只使用软件位、satp/mtvec/medeleg/
  MPP/mcountinhibit WARL、TVM 门控 satp 访问、misa 可写并按格式门控
  F/D、mcycle/minstret 独立计数且受 mcountinhibit 抑制
- **中断**：CLINT（msip/mtime/mtimecmp）、PLIC（96 源、M/S context、
  阈值、claim/complete、挂起锁存 + 网关）、外部中断线每步同步进 mip
- **设备**：NS16550 UART（接收侧 + 中断 → PLIC 源 10）、sifive_test
- **引导**：真 OpenSBI `fw_dynamic`（a0/a1/a2 + fw_dynamic_info v2）、
  ELF/二进制内核、外部 initramfs + DTB 运行时回填
- **宿主控制台**：stdin 逐字节接入 UART RX，guest shell 完全可交互

## 仓库结构

```
src/
  main.rs        CLI、固件引导流程、stdin 注入、诊断信息
  machine.rs     整机：CPU + 总线、运行循环（中断线同步）、停机原因
  cpu.rs         hart：执行、ALU、特权/trap 交付、地址翻译 + PMP
  csr.rs         CSR 文件（M/S）、视图寄存器、WARL 规范化
  decode.rs      RV64IMAFC 解码（32 位 + 压缩 → 规范形式）
  fpu.rs         IEEE-754 F/D 核心（无误差变换实现舍入/标志）
  bus.rs         物理地址路由：RAM / MMIO
  mmu.rs         Sv39 走表 + 直接映射 TLB + 走表 PMP
  pmp.rs         16 项 PMP（TOR/NA4/NAPOT、锁、first-match）
  elf.rs         ELF64 加载器（对齐 QEMU：按 paddr 加载、支持 vmlinux）
  exception.rs   异常类型 / mcause 编码
  devices/       uart (16550)、clint、plic、testdev (sifive_test)
board/           virt.dts/dtb（对齐 QEMU virt，裁剪版）+ Alpine rootfs
initramfs/       busybox initramfs 构建脚本（下载 Debian busybox-static）
dist/            构建脚本与拉取的产物（gitignored）
tests/           16 套：ISA、特权、mmu、pmp、timer、plic、sbi、fpu、
                 fp_ctx、spec（规范一致性回归）、csr_matrix…
```

## 手动构建整个世界（可选）

`linux.sh` / `alpine.sh` 会自动完成以下步骤。手动执行：

```sh
# OpenSBI v1.5（clang 可用；脚本会自动补 -Wno unused-but-set）
git clone --depth 1 --branch v1.5 \
    https://github.com/riscv-software-src/opensbi.git
make -C opensbi O=build PLATFORM=generic LLVM=1 FW_PEXT=imac -j$(nproc)
cp opensbi/build/platform/generic/firmware/fw_dynamic.bin dist/

# Linux v6.19-rc2（配置：dist/linux.config——RV64IMAFC、Sv39、8250，
# 无 SMP/EFI/NET/PCI；内嵌 initramfs 清空，initrd 经 --initrd 传入）
git clone --depth 1 --branch v6.19-rc2 \
    https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git
sh dist/build-all.sh linux    # 或照脚本手动执行 olddefconfig + Image

# BusyBox initramfs（Debian 静态 busybox；无需 root）
initramfs/build-busybox-initramfs.sh

# 启动
./target/x86_64-unknown-linux-musl/release/riscv-emu \
    --bios dist/fw_dynamic.bin --kernel dist/Image \
    --initrd initramfs/busybox.cpio --dtb board/virt.dtb
```

### Alpine + tcc（在 VM 内编译）

```sh
board/alpine/build-rootfs.sh dist/rootfs.cpio
./target/x86_64-unknown-linux-musl/release/riscv-emu \
    --bios dist/fw_dynamic.bin --kernel dist/Image \
    --initrd dist/rootfs.cpio --dtb board/virt.dtb --mem 512
```

initramfs 含 Alpine 3.22 用户态、musl 与头文件、tcc（小型 C 编译器）。
`/init` 在 VM 内编译并运行 `hello.c`，然后进入 shell。rootfs 约 34MB
cpio；需要 `--mem 512`。

## 命令行参考

```
riscv-emu [--trace] [--stats] [--mem <MB>] [--bin] [--sbi] <image>
riscv-emu --bios <fw> [--kernel <image>] [--initrd <cpio>] [--dtb <dtb>]
          [--trace] [--stats] [--mem <MB>]
```

## 差分测试方法论

每一层都用 QEMU（`qemu-system-riscv64 -M virt`，v11.1）验证：运行
**相同的**固件/内核/DTB，QEMU 的 CPU 扩展裁剪到与本模拟器一致：

- OpenSBI banner + payload 输出：**逐字节一致**
- Linux dmesg：除时序自测量（raid6/xor 基准）与诚实的能力差异
  （无 H 扩展 → 无 ELF compat 模式）外完全一致
- 16 套测试（`cargo test`）覆盖 ISA 语义、特权架构、Sv39、PMP、中断
  控制器、F/D 数值与规范一致性回归——每条回归都能追溯到一次审查发现
  （见 `tests/spec.rs` 注释）

若干对规范敏感的细节（均对照规范文本而非仅 QEMU 验证）：AMO 32 位
结果符号扩展；SUM 对取指无效果；mstatus.SD 只读合成；M→S 陷阱时
mstatus.SPP 记录真实前特权；失败的 SC 作废预约；`ESC[6n` 之类的终端
怪癖是 guest 自己的事，不是模拟器的。

## 已知限制

- 单 hart；无 hypervisor/AIA/Svnapot/Svpbmt
- `mcycle` 按 1 CPI 与指令同步递增（无独立时钟模型）；F/D 标志除
  FMADD（two-sum 近似）与低于最小次正规数的结果（见 `src/fpu.rs`）
  外均精确
- 尚无 virtio 设备（rootfs 经 initramfs 提供）；约 20 MIPS

## 许可

MIT（见 `LICENSE-MIT`）。客户软件保留各自许可：Linux GPLv2、OpenSBI
BSD-2-Clause、BusyBox GPLv2、Alpine/musl MIT、tcc LGPLv2.1——本仓库不
再分发其中任何部分，构建脚本从上游获取。
