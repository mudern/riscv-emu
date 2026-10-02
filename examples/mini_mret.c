// 最小 S 态切换差分测试：
//   M: mepc=s_start, MPP=S, mret → S 态写 UART 'S'，写 sifive_test 关机（32 位写！）
//   SATP_MODE 控制是否先开 Sv39 恒等映射（MMIO vpn2=0 + RAM vpn2=2）
#ifndef SATP_MODE
#define SATP_MODE 0
#endif

typedef unsigned long u64;
typedef unsigned int u32;

#define UART0 ((volatile unsigned char *)0x10000000)
#define TEST_DEV ((volatile u32 *)0x100000)

#define PTE_V (1ul << 0)
#define PTE_R (1ul << 1)
#define PTE_W (1ul << 2)
#define PTE_X (1ul << 3)
#define PTE_A (1ul << 6)
#define PTE_D (1ul << 7)

__attribute__((aligned(4096))) static u64 l2[512];

void s_start(void) {
    *UART0 = 'S';
    *TEST_DEV = 0x5555;
    for (;;)
        ;
}

void m_trap_entry(void) {
    u64 mc, mp, mt, st;
    asm volatile("csrr %0, mcause" : "=r"(mc));
    asm volatile("csrr %0, mepc" : "=r"(mp));
    asm volatile("csrr %0, mtval" : "=r"(mt));
    asm volatile("csrr %0, satp" : "=r"(st));
    static const char hex[] = "0123456789abcdef";
    u64 vals[4] = {mc, mp, mt, st};
    for (int i = 0; i < 4; i++) {
        *UART0 = '0';
        *UART0 = 'x';
        for (int b = 60; b >= 0; b -= 4)
            *UART0 = hex[(vals[i] >> b) & 0xF];
        *UART0 = '\n';
    }
    *TEST_DEV = ((u32)1 << 16) | 0x5555;
    for (;;)
        ;
}

void _start(void) {
    extern char _estack[];
    asm volatile("la sp, _estack" ::: "memory");

#if SATP_MODE
    // MMIO 恒等 1GB（vpn2=0: 0x0..0x3FFF_FFFF，无 X）
    l2[0] = ((u64)0 >> 12 << 10) | PTE_V | PTE_R | PTE_W | PTE_A | PTE_D;
    // RAM 恒等 1GB 大页（vpn2=2: 0x8000_0000..0xBFFF_FFFF）
    l2[2] = ((u64)0x80000000 >> 12 << 10) | PTE_V | PTE_R | PTE_W | PTE_X | PTE_A | PTE_D;
    u64 satp = ((u64)SATP_MODE << 60) | ((u64)l2 >> 12);
    asm volatile("csrw satp, %0" ::"r"(satp));
    asm volatile("sfence.vma");
#endif

    u64 t = (u64)m_trap_entry;
    asm volatile("csrw mtvec, %0" ::"r"(t));
    t = (u64)s_start;
    asm volatile("csrw mepc, %0" ::"r"(t));
    // PMP 全空间 RWX：mret 进 S 前必须配规则（否则 S 态取指被拒）
    asm volatile("csrw pmpaddr0, %0" ::"r"(~0ul));
    asm volatile("csrw pmpcfg0, %0" ::"r"((3ul << 3) | 0x7));
    // MPP = S：bit 11
    asm volatile("csrs mstatus, %0" ::"r"((u64)1 << 11));
    asm volatile("mret");
    for (;;)
        ;
}

__attribute__((aligned(16))) char stack_space[4096];
char _estack[0] __attribute__((section(".stack_top")));
