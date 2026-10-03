# riscv-emu

[中文说明](README.zh-CN.md)

A RISC-V full-system emulator written in Rust. Goal: boot a real Linux
userspace entirely inside the emulator — verified step by step against
QEMU behavior.

**Current status: boots OpenSBI v1.5 → Linux 6.19-rc2 → an interactive
BusyBox shell, and compiles & runs C programs in-VM with Alpine + musl +
TinyCC.** Everything a real RISC-V machine does at this level —
privileged architecture, Sv39 paging, PMP, PLIC, serial — is implemented
from the spec (Privileged ISA v1.13) and differential-tested against QEMU.

## Quick start

```sh
cargo build --release      # build the emulator (host clang only, no cross toolchain)

./linux.sh                 # OpenSBI + Linux + BusyBox interactive shell
./alpine.sh                # Alpine + musl + tcc: compile hello.c *inside* the VM
```

Both scripts build whatever is missing on first run (git clone + compile,
~5–10 min; sources go to `dist/src/`, artifacts to `dist/`, all
gitignored). If `~/Code/source/{opensbi,linux}` already exist they are
reused as-is. Later runs start instantly from cached artifacts.

`./alpine.sh` runs a canned demo inside the VM (`tcc hello.c -o hello &&
./hello` → prints `Hello, world!`, ~1.4 s of guest time) and then drops
into an Alpine shell with busybox + musl + tcc. Edit
`board/alpine/init.tmpl` to change what runs; `poweroff -f` shuts down.

## Feature set

- **ISA**: RV64IMAFC + Zicsr + Zifencei + F/D floating point
  - full F/D instruction set, all five rounding modes, accurate `fflags`
    (inexact/underflow via error-free transformations: two-sum / FMA
    residual), canonical NaN, f32 NaN-boxing
- **Privilege**: M/S/U, standard trap delivery (vectored mtvec/stvec),
  mret/sret, MPRV/SUM/MXR/TVM/TW/TSR, mstatus.FS tracking + SD synthesis
- **Sv39 MMU**: 3-level walk, superpages, U/S perms with SUM/MXR,
  A/D auto-update, direct-mapped TLB, reserved-PTE checks, PMP-checked
  page-table walks (S-mode effective privilege, original access type on
  fault)
- **PMP**: 16 entries, G=0 (NA4 supported), PA width 56 → 54 implemented
  address bits; first-match-any-byte-overlap semantics per §3.7.1.3;
  L bit enforced on all modes; locked TOR locks the previous pmpaddr;
  WARL canonicalization on write
- **CSRs**: audited against Privileged ISA v1.13 — mip writability split
  from sip visibility (delegation subset), CSRRS/CSRRC on mip uses
  software bits only, satp/mtvec/medeleg/MPP/mcountinhibit WARL,
  TVM gates satp access, misa writable with format-based F/D gating,
  mcycle/minstret independently countable and inhibited
- **Interrupts**: CLINT (msip/mtime/mtimecmp), PLIC (96 sources, M/S
  contexts, thresholds, claim/complete, latched pending + gateway),
  external lines synced into mip each step
- **Devices**: NS16550 UART (RX side + IRQ → PLIC source 10), sifive_test
- **Boot**: real OpenSBI `fw_dynamic` (a0/a1/a2 + fw_dynamic_info v2),
  ELF/binary kernel, external initramfs with runtime DTB patching
- **Host console**: stdin is piped into the UART RX, so the guest shell
  is fully interactive

## Repository layout

```
src/
  main.rs        CLI, firmware boot flow, stdin injection, diagnostics
  machine.rs     machine: CPU + bus, run loop (IRQ line sync), halt reasons
  cpu.rs         hart: execute, ALU, privilege/trap delivery, translation+PMP
  csr.rs         CSR file (M/S), view registers, WARL canonicalization
  decode.rs      RV64IMAFC decoding (32-bit + compressed → canonical form)
  fpu.rs         IEEE-754 F/D core (rounding/flags via error-free transforms)
  bus.rs         physical address routing: RAM / MMIO
  mmu.rs         Sv39 walk + direct-mapped TLB + walk-time PMP
  pmp.rs         16-entry PMP (TOR/NA4/NAPOT, locks, first-match)
  elf.rs         ELF64 loader (QEMU-compatible: paddr-based, vmlinux aware)
  exception.rs   exception types / mcause codes
  devices/       uart (16550), clint, plic, testdev (sifive_test)
board/           virt.dts/dtb (QEMU-virt-aligned, trimmed) + Alpine rootfs
initramfs/       busybox initramfs builder (downloads Debian busybox-static)
dist/            build scripts + fetched artifacts (gitignored)
tests/           16 suites: ISA, priv, mmu, pmp, timer, plic, sbi, fpu,
                 fp_ctx, spec (spec-conformance regressions), csr_matrix…
```

