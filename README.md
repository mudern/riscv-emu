# riscv-emu

用 Rust 写的 RISC-V 全系统模拟器。目标（长线）：启动 Linux 内核 + busybox。

当前是**阶段 3**：真固件引导 + F/D 浮点 + **启动 Linux + busybox shell**。
`--bios fw_dynamic.bin --kernel Image --dtb board/virt.dtb` 引导真 OpenSBI v1.5
→ Linux 6.19-rc2（LLVM 构建，CONFIG_FPU=y）→ 内嵌 initramfs 的 busybox
ash 交互 shell，`poweroff -f` 干净关机（宿主退出码 0）。全流程约 12.7 亿条
指令、95 秒（13.3 MIPS）。与 QEMU 11.1.1（同固件/DTB/内核，`-cpu` 关闭
本模拟器未实现的扩展）对比：OpenSBI banner + payload 输出逐字节一致；
内核 dmesg 98 行中仅性能自测值（raid6/xor/对齐访问比例）与能力差异
（无 H 扩展的 ELF compat）不同，其余全部一致。

## 已支持

- ISA：RV64IMAFDC + Zicsr + Zifencei（含完整 F/D 浮点：IEEE 语义、
  全部 5 种舍入模式、fflags 精确标志——inexact/underflow 用无误差变换
  （two-sum/FMA 残差）判定；NaN 规范化；f32 NaN-boxing）
- 规范一致性（对照特权/非特权规范逐条审查 + QEMU 差分）：
  mret 仅 M 态且退出时清 MPRV；SPP 记录真实前特权（M→S 陷阱 SPP=1）；
  委派中断按 QEMU 语义归属 S 集；EBREAK mtval=0；失败 SC 作废预约；
  mstatus.SD 只读合成；misa 可写（按格式门控 F/D）；satp/mepc WARL；
  Sv39 保留 PTE 编码报页 fault；RMM 平局取绝对值更大候选（含 2 的幂
  边界两侧间距不对称）；溢出窗口（RNE 舍回 max 仍报 OF）
- 特权级：M/S/U 三级，标准 trap 交付（mtvec/stvec、vectored 模式）、
  medeleg/mideleg 委托、mret/sret、MPRV/SUM/MXR/TVM/TW/TSR
- Sv39 MMU：三级走表、大页（对齐检查）、U/S 权限与 SUM/MXR、A/D 位自动置位、
  直接映射 TLB（satp 写与 sfence.vma 冲刷）；跨页 misaligned 访问按两段
  分别翻译/PMP 检查；AMO 要求自然对齐（对齐 QEMU 的 misaligned 异常语义）
- PMP（对照规范 3.7）：16 项、G=0（NA4 支持）、PA 宽 56 位 → pmpaddr
  实现 54 位（高位 WARL 零）；first-match 按**任意字节重叠**的最低编号
  条目裁决，未全覆盖即失败；L=1 约束所有特权级，L=0 放行 M；锁定的
  TOR 项连带锁定 pmpaddr[i-1]；写入规范化（保留位零、R=0&W=1 → W=0）；
  走表访问（PTE 读/A-D 写）以 S 态过 PMP，拒绝时报原始类型 access fault（对齐 QEMU）
- 中断：CLINT mtime/mtimecmp（MTIP）与 msip（MSIP）；PLIC（96 源、3 位
  优先级、M/S 双 context、阈值/claim/complete、挂起锁存与网关占用语义对齐
  QEMU）；mip 硬件线（MTIP/MSIP/MEIP/SEIP）由运行循环同步，SEIP 与软件位
  合并（标准双触发语义）
- 设备：NS16550 UART（THR/RBR/IER/IIR/FCR/LCR/MCR/LSR/MSR/SCR + 波特率
  除数，RX FIFO 16 字节，IER.0 接收中断 → PLIC 源 10）；CLINT；sifive_test
- 内置 SBI（`--sbi`）：S 态 ecall 按 SBI v1 处理（BASE/TIME/IPI/RFENCE/
  SRST/DBCN + legacy putchar/shutdown），便于不引 OpenSBI 直接跑 S 态内核
