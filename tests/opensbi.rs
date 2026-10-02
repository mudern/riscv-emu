//! OpenSBI 引导冒烟测试（需要外部固件，缺环境时跳过）：
//! 设 OPENSBI_FW=<fw_dynamic.bin 路径> 后运行。
//! payload 用 common 裸机装配器现写：S 态 ecall 打印 "OOO" 后 legacy
//! shutdown（OpenSBI → sifive_test → 宿主退出 0）。
//! 完整的 QEMU 输出差分见 README。

mod common;

use common::*;
use riscv_emu::machine::Halt;
use riscv_emu::{Machine, DRAM_BASE};

const FDT_ALIGN: u64 = 0x20_0000;

/// fw_dynamic_info（v2）：magic/version/next_addr/next_mode/options/boot_hart
fn write_fw_dynamic_info(bus: &mut riscv_emu::Bus, addr: u64, next_addr: u64) {
    let mut b = [0u8; 48];
    b[0..8].copy_from_slice(&0x4942_534F_u64.to_le_bytes()); // "OSBI"
    b[8..16].copy_from_slice(&2u64.to_le_bytes());
    b[16..24].copy_from_slice(&next_addr.to_le_bytes());
    b[24..32].copy_from_slice(&1u64.to_le_bytes()); // next_mode = PRV_S
    assert!(bus.write_dram(addr, &b));
}

#[test]
fn opensbi_boots_smode_payload() {
    let Ok(fw_path) = std::env::var("OPENSBI_FW") else {
        eprintln!("跳过：未设置 OPENSBI_FW（指向 OpenSBI fw_dynamic.bin）");
        return;
    };
    let fw = std::fs::read(&fw_path).unwrap_or_else(|e| panic!("OPENSBI_FW={fw_path} 无法读取: {e}"));
    let dtb = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/board/virt.dtb")).expect("board/virt.dtb");

    // S 态 payload：连续 3 次 legacy console_putchar（a7=1, a0='O'），
    // 然后 legacy shutdown（a7=8）。不使用 sp/ra，位置无关。
    let mut a = Asm::new();
    use reg::*;
    for _ in 0..3 {
        a.emit32(addi(A0, ZERO, b'O' as i32));
        a.emit32(addi(A7, ZERO, 1));
        a.emit32(ECALL);
    }
    a.emit32(addi(A7, ZERO, 8));
    a.emit32(ECALL);
    // 兜底死循环（shutdown 成功则不可达）
    a.emit32(beq(ZERO, ZERO, -4));

    let mut m = Machine::new(128);
    m.bus.uart.print = false;
    assert!(m.bus.write_dram(DRAM_BASE, &fw), "固件大于 RAM");
    let payload_addr = DRAM_BASE + 0x20_0000;
    assert!(m.bus.write_dram(payload_addr, &a.buf));

    // DTB 按 QEMU 规则放 RAM 顶 2MB 对齐处，fw_dynamic_info 放其下 1 页
    let fdt_addr = (m.bus.dram_end() - dtb.len() as u64) & !(FDT_ALIGN - 1);
    assert!(m.bus.write_dram(fdt_addr, &dtb));
    let dyn_addr = fdt_addr - 0x1000;
    write_fw_dynamic_info(&mut m.bus, dyn_addr, payload_addr);

    // QEMU reset vector 的寄存器约定
    m.cpu.regs = [0; 32];
    m.cpu.regs[10] = 0; // a0 = hartid
    m.cpu.regs[11] = fdt_addr; // a1 = FDT
    m.cpu.regs[12] = dyn_addr; // a2 = &fw_dynamic_info
    m.cpu.pc = DRAM_BASE;

    let halt = m.run(2_000_000_000);
    assert_eq!(halt, Halt::Exit(0), "OpenSBI 应经 sifive_test 正常关机");
    let out = String::from_utf8_lossy(&m.bus.uart.output);
    assert!(out.contains("OpenSBI"), "缺少 OpenSBI banner");
    assert!(
        out.contains("OOO"),
        "payload 未输出（末尾: …{:?}）",
        &out[out.len().saturating_sub(80)..]
    );
}
