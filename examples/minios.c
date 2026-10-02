// mini OS：M 态引导 → 建 Sv39 页表 → S 态 → U 态用户程序 →
// ecall 陷入 / page fault / 定时器中断（OpenSBI 风格转发）→ 汇报结果。
//
// 在我的模拟器与 qemu-system-riscv64（-M virt -bios none）下输出应完全一致：
//
//   clang --target=riscv64-unknown-elf -march=rv64imac_zicsr -mabi=lp64 \
//         -mcmodel=medany -nostdlib -static -O2 -fuse-ld=lld \
//         -Texamples/baremetal.ld -o examples/minios.elf examples/minios.c
//
//   ./riscv-emu examples/minios.elf
//   qemu-system-riscv64 -M virt -bios none -nographic -kernel examples/minios.elf
//
// 虚拟内存布局（Sv39）：
//   VA 0x0000_0000 .. 0x3FFF_FFFF  → PA 恒等（MMIO：UART/test/CLINT），S RW
//   VA 0x8000_0000 .. 0xBFFF_FFFF  → PA 恒等（内核 + RAM），S RWX
//   VA 0x4040_0000                 → U 代码页（拷贝自内核 .text.user）
//   VA 0x4040_1000                 → U 栈页

typedef unsigned long u64;
typedef unsigned int u32;

void *memcpy(void *dst, const void *src, unsigned long n) {
    char *d = dst;
    const char *s = src;
    while (n--)
        *d++ = *s++;
    return dst;
}

#define UART0 ((volatile unsigned char *)0x10000000)
#define TEST_DEV ((volatile u32 *)0x100000)
#define MTIMECMP ((volatile u64 *)0x02004000)
#define MTIME ((volatile u64 *)0x0200BFF8)