- 真固件引导（`--bios`）：QEMU 同款流程——a0=hartid、a1=FDT、a2=
  fw_dynamic_info（v2 结构）；固件支持 ELF 与裸二进制（QEMU 默认的
  fw_dynamic.bin 即裸二进制装到 0x8000_0000）；FDT 按 QEMU 规则放 RAM 顶
  2MB 对齐处
- CSR 面（对照特权规范 1.13 逐条审查）：
  - mip 写掩码 = SSIP|STIP/SEIP（stimecmp 未实现时恒可写），MSIP/MTIP/
    MEIP 只读由 CLINT/PLIC 驱动；sip/sie 可见与可写位 = mideleg 委派子集；
    CSRRS/CSRRC 对 mip/sip 的 RMW 只使用软件位（PLIC 信号不参与）
  - WARL 写入规范化：satp（不支持 MODE 整体无效；ASID 16 位/PPN 44 位）、
    mtvec/stvec（BASE 对齐 + MODE 仅 direct/vectored）、medeleg（排除
    ecall-M 与保留位）、mideleg（仅 S 中断源）、MPP（保留值 2→U）、
    mcountinhibit（CY/TM/IR）、mepc[0]=0
  - TVM=1 时 S 态读/写 satp 均 illegal；mcycle/minstret M 态可写、独立
    计数、受 mcountinhibit.CY/IR 抑制；mcounteren/scounteren 访问门控
  - mstatus.SD 只读合成（FS=Dirty 时置 1）；misa 可写并按格式门控 F/D
- 内存布局对齐 QEMU `virt` 机器：

  | 区域 | 范围 | 说明 |
  |---|---|---|
  | sifive_test | `0x0010_0000` | 写 `0x5555` 正常关机、`0x3333` 失败关机 |
  | CLINT | `0x0200_0000` | msip、mtime（10MHz）/mtimecmp |
  | PLIC | `0x0C00_0000` | 优先级/pending/enable/threshold/claim |
  | UART | `0x1000_0000` | NS16550，中断接 PLIC 源 10 |
  | RAM | `0x8000_0000` | 默认 128MB，`--mem` 可调 |

- ELF64 加载器（静态、非 PIE；`ET_DYN` 按 p_vaddr 原地装入，同 QEMU
  load_elf；内核 vmlinux 按 p_paddr 加载并换算物理入口，Image 裸二进制
  放 0x8020_0000——均对齐 QEMU）
- 交互控制台：宿主 stdin 逐字节注入 UART RX（绕开 stdio 缓冲），
  busybox shell 可交互

### 阶段 1 约定（裸机程序怎么用）

M-mode `ecall` 被模拟器拦截为 Linux 风格宿主调用（ABI 同 Linux：
`a7`=调用号，`a0..a2`=参数，返回值放 `a0`）：

| 调用号 | 调用 | 说明 |
|---|---|---|
| 64 | `write(fd, buf, count)` | fd=1/2 输出到标准输出/错误 |
| 93 | `exit(code)` | 退出，退出码即进程退出码 |

未实现的调用返回 -1。也可以不 ecall，直接往 UART `0x1000_0000` 写字符、
往 sifive_test 写 `0x5555` 关机。S 态程序可用 `--sbi` 走内置 SBI，
或自带 trap handler 按标准异常处理 ecall。

## 用法

```sh
cargo build --release
./target/x86_64-unknown-linux-musl/release/riscv-emu [选项] <image>

  --trace   打印每条指令
  --stats   退出时打印指令数 / MIPS
  --mem N   RAM 大小（MB）
  --bin     把 image 当裸二进制加载到 0x8000_0000
  --sbi     S 态 ecall 按内置 SBI 处理

固件引导（QEMU 同款）：
  --bios <fw>            固件（fw_dynamic.bin/ELF）
  --kernel <image>       payload：ELF 按其地址加载，裸二进制放 0x8020_0000
  --dtb <dtb>            设备树（默认 board/virt.dtb）
```

## 编译测试程序

宿主机上 clang 自带 RISC-V 后端，无需安装交叉工具链：

