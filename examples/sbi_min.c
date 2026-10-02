// OpenSBI payload 最小验证程序（S 态入口，需 OpenSBI 提供的 SBI 环境）：
//   legacy console_putchar(a7=1) 打印
//   BASE(a7=0x10, fid=0) 读 spec_version
//   legacy sbi_set_timer(a7=0) 设定时器 → OpenSBI MTIP 转发 STIP
//   legacy sbi_shutdown(a7=8) 关机（OpenSBI → sifive_test → 宿主退出 0）
//
// 两种引导下 payload 输出与退出码应完全一致：
//   1) riscv-emu --bios fw_dynamic.bin --kernel sbi_min.elf --dtb board/virt.dtb
//   2) qemu-system-riscv64 -M virt -nographic -bios fw_dynamic.bin \
//        -kernel sbi_min.elf -dtb board/virt.dtb \
//        -cpu rv64,f=off,d=off,v=off,h=off,zfa=off,zawrs=off,sstc=off,svadu=off,\
// zicbom=off,zicbop=off,zicboz=off,zba=off,zbb=off,zbc=off,zbs=off,\
// zihintntl=off,zihintpause=off,zihpm=off,sdtrig=off,sv48=off,sv57=off,sv39=on
//      （-cpu 与 board/virt.dts 生成时一致；尤其 sstc=off：QEMU 默认 CPU 的
//       stimecmp 复位值为 0 会令 OpenSBI 交接后 STIP 恒挂起）
//
// OpenSBI 已配好：mideleg（STIP/SSIP 委派）、PMP（S 态全空间）、mcounteren。
// payload 只需 stvec、sie.STIE、sstatus.SIE。

typedef unsigned long u64;

#define csr_read(csr)                                                              \
    ({                                                                            \
        u64 v_;                                                                  \
        asm volatile("csrr %0, " #csr : "=r"(v_));                                    \
        v_;                                                                     \
    })
#define csr_set(csr, v) asm volatile("csrs " #csr ", %0" ::"r"(v))
#define csr_write(csr, v) asm volatile("csrw " #csr ", %0" ::"r"(v))

// legacy console_putchar：SBI v0.1 调用无错误码，值直接在 a0
static void putc_(char c) {
    register u64 a0_ asm("a0") = (u64)c;
    register u64 a7_ asm("a7") = 1;
    asm volatile("ecall" ::"r"(a0_), "r"(a7_) : "memory");
}

static void puts_(const char *s) {
    while (*s)
        putc_(*s++);
}

static void put_hex(u64 v) {
    static const char d[] = "0123456789abcdef";
    puts_("0x");
    for (int i = 60; i >= 0; i -= 4)
        putc_(d[(v >> i) & 0xF]);
    puts_("\n");
}

// v0.2+ 调用：错误码在 a0，值在 a1（fid 由 a6 传）
static long sbi_call(u64 a7, u64 fid, u64 a0, u64 *val) {
    register u64 a0_ asm("a0") = a0;
    register u64 a1_ asm("a1");
    register u64 a6_ asm("a6") = fid;
    register u64 a7_ asm("a7") = a7;
    asm volatile("ecall" : "+r"(a0_), "=r"(a1_) : "r"(a6_), "r"(a7_) : "memory");
    if (val)
        *val = a1_;
    return (long)a0_;
}

volatile int timer_hits;
volatile u64 last_scause;

__attribute__((used)) static void s_trap_body(void) {
    last_scause = csr_read(scause);
    if (last_scause == (1ul << 63 | 5)) { // S 态定时器中断（OpenSBI 转发）
        timer_hits++;
        // 重新 arm 到远期：mtimecmp 不清的话 MTIP 电平恒挂起，OpenSBI 会
        // 不断重新置 STIP（SBI 契约：S 每次收到 timer 都要重新 set_timer）
        sbi_call(0, 0, ~0ul, 0);
    } else if (last_scause >> 63) {
        csr_write(sie, 0); // 未知中断：关 sie 兜底，防风暴
    }
}

// trap handler 必须保存/恢复被打断代码的寄存器（sret 回原指令继续执行）
__attribute__((naked, aligned(4))) static void s_trap(void) {
    asm volatile(
        "addi sp, sp, -128\n"
        "sd ra, 0(sp)\n"
        "sd t0, 16(sp)\n"
        "sd t1, 24(sp)\n"
        "sd t2, 32(sp)\n"
        "sd a0, 40(sp)\n"
        "sd a1, 48(sp)\n"
        "sd a2, 56(sp)\n"
        "sd a3, 64(sp)\n"
        "sd a4, 72(sp)\n"
        "sd a5, 80(sp)\n"
        "sd a6, 88(sp)\n"
        "sd a7, 96(sp)\n"
        "call s_trap_body\n"
        "ld ra, 0(sp)\n"
        "ld t0, 16(sp)\n"
        "ld t1, 24(sp)\n"
        "ld t2, 32(sp)\n"
        "ld a0, 40(sp)\n"
        "ld a1, 48(sp)\n"
        "ld a2, 56(sp)\n"
        "ld a3, 64(sp)\n"
        "ld a4, 72(sp)\n"
        "ld a5, 80(sp)\n"
        "ld a6, 88(sp)\n"
        "ld a7, 96(sp)\n"
        "addi sp, sp, 128\n"
        "sret\n");
}

// used：仅被 _start 的内联汇编引用，防止编译器丢弃
__attribute__((used)) static char boot_stack[4096] __attribute__((aligned(16)));

__attribute__((naked, section(".text.init"))) void _start(void) {
    // OpenSBI 进 payload 时 a0=hartid a1=FDT，sp 未定，需自备
    asm volatile("la sp, boot_stack + 4096\n"
                 "tail s_main\n");
}

void s_main(void) {
    puts_("P: hello from S-mode\n");
    put_hex(csr_read(sstatus)); // S 态可读，输出 MPP/SPIE 等位（两边应一致）

    csr_write(stvec, (u64)s_trap);
    csr_write(sie, 1ul << 5);                  // SIE.STIE
    csr_set(sstatus, (1ul << 1) | (1ul << 5)); // SIE | SPIE

    // SBI BASE spec_version：OpenSBI v1.5 返回 2（SBI 2.0）
    u64 ver = 0;
    long err = sbi_call(0x10, 0, 0, &ver);
    puts_("P: sbi spec_version: err=");
    put_hex((u64)err);
    put_hex(ver);

    // 定时器：约 2ms 后（10MHz → 20000 ticks），legacy sbi_set_timer。
    // 读时间用 rdtime（OpenSBI 已设 mcounteren.TM）；CLINT MMIO 被 OpenSBI
    // PMP 保护为 M-only（Domain0 Region02），S 态直读会 access fault。
    u64 mtime;
    asm volatile("rdtime %0" : "=r"(mtime));
    sbi_call(0, 0, mtime + 20000, 0);

    while (!timer_hits)
        ;
    puts_("P: timer ok, scause=");
    put_hex(last_scause);

    puts_("P: shutdown\n");
    sbi_call(8, 0, 0, 0); // legacy shutdown
    for (;;)
        ;
}
