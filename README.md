# riscv-emu

用 Rust 写的 RISC-V 全系统模拟器。目标（长线）：启动 Linux 内核 + busybox。

当前是**阶段 3**：真固件引导（OpenSBI fw_dynamic）+ PLIC + 16550 接收中断。
`--bios fw_dynamic.bin --kernel payload.elf --dtb board/virt.dtb` 引导真 OpenSBI
v1.5 进入 S 态 payload，与 `qemu-system-riscv64 -M virt`（QEMU 11.1.1，同一
固件与 DTB，`-cpu` 关闭本模拟器未实现的扩展）输出与退出码**逐字节一致**。
阶段 2 的特权级/Sv39/PMP/定时器与阶段 1 的裸机对照测试依旧全绿。

## 已支持

- ISA：RV64IMAC + Zicsr + Zifencei（压缩指令解码为规范形式；F/D 暂未实现，
  编译时用 `-march=rv64imac` 即可）
- 特权级：M/S/U 三级，标准 trap 交付（mtvec/stvec、vectored 模式）、
  medeleg/mideleg 委托、mret/sret、MPRV/SUM/MXR/TVM/TW/TSR
- Sv39 MMU：三级走表、大页（对齐检查）、U/S 权限与 SUM/MXR、A/D 位自动置位、
  直接映射 TLB（satp 写与 sfence.vma 冲刷）；跨页 misaligned 访问按两段
  分别翻译/PMP 检查；AMO 要求自然对齐（对齐 QEMU 的 misaligned 异常语义）
- PMP：16 项规则（TOR/NA4/NAPOT）、L 锁定、非 M 态取指/读写/AMO 检查；
  空 PMP 下 mret 进低特权级在 mret 处抛 instruction access fault（对齐 QEMU）
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
- CSR 面：mcounteren/scounteren（U/S 计数器访问权限）、mcountinhibit、
  menvcfg/senvcfg 存储位；misa = RV64 IMA C S U（OpenSBI 用 CSR 读探测
  特权级版本与扩展，本实现可探测到 priv 1.12）
- 内存布局对齐 QEMU `virt` 机器：

  | 区域 | 范围 | 说明 |
  |---|---|---|
  | sifive_test | `0x0010_0000` | 写 `0x5555` 正常关机、`0x3333` 失败关机 |
  | CLINT | `0x0200_0000` | msip、mtime（10MHz）/mtimecmp |
  | PLIC | `0x0C00_0000` | 优先级/pending/enable/threshold/claim |
  | UART | `0x1000_0000` | NS16550，中断接 PLIC 源 10 |
  | RAM | `0x8000_0000` | 默认 128MB，`--mem` 可调 |

- ELF64 加载器（静态、非 PIE、`ET_EXEC`；`ET_DYN` 按 p_vaddr 原地装入，
  同 QEMU load_elf，兼容 OpenSBI 固件 ELF）

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

## 代码结构

```
src/
  main.rs        CLI：参数解析、固件引导流程、诊断信息
  machine.rs     整机：CPU + 总线，运行循环（中断线同步），停机原因
  cpu.rs         hart：执行、ALU 语义、特权级/trap 交付、地址翻译与 PMP
  csr.rs         CSR 文件：M/S 两级 CSR、位域视图、pmpcfg/pmpaddr 后端
  decode.rs      RV64IMAC 指令解码（32 位 + 16 位压缩 → 规范形式）
  bus.rs         物理地址路由：RAM / MMIO
  mmu.rs         Sv39 走表 + 直接映射 TLB
  pmp.rs         PMP 规则（TOR/NA4/NAPOT、锁、判权）
  elf.rs         ELF64 加载器
  exception.rs   异常类型与 mcause 编码
  devices/       uart（NS16550）、clint（msip/mtime/mtimecmp）、
                 plic（96 源 M/S 双 context）、testdev（sifive_test）
board/           virt.dts/dtb（对齐 QEMU virt 的裁剪设备树）
tests/
  common/        最小指令编码器 + 裸机程序装配器
  bare_metal.rs  手工编码指令的集成测试（ALU/乘法/AMO/压缩指令/trap/ecall）
  priv.rs        特权级切换、委托、ecall 陷阱
  mmu.rs         Sv39 翻译单元测试（大页/权限/SUM/MXR/A-D 位）
  pmp.rs         PMP 集成测试（空规则 mret、TOR 只读、CSR 权限）
  timer.rs       CLINT 定时器中断（M 态直收 / OpenSBI 风格转发 S 态）
  plic.rs        PLIC：UART RX 中断 M/S 交付、claim/complete、阈值、pending
  sbi.rs         内置 SBI 冒烟测试
  opensbi.rs     真 OpenSBI 引导冒烟测试（设 OPENSBI_FW 后启用）
```

## 路线图（后续阶段）

1. ~~特权级与 MMU~~、~~中断~~、~~SBI~~（阶段 2）
2. ~~PLIC~~、~~16550 RX~~、~~OpenSBI fw_dynamic 引导~~（阶段 3 完成 OpenSBI
   差分对齐）
3. **引导 Linux 6.19-rc2 + busybox**（initramfs；Linux 需要 PLIC/CLINT/
   UART/`--bios`，都已就位）
4. **F/D 扩展**：浮点
5. **virtio-blk / virtio-net**（磁盘、网络）

## 测试

```sh
cargo test            # 单元测试 + 集成测试
cargo clippy          # lint
# 可选：OpenSBI 冒烟测试
OPENSBI_FW=~/Code/source/opensbi/build/platform/generic/firmware/fw_dynamic.bin \
    cargo test --test opensbi
```