```sh
clang --target=riscv64-unknown-elf -march=rv64imac_zicsr -mabi=lp64 \
      -mcmodel=medany -nostdlib -static -O2 -fuse-ld=lld \
      -Texamples/baremetal.ld -o examples/hello.elf examples/hello.c
```

两个注意点（都是 RISC-V 本身的特性，不是模拟器的限制）：

1. **必须 `-mcmodel=medany`**：RV64 的 `lui` 会把 32 位结果符号扩展，
   链接在 `0x8000_0000` 的代码用 medlow（默认）的 `%hi/%lo` 寻址会
   链接失败/得到负地址。medany 用 `auipc` PC 相对寻址，没有这个问题。
2. 链接脚本（`examples/baremetal.ld`）把所有段放进 RAM，并让 `.rodata`
   独占一页（避免 qemu-user 对照运行时同页段权限互相覆盖）。

examples/ 下的测试程序：
`hello.c`/`torture.c` 是阶段 1 的 M-mode 对照；`mini_mret.c` 是最小
M→S 切换；`minios.c` 是阶段 2 的全特性端到端；`sbi_min.c` 是阶段 3 的
OpenSBI payload（SBI ecall 打印/BASE/定时器/shutdown，链接脚本
`examples/sbi.ld` 放 0x8020_0000）。

### OpenSBI 引导差分

固件构建（v1.5，`~/Code/source/opensbi`；clang 23 需 sed 加
`-Wno-unused-but-set-variable`，不能命令行传 CFLAGS）：

```sh
make O=build PLATFORM=generic LLVM=1 CROSS_COMPILE=riscv64-unknown-elf- \
     FW_PEXT=imac -j8
# 产物 build/platform/generic/firmware/fw_dynamic.bin
```

注意 **QEMU 加载不了 fw_dynamic.elf**（其段链接在 0，QEMU 会警告并回退
raw 加载导致跑飞），必须用 `.bin`。payload 编译：

```sh
clang --target=riscv64-unknown-elf -march=rv64imac_zicsr -mabi=lp64 \
      -mcmodel=medany -nostdlib -static -O2 -fuse-ld=lld \
      -Texamples/sbi.ld -o examples/sbi_min.elf examples/sbi_min.c
```

差分命令（QEMU 的 `-cpu` 与 board/virt.dts 的裁剪一致；**尤其
sstc=off**：QEMU 默认 CPU 的 stimecmp 复位值为 0，会让 OpenSBI 交接后
STIP 恒挂起）：

```sh
CPU=rv64,f=off,d=off,v=off,h=off,zfa=off,zawrs=off,sstc=off,svadu=off,\
zicbom=off,zicbop=off,zicboz=off,zba=off,zbb=off,zbc=off,zbs=off,\
zihintntl=off,zihintpause=off,zihpm=off,sdtrig=off,sv48=off,sv57=off,sv39=on

FW=~/Code/source/opensbi/build/platform/generic/firmware/fw_dynamic.bin
./target/x86_64-unknown-linux-musl/release/riscv-emu \
    --bios "$FW" --kernel examples/sbi_min.elf --dtb board/virt.dtb
timeout 10 qemu-system-riscv64 -M virt -nographic -bios "$FW" \
    -kernel examples/sbi_min.elf -dtb board/virt.dtb -cpu $CPU
# 两边输出（OpenSBI banner + payload）与退出码 0 逐字节一致
```

已知输出差异：OpenSBI banner 中 `Firmware Heap/Scratch` 的 used 字节数
（两边探测到的硬件特性集合略有不同，misa 细节差异所致），payload 部分与
退出码完全一致。

## 设备树（board/virt.dts）

对齐 QEMU virt 的裁剪版：isa 固定 `rv64imac`、mmu-type 固定 Sv39（用
QEMU `-cpu` 关闭扩展后 dumpdtb 再手工编辑），删除本模拟器未实现的设备
节点（pci/virtio/goldfish-rtc/fw-cfg/flash/platform-bus，避免内核 probe
时 MMIO load fault）。memory 节点固定 128MB，与默认 `--mem 128` 一致。
修改后用 `dtc -I dts -O dtb -o board/virt.dtb board/virt.dts` 重编译。

