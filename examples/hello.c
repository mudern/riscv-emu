// 裸机 RISC-V 测试程序（阶段 1 约定：M-mode ecall = Linux 风格 write/exit）。
// 编译（示例）：
//   clang --target=riscv64-unknown-elf -march=rv64imac_zicsr -mabi=lp64 \
//         -nostdlib -static -O2 -Wl,-Ttext=0x80000000 -o hello.elf hello.c

typedef unsigned long u64;

static u64 sys3(u64 n, u64 a, u64 b, u64 c) {
    register u64 a0 asm("a0") = a;
    register u64 a1 asm("a1") = b;
    register u64 a2 asm("a2") = c;
    register u64 a7 asm("a7") = n;
    asm volatile("ecall"
                 : "+r"(a0)
                 : "r"(a1), "r"(a2), "r"(a7)
                 : "memory");
    return a0;
}

static u64 slen(const char *s) {
    u64 n = 0;
    while (s[n])
        n++;
    return n;
}

void _start(void) {
    static const char msg[] = "Hello from bare-metal RISC-V!\n";
    sys3(64, 1, (u64)msg, slen(msg));

    // 顺手压测一下 M 扩展和分支（结果进 exit code，可和 qemu 对照）
    volatile long x = 1234567;
    volatile long y = 0;
    for (long i = 0; i < 1000; i++)
        y += i * x % 7;

    sys3(64, 1, (u64)msg, slen(msg));
    sys3(93, (u64)(y % 256), 0, 0);
}