#define csr_read(csr)                                                              \
    ({                                                                             \
        u64 v_;                                                                   \
        asm volatile("csrr %0, " #csr : "=r"(v_));                                 \
        v_;                                                                       \
    })
#define csr_write(csr, v) asm volatile("csrw " #csr ", %0" ::"r"(v))
#define csr_set(csr, v) asm volatile("csrs " #csr ", %0" ::"r"(v))

static void puts_(const char *s) {
    while (*s)
        *UART0 = *s++;
}
static void put_hex(u64 v) {
    static const char d[] = "0123456789abcdef";
    *UART0 = '0';
    *UART0 = 'x';
    for (int i = 60; i >= 0; i -= 4)
        *UART0 = d[(v >> i) & 0xF];
    *UART0 = '\n';
}

__attribute__((aligned(4096))) static u64 pt_l2[512];
__attribute__((aligned(4096))) static u64 pt_l1_user[512];
__attribute__((aligned(4096))) static u64 pt_l0_user[512];
__attribute__((aligned(4096))) static char user_code_page[4096];
__attribute__((aligned(4096))) static char user_stack_page[4096];
char s_stack[8192]; // 全局符号供入口 asm 引用

#define PTE_V (1ul << 0)
#define PTE_R (1ul << 1)
#define PTE_W (1ul << 2)
#define PTE_X (1ul << 3)
#define PTE_U (1ul << 4)
#define PTE_A (1ul << 6)
#define PTE_D (1ul << 7)

static void map(u64 *table, int idx, u64 phys, u64 flags) {
    table[idx] = ((u64)phys >> 12) << 10 | flags;
}

static volatile int u_done, timer_hits, fault_seen;
static volatile u64 echo_val, last_scause;
volatile u64 trap_a0, trap_a7; // 顶层 asm 引用，需全局符号

// S 态 trap 入口桩：先把 a0/a7 存到全局（避免 C 代码把它俩挪用后再读）
__asm__(
    ".align 4\n"
    ".globl s_trap_entry\n"
    "s_trap_entry:\n"
    "   la t0, trap_a0\n"
    "   sd a0, 0(t0)\n"
    "   sd a7, 8(t0)\n"
    "   j s_trap\n");

// U 态代码放在独立 section，引导时拷到用户页（代码位置无关：只用绝对常量）
__attribute__((section(".text.user"), noinline)) void u_entry(void) {
    volatile u64 *up = (volatile u64 *)0x40400000;
    *up = 0x1234;

    register u64 a0 asm("a0") = *up;
    register u64 a7 asm("a7") = 1; // sys_echo: 返回 a0+1
    asm volatile("ecall" : "+r"(a0) : "r"(a7) : "memory");

    *(volatile u64 *)0x80000000 = 1; // 写内核页 → store page fault → 跳过

    register u64 a0b asm("a0") = 0;
    register u64 a7b asm("a7") = 2; // sys_done
    asm volatile("ecall" : "+r"(a0b) : "r"(a7b) : "memory");
    for (;;)
        ;
}

__attribute__((noreturn)) static void poweroff(int code) {
    puts_("S: done\n");
    put_hex(echo_val);
    put_hex(timer_hits);
    put_hex(fault_seen);
    put_hex(last_scause);
    *TEST_DEV = ((u32)code << 16) | 0x5555;
    for (;;)
        ;
}

static void check_done(void) {
    if (u_done && timer_hits)
        poweroff(echo_val == 0x1235 && fault_seen ? 0 : 1);
}

// M 态：硬件定时器中断 → 转发 STIP（OpenSBI 风格）
__attribute__((noinline, aligned(4))) static void m_trap(void) {
    if (csr_read(mcause) == ((1ul << 63) | 7)) {
        *MTIMECMP = ~0ul;
        csr_set(mip, 1ul << 5);
        asm volatile("mret");
    }
    puts_("M: unexpected trap\n");
    put_hex(csr_read(mcause));
    put_hex(csr_read(mepc));
    put_hex(csr_read(mtval));
    put_hex(csr_read(mstatus));
    put_hex(csr_read(satp));
    poweroff(2);
}

// S 态：U 的 ecall、page fault、转发来的定时器（s_trap_entry 桩跳入，需全局符号）
__attribute__((noinline, aligned(4))) void s_trap(void) {
    u64 cause = csr_read(scause);
    u64 sepc = csr_read(sepc);
    u64 a0 = trap_a0, a7 = trap_a7;
    last_scause = cause;

    if (cause == ((1ul << 63) | 5)) { // S timer
        csr_write(sip, 0);
        timer_hits++;
    } else if (cause == 8) { // ecall from U
        if (a7 == 1)
            echo_val = a0 + 1;
        else if (a7 == 2)
            u_done = 1;
        csr_write(sepc, sepc + 4);
    } else if (cause == 15 || cause == 13 || cause == 12) { // page fault
        fault_seen = 1;
        csr_write(sepc, sepc + 4); // 跳过触发指令
    }
    check_done();
    asm volatile("sret");
}

// S 态入口：进入 U 态
__attribute__((noinline)) void s_entry(void) {
    puts_("S: enter\n");
    // SIE/SPIE 置 1（sret 后 SIE ← SPIE）
    csr_set(sstatus, (1ul << 1) | (1ul << 5));
    csr_write(sie, (1ul << 5) | (1ul << 1)); // STIE | SSIE

    // U 栈（VA 0x40401000 顶）+ 用户入口
    asm volatile("mv sp, %0" ::"r"(0x40402000));
    csr_write(sepc, 0x40400000);
    csr_write(sstatus, csr_read(sstatus) & ~(1ul << 8)); // SPP=0 → U
    asm volatile("sret");
    __builtin_unreachable();
}

// 入口：qemu -bios none 时 sp=0，必须先立栈（模拟器预置了 sp，两边都成立）
__attribute__((naked, section(".text.init"))) void _start(void) {
    asm volatile("la t0, s_stack + 8192\n"
                 "mv sp, t0\n"
                 "j boot_main\n");
}

void boot_main(void) {
    puts_("M: boot\n");

    // 页表
    map(pt_l2, 0, 0, PTE_V | PTE_R | PTE_W | PTE_A | PTE_D);         // MMIO 恒等 1GB
    map(pt_l2, 2, 0x80000000, PTE_V | PTE_R | PTE_W | PTE_X | PTE_A | PTE_D); // RAM 恒等 1GB
    map(pt_l2, 1, (u64)pt_l1_user, PTE_V);                           // → 用户 L1
    map(pt_l1_user, 2, (u64)pt_l0_user, PTE_V);                      // VA 0x4040_0000 → L0
    map(pt_l0_user, 0, (u64)user_code_page, PTE_V | PTE_R | PTE_W | PTE_X | PTE_U | PTE_A | PTE_D);
    map(pt_l0_user, 1, (u64)user_stack_page, PTE_V | PTE_R | PTE_W | PTE_U | PTE_A | PTE_D);

    // 拷贝用户代码（satp 尚未开启）
    char *src = (char *)u_entry;
    for (int i = 0; i < 4096; i++)
        user_code_page[i] = src[i];

    // 委托：SSIP/STIP 中断，illegal/8/12/13/15 异常
    csr_write(mideleg, (1ul << 5) | (1ul << 1));
    csr_write(medeleg, (1ul << 2) | (1ul << 8) | (1ul << 12) | (1ul << 13) | (1ul << 15));

    // PMP：全地址空间 RWX（OpenSBI 同款）。mret 进低特权级前必须配规则，
    // 否则 S/U 态取指全被拒（QEMU 与本模拟器都会在 mret 处抛 instruction access fault）
    csr_write(pmpaddr0, ~0ul);
    csr_write(pmpcfg0, (3ul << 3) | 0x7); // NAPOT | R | W | X

    csr_write(mtvec, (u64)m_trap);
    extern void s_trap_entry(void);
    csr_write(stvec, (u64)s_trap_entry);

    // SUM：允许 S 态（trap handler）访问 U 页（如用户栈）
    csr_set(mstatus, 1ul << 18);

    // 定时器：2ms 后
    *MTIMECMP = *MTIME + 20000;
    csr_set(mie, 1ul << 7); // MTIE
    csr_set(mstatus, (1ul << 3) | (1ul << 7)); // MIE + MPIE（mret 后 MIE ← MPIE）

    csr_write(satp, (8ul << 60) | ((u64)pt_l2 >> 12));
    asm volatile("sfence.vma");

    // S 栈（qemu -bios none 下 sp=0，必须先备好）
    asm volatile("mv sp, %0" ::"r"((u64)s_stack + sizeof(s_stack)));
    csr_write(mepc, (u64)s_entry);
    csr_set(mstatus, 1ul << 11); // MPP = S
    asm volatile("mret");
    __builtin_unreachable();
}
