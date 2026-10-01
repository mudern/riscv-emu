// 更重的交叉验证程序：递归、64 位乘除、访存、大量压缩指令。
// 预期：与 qemu-riscv64 的输出和退出码完全一致。

typedef unsigned long u64;

static u64 sys3(u64 n, u64 a, u64 b, u64 c) {
    register u64 a0 asm("a0") = a;
    register u64 a1 asm("a1") = b;
    register u64 a2 asm("a2") = c;
    register u64 a7 asm("a7") = n;
    asm volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a7) : "memory");
    return a0;
}

static void puts_(const char *s) {
    u64 n = 0;
    while (s[n])
        n++;
    sys3(64, 1, (u64)s, n);
}

static long fib(long n) {
    if (n < 2)
        return n;
    return fib(n - 1) + fib(n - 2);
}

static unsigned long checksum(unsigned char *p, unsigned long n) {
    unsigned long h = 1469598103934665603UL;
    for (unsigned long i = 0; i < n; i++) {
        h ^= p[i];
        h *= 1099511628211UL;
    }
    return h;
}

#define N 4096
static unsigned char buf[N];

void _start(void) {
    puts_("fib(27)=");

    // 打印十进制（裸机没有 printf，自己实现）
    unsigned long v = fib(27);
    char digits[24];
    int i = 23;
    digits[i--] = '\n';
    if (v == 0)
        digits[i--] = '0';
    while (v) {
        digits[i--] = '0' + v % 10;
        v /= 10;
    }
    sys3(64, 1, (u64)&digits[i + 1], 23 - i);

    // 内存校验和压测
    for (unsigned long j = 0; j < N; j++)
        buf[j] = (unsigned char)(j * 7 + 3);
    unsigned long h = 0;
    for (int r = 0; r < 64; r++)
        h = h * 31 + checksum(buf, N);
    puts_("checksum done\n");

    // 除法/取余压测
    unsigned long d = 0;
    for (unsigned long j = 1; j < 200000; j++)
        d += (j * 2654435761UL) % j;

    sys3(93, (h + d) % 256, 0, 0);
}
