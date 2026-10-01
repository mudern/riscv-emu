# riscv-emu

用 Rust 写的 RISC-V 全系统模拟器。目标（长线）：启动 Linux 内核 + busybox。

当前是**阶段 1**：能直接运行你交叉编译的 RV64 裸机二进制（ELF）。
已在真实 clang 交叉编译的程序上与 `qemu-riscv64` 做过输出/退出码一致性对照。

## 已支持

- ISA：RV64IMAC + Zicsr + Zifencei（压缩指令解码为规范形式；F/D 暂未实现，
  编译时用 `-march=rv64imac` 即可；rv64gc 程序只要不执行浮点指令也能跑）
- 特权级：M-mode，标准 mtvec/mepc/mcause/mtval 异常交付，mret，常用 CSR
- 内存布局对齐 QEMU `virt` 机器（为启动 OpenSBI/Linux 预留）：

  | 区域 | 范围 | 说明 |
  |---|---|---|
  | sifive_test | `0x0010_0000` | 写 `0x5555` 正常关机、`0x3333` 失败关机 |
  | CLINT | `0x0200_0000` | mtime（10MHz）/mtimecmp（定时器中断后续实现） |
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
往 sifive_test 写 `0x5555` 关机。这个约定以后会被真正的 SBI（S-mode ecall）取代。

## 用法

```sh
cargo build --release
./target/x86_64-unknown-linux-musl/release/riscv-emu [选项] <image>

  --trace   打印每条指令
  --stats   退出时打印指令数 / MIPS
  --mem N   RAM 大小（MB）
  --bin     把 image 当裸二进制加载到 0x8000_0000
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

examples/ 下有 `hello.c`（基础输出/退出）和 `torture.c`（递归、乘除、
访存校验和），两者在 qemu-riscv64 下行为一致，可作回归对照：

```sh
./target/x86_64-unknown-linux-musl/release/riscv-emu examples/torture.elf
qemu-riscv64 examples/torture.elf   # 输出和退出码应完全一致
```

## 代码结构

```
src/
  main.rs        CLI：参数解析、加载、运行、诊断信息
  machine.rs     整机：CPU + 总线，运行循环，停机原因
  cpu.rs         hart：执行、ALU 语义、CSR 文件、异常交付、ecall 宿主调用
  decode.rs      RV64IMAC 指令解码（32 位 + 16 位压缩 → 规范形式）
  bus.rs         物理地址路由：RAM / MMIO
  elf.rs         ELF64 加载器
  exception.rs   异常类型与 mcause 编码
  devices/       uart（NS16550）、clint、testdev（sifive_test）
tests/
  bare_metal.rs  手工编码指令的集成测试（ALU/乘法/AMO/压缩指令/trap/ecall）
```

## 路线图（后续阶段）

1. **特权级与 MMU**：S-mode/U-mode、mstatus 完整语义、Sv39 分页 → 跑 Linux
2. **中断**：CLINT 定时器中断、PLIC → 时钟与设备中断
3. **SBI**：实现 OpenSBI 的 S-mode ecall 接口（取代阶段 1 的 ecall 约定）
4. **更多设备**：virtio-blk / virtio-net（磁盘、网络）
5. **F/D 扩展**：浮点
6. 启动 OpenSBI → Linux → busybox

## 测试

```sh
cargo test            # 单元测试 + 集成测试
cargo clippy          # lint
```