## 启动 Linux（阶段 3 成果）

内核与 initramfs 的构建（宿主机 clang/LLVM 即可，无交叉工具链）：

```sh
# 内核：Linux 6.19-rc2，CONFIG_FPU=y（DTB isa=rv64imafdc）、SMP/EFI/NET 等关闭、
# 内嵌 initramfs（initramfs/initramfs.list 清单：busybox 静态二进制 + /init）
cd ~/Code/source/linux
make O=build-rv64-emu ARCH=riscv LLVM=1 defconfig
./scripts/config --file build-rv64-emu/.config -d SMP -d EFI -d NET -d PCI \
  -e FPU -e BLK_DEV_INITRD --set-str INITRAMFS_SOURCE <repo>/initramfs/initramfs.list
make O=build-rv64-emu ARCH=riscv LLVM=1 olddefconfig Image

# busybox：Debian riscv64 静态包（硬浮点 ABI，需 F/D）
curl -O https://mirrors.kernel.org/debian/pool/main/b/busybox/busybox-static_*_riscv64.deb
ar x busybox-static_*.deb && tar xf data.tar.xz   # usr/bin/busybox
```

引导：

```sh
./target/x86_64-unknown-linux-musl/release/riscv-emu \
    --bios "$FW" --kernel Image --dtb board/virt.dtb
# OpenSBI banner → 内核 dmesg → /init（挂载 proc/sys、busybox --install、
# 设置终端尺寸）→ "~ #" 交互 shell（stdin 已接入）
```

关键引导语义（都在内核/固件代码里可直接印证）：
- **amoswap.w/lr.w 结果符号扩展**——内核引用计数（i_writecount 等）依赖；
  曾因零扩展导致 exec 报 ETXTBSY
- **THRE 中断在 IER.1 使能沿立即挂起**（THR 恒空）——驱动 stop_tx/start_tx
  循环依赖，否则 tty 发送永久停摆
- **mstatus.FS 在 bits 14:13**——OpenSBI/kernel 切换浮点上下文依赖
- OpenSBI 把 CLINT 区 PMP 保护为 M-only，S 态读时间须用 `rdtime`

## 代码结构

```
src/
  main.rs        CLI：参数解析、固件引导流程、stdin 接入、诊断信息
  machine.rs     整机：CPU + 总线，运行循环（中断线同步），停机原因
  cpu.rs         hart：执行、ALU 语义、特权级/trap 交付、地址翻译与 PMP
  csr.rs         CSR 文件：M/S 两级 CSR、位域视图、pmpcfg/pmpaddr 后端
  fpu.rs         IEEE-754 F/D 核心（舍入/标志的无误差变换实现）
  decode.rs      RV64IMAC 指令解码（32 位 + 16 位压缩 → 规范形式）
  bus.rs         物理地址路由：RAM / MMIO
  mmu.rs         Sv39 走表 + 直接映射 TLB
  pmp.rs         PMP 规则（TOR/NA4/NAPOT、锁、判权）
  elf.rs         ELF64 加载器
  exception.rs   异常类型与 mcause 编码
  devices/       uart（NS16550）、clint（msip/mtime/mtimecmp）、
                 plic（96 源 M/S 双 context）、testdev（sifive_test）
board/           virt.dts/dtb（对齐 QEMU virt 的裁剪设备树）
initramfs/       内嵌 initramfs：清单、/init、busybox 静态二进制、setwinsize
tests/
  common/        最小指令编码器 + 裸机程序装配器
  spec.rs        规范一致性回归（每项对应一次审查发现的偏差）
  csr_matrix.rs  CSR 指令六形式矩阵 + WARL 探测 + trap 现场 + 计数器抑制
  bare_metal.rs  手工编码指令的集成测试（ALU/乘法/AMO/压缩指令/trap/ecall）
  priv.rs        特权级切换、委托、ecall 陷阱
  mmu.rs         Sv39 翻译单元测试（大页/权限/SUM/MXR/A-D 位）
  pmp.rs         PMP 集成测试（空规则 mret、TOR 只读、CSR 权限）
  timer.rs       CLINT 定时器中断（M 态直收 / OpenSBI 风格转发 S 态）
  plic.rs        PLIC：UART RX 中断 M/S 交付、claim/complete、阈值、pending
  sbi.rs         内置 SBI 冒烟测试
  opensbi.rs     真 OpenSBI 引导冒烟测试（设 OPENSBI_FW 后启用）
  fpu.rs         F/D 数值语义单元测试（舍入/标志/NaN/转换）
  fp_ctx.rs      FP 保存/恢复与内存完整性（内核 fstate 模式）
```

