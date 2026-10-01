/* 裸机 RISC-V 链接脚本：整体放到 DRAM 0x8000_0000，ELF 头并入首段 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    . = 0x80000000;

    .text : {
        KEEP(*(.text.init))
        *(.text .text.*)
    }

    /* 独占一页：避免与 .text 共页时 qemu-user 的权限覆盖问题 */
    . = ALIGN(0x1000);
    .rodata : ALIGN(8) { *(.rodata .rodata.* .srodata .srodata.*) }

    .data : ALIGN(8) { *(.data .data.* .sdata .sdata.*) }

    .bss : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.* COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    /DISCARD/ : { *(.eh_frame .eh_frame_hdr .note.gnu.build-id .riscv.attributes .comment) }
}