## Building the world (manual, optional)

`linux.sh` / `alpine.sh` do all of this automatically. To run the steps
by hand:

```sh
# OpenSBI v1.5 (clang works; a -Wno for unused-but-set is applied by script)
git clone --depth 1 --branch v1.5 \
    https://github.com/riscv-software-src/opensbi.git
make -C opensbi O=build PLATFORM=generic LLVM=1 FW_PEXT=imac -j$(nproc)
cp opensbi/build/platform/generic/firmware/fw_dynamic.bin dist/

# Linux v6.19-rc2 (config: dist/linux.config — RV64IMAFC, Sv39, 8250,
# no SMP/EFI/NET/PCI; embedded initramfs empty, initrd comes via --initrd)
git clone --depth 1 --branch v6.19-rc2 \
    https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git
sh dist/build-all.sh linux    # or replicate: olddefconfig + Image, see script

# BusyBox initramfs (static busybox from Debian; no root needed)
initramfs/build-busybox-initramfs.sh

# Boot
./target/x86_64-unknown-linux-musl/release/riscv-emu \
    --bios dist/fw_dynamic.bin --kernel dist/Image \
    --initrd initramfs/busybox.cpio --dtb board/virt.dtb
```

### Alpine + tcc (compile inside the VM)

```sh
board/alpine/build-rootfs.sh dist/rootfs.cpio
./target/x86_64-unknown-linux-musl/release/riscv-emu \
    --bios dist/fw_dynamic.bin --kernel dist/Image \
    --initrd dist/rootfs.cpio --dtb board/virt.dtb --mem 512
```

The initramfs contains Alpine 3.22 userland, musl + headers, and tcc
(small C compiler). `/init` compiles and runs `hello.c` in the VM, then
drops to a shell. Full rootfs ≈ 34 MB cpio; the VM needs `--mem 512`.

## CLI reference

```
riscv-emu [--trace] [--stats] [--mem <MB>] [--bin] [--sbi] <image>
riscv-emu --bios <fw> [--kernel <image>] [--initrd <cpio>] [--dtb <dtb>]
          [--trace] [--stats] [--mem <MB>]
```

## Differential testing methodology

Every layer is validated against QEMU (`qemu-system-riscv64 -M virt`,
v11.1) running the *same* firmware/kernel/DTB with CPU extensions trimmed
to what the emulator implements:

- OpenSBI banner + payload output: **byte-identical**
- Linux dmesg: identical except timing self-measurements (raid6/xor
  benchmarks) and honest capability differences (no H extension → no
  ELF compat mode)
- 16 test suites (`cargo test`) cover ISA semantics, privilege
  architecture, Sv39, PMP, interrupt controllers, F/D numerics and
  spec-conformance regressions — each regression traces back to a
  specific audit finding (see `tests/spec.rs` comments)

Notable conformance-sensitive details (all verified against the spec
text, not just QEMU): AMO 32-bit results sign-extended; SUM has no
effect on instruction fetches; mstatus.SD read-only synthesis;
mstatus.SPP records the real previous privilege on M→S traps; failed
SC invalidates the reservation; `ESC[6n`-style terminal quirks are the
guest's business, not the emulator's.

## Known limitations

- Single hart; no hypervisor/AIA/Svnapot/Svpbmt
- `mcycle` advances at 1 CPI (no independent clock model); F/D flags are
  exact except FMADD (two-sum approximation) and results below the
  smallest subnormal (documented in `src/fpu.rs`)
- No virtio devices yet (rootfs comes via initramfs); ~20 MIPS

## License

MIT (see `LICENSE-MIT`). Guest software keeps its own licenses:
Linux GPLv2, OpenSBI BSD-2-Clause, BusyBox GPLv2, Alpine/musl MIT,
tcc LGPLv2.1 — none of it is redistributed in this repository; the
build scripts fetch it from upstream.