## 使用教程

### 1. 裸机程序（阶段 1/2，无需固件）

```sh
cargo build --release
./target/x86_64-unknown-linux-musl/release/riscv-emu examples/minios.elf
./target/x86_64-unknown-linux-musl/release/riscv-emu --stats examples/torture.elf
```

### 2. OpenSBI + Linux + busybox shell

前置（一次性）：
- 内核 Image：见上文"启动 Linux"一节的构建命令（`build-rv64-emu/arch/riscv/boot/Image`）。
  **注意**：内核不含内嵌 initramfs，必须配 `--initrd`
- OpenSBI 固件：`fw_dynamic.bin`（构建命令见上文）

```sh
EMU=./target/x86_64-unknown-linux-musl/release/riscv-emu
FW=~/Code/source/opensbi/build/platform/generic/firmware/fw_dynamic.bin
KERNEL=~/Code/source/linux/build-rv64-emu/arch/riscv/boot/Image

$EMU --bios "$FW" --kernel "$KERNEL" --initrd initramfs/busybox.cpio --dtb board/virt.dtb
# 引导至 busybox 交互 shell（stdin 已接通，可输入命令），
# 退出：poweroff -f
# busybox.cpio 可用 gen_init_cpio 重新生成：
# ~/Code/source/linux/build-rv64-emu/usr/gen_init_cpio initramfs/initramfs.list > initramfs/busybox.cpio
```

### 3. Alpine + musl + tcc：在模拟器里编译程序

```sh
# 一次性：生成 Alpine rootfs initramfs（下载 ~8MB，无需 root）
board/alpine/build-rootfs.sh /tmp/rootfs.cpio

$EMU --bios "$FW" --kernel "$KERNEL" --initrd /tmp/rootfs.cpio \
     --dtb board/virt.dtb --mem 512
```

`/init` 会自动在 VM 内执行：`tcc hello.c -o hello && ./hello`，
输出 "Hello, world!"（在 VM 里现场编译，非预编译），随后自动关机。
想手动交互可编辑 `board/alpine/init.tmpl`（末行 `poweroff -f` 删掉即可得 shell）。

### 4. 通用选项

| 选项 | 说明 |
|---|---|
| `--mem <MB>` | RAM 大小（默认 128；Alpine 流程用 512） |
| `--initrd <cpio>` | 未压缩 newc 格式 initramfs（放 DRAM_BASE+64MB，回填 DTB） |
| `--trace` | 逐条指令跟踪 |
| `--stats` | 退出时打印指令数/用时/MIPS |
| `--sbi` | 内置 SBI（不用 OpenSBI 跑 S 态程序时用） |

## 路线图（后续阶段）

1. ~~特权级与 MMU~~、~~中断~~、~~SBI~~（阶段 2）
2. ~~PLIC~~、~~16550 RX~~、~~OpenSBI fw_dynamic 引导~~、~~F/D 浮点~~、
   ~~Linux 6.19-rc2 + busybox shell~~（阶段 3 完成 ✅）
3. **virtio-blk**（磁盘 rootfs，替代 initramfs）/ virtio-net
4. **性能**：取指页缓存 + 解码缓存已落地（13 → 23 MIPS，Linux 启动
   95s → 53s）；下一步：翻译直通缓存与执行闭包缓存
5. 多核（SMP harts + MSWI/ACLINT）

## 测试

```sh
cargo test            # 单元测试 + 集成测试（15 套）
cargo clippy          # lint
# 可选：OpenSBI 冒烟测试
OPENSBI_FW=~/Code/source/opensbi/build/platform/generic/firmware/fw_dynamic.bin \
    cargo test --test opensbi
```
