// 无 libc：raw syscall ioctl(0, TIOCSWINSZ, &ws)，告诉 busybox ash 终端尺寸，
// 避免 ash 行编辑发 ESC[6n 光标查询（管道/仿真终端无法应答会卡住输入）。
// ioctl = syscall 29（asm-generic），TIOCSWINSZ = 0x5414。
struct winsize {
    unsigned short ws_row, ws_col, ws_xpixel, ws_ypixel;
};

static void exit2(long code) {
    register long a0 asm("a0") = code;
    register long a7 asm("a7") = 93;
    asm volatile("ecall" ::"r"(a0), "r"(a7));
    __builtin_unreachable();
}

void _start(void) {
    struct winsize ws = {24, 80, 0, 0};
    register long a0 asm("a0") = 0; // fd 0 = /dev/console
    register long a1 asm("a1") = 0x5414; // TIOCSWINSZ
    register long a2 asm("a2") = (long)&ws;
    register long a7 asm("a7") = 29;
    asm volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a7));
    exit2(0);
}
