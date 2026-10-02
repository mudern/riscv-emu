# riscv-emu

用 Rust 写的 RISC-V 全系统模拟器。目标（长线）：启动 Linux 内核 + busybox。

当前是**阶段 2**：完整特权级架构（M/S/U）+ Sv39 分页 + PMP + 定时器中断 +
内置 SBI。`examples/minios.c`（自建页表、M→S→U、ecall/page fault/定时器中断）
在我的模拟器与 `qemu-system-riscv64 -M virt -bios none`（QEMU 11）下输出
完全一致。阶段 1 的裸机 M-mode 程序（`hello`/`torture`）与 `qemu-riscv64`
对照仍然一致。

## 已支持

- ISA：RV64IMAC + Zicsr + Zifencei（压缩指令解码为规范形式；F/D 暂未实现，
  编译时用 `-march=rv64imac` 即可；rv64gc 程序只要不执行浮点指令也能跑）
- 特权级：M/S/U 三级，标准 trap 交付（mtvec/stvec、vectored 模式）、
  medeleg/mideleg 委托、mret/sret、MPRV/SUM/MXR/TVM/TW/TSR
- Sv39 MMU：三级走表、大页（对齐检查）、U/S 权限与 SUM/MXR、A/D 位自动置位、
  直接映射 TLB（satp 写与 sfence.vma 冲刷）
- PMP：16 项规则（TOR/NA4/NAPOT）、L 锁定、非 M 态取指/读写/AMO 检查；
  空 PMP 下 mret 进低特权级在 mret 处抛 instruction access fault（对齐 QEMU）
- 中断：CLINT mtime/mtimecmp 定时器中断（MTIP），软件中断位（SSIP/STIP/SEIP
  按 mideleg 委派可写），中断优先级与目标特权级投递
- 内置 SBI（`--sbi`）：S 态 ecall 按 SBI v1 处理（BASE/TIME/IPI/RFENCE/SRST/DBCN
  + legacy putchar/shutdown），便于不引 OpenSBI 直接跑 S 态内核
- 内存布局对齐 QEMU `virt` 机器（为启动 OpenSBI/Linux 预留）：

  | 区域 | 范围 | 说明 |
  |---|---|---|
  | sifive_test | `0x0010_0000` | 写 `0x5555` 正常关机、`0x3333` 失败关机 |
  | CLINT | `0x0200_0000` | mtime（10MHz）/mtimecmp，定时器中断 |
  | UART | `0x1000_0000` | NS16550，写 THR 输出字符 |
  | RAM | `0x8000_0000` | 默认 128MB，`--mem` 可调 |

- ELF64 加载器（静态、非 PIE、`ET_EXEC`）

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

examples/ 下的测试程序（`hello.c`/`torture.c` 是阶段 1 的 M-mode 对照，
`mini_mret.c` 是最小 M→S 切换，`minios.c` 是阶段 2 的全特性端到端）：
`minios` 在模拟器与 QEMU system 模式下输出应完全一致：

```sh
./target/x86_64-unknown-linux-musl/release/riscv-emu examples/minios.elf
qemu-system-riscv64 -M virt -bios none -nographic -kernel examples/minios.elf
# 两边输出与退出码应完全一致

./target/x86_64-unknown-linux-musl/release/riscv-emu examples/torture.elf
qemu-riscv64 examples/torture.elf   # 阶段 1 对照，同样应一致
```

## 代码结构

```
src/
  main.rs        CLI：参数解析、加载、运行、诊断信息
  machine.rs     整机：CPU + 总线，运行循环，停机原因
  cpu.rs         hart：执行、ALU 语义、特权级/trap 交付、地址翻译与 PMP 检查、ecall/SBI
  csr.rs         CSR 文件：M/S 两级 CSR、位域视图（sie/sip）、pmpcfg/pmpaddr 后端
  decode.rs      RV64IMAC 指令解码（32 位 + 16 位压缩 → 规范形式）
  bus.rs         物理地址路由：RAM / MMIO
  mmu.rs         Sv39 走表 + 直接映射 TLB
  pmp.rs         PMP 规则（TOR/NA4/NAPOT、锁、判权）
  elf.rs         ELF64 加载器
  exception.rs   异常类型与 mcause 编码
  devices/       uart（NS16550）、clint（mtime/mtimecmp）、testdev（sifive_test）
tests/
  common/        最小指令编码器 + 裸机程序装配器
  bare_metal.rs  手工编码指令的集成测试（ALU/乘法/AMO/压缩指令/trap/ecall）
  priv.rs        特权级切换、委托、ecall 陷阱
  mmu.rs         Sv39 翻译单元测试（大页/权限/SUM/MXR/A-D 位）
  pmp.rs         PMP 集成测试（空规则 mret、TOR 只读、CSR 权限）
  timer.rs       CLINT 定时器中断（M 态直收 / OpenSBI 风格转发 S 态）
  sbi.rs         内置 SBI 冒烟测试
```

## 路线图（后续阶段）

1. ~~特权级与 MMU~~、~~中断~~、~~SBI~~（阶段 2 已完成）
2. **更多设备**：PLIC、virtio-blk / virtio-net（磁盘、网络）
3. **F/D 扩展**：浮点
4. **启动真 OpenSBI → Linux → busybox**（`-bios opensbi.fw` 直接引导）

## 测试

```sh
cargo test            # 单元测试 + 集成测试
cargo clippy          # lint
```
