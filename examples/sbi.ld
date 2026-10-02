/* OpenSBI payload 链接脚本：S 态程序放到 0x8020_0000（OpenSBI 约定的
 * payload 地址，避开固件自身的 0x8000_0000 区域） */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    . = 0x80200000;

    .text : {
        KEEP(*(.text.init))
        *(.text .text.*)
    }

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
