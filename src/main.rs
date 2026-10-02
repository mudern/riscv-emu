use std::env;
use std::process::ExitCode;

use riscv_emu::{elf, exception::TrapInfo, machine::Halt, Machine};

const USAGE: &str = "\
用法: riscv-emu [--trace] [--stats] [--mem <MB>] [--bin] [--sbi] <image>
      riscv-emu [--trace] [--stats] [--mem <MB>] --bios <fw> [--kernel <image>] [--dtb <dtb>]

  <image>   RISC-V ELF64 可执行文件（静态、非 PIE），或 --bin 时的裸二进制
  --trace   打印每条指令
  --stats   退出时打印指令数和性能统计
  --mem     RAM 大小，单位 MB（默认 128）
  --bin     按裸二进制加载到 0x8000_0000
  --sbi     S 态 ecall 按内置 SBI 处理（putchar/timer/shutdown 等），而非异常交付
  --bios    固件 ELF（如 OpenSBI fw_dynamic）：QEMU 同款引导流程，
            a0=hartid a1=DTB a2=fw_dynamic_info
  --kernel  固件模式下的 payload（ELF 按其地址加载，裸二进制放 0x8020_0000）
  --dtb     固件模式下的设备树（默认 board/virt.dtb）
";

/// fw_dynamic_info（OpenSBI fw_dynamic.h v2）：QEMU 引导 fw_dynamic 时放在
/// a2 指向的内存里。放在 FDT 下方一页（FDT 位于 RAM 顶部 2MB 对齐处，
/// 互不重叠）。
fn fw_dynamic_info(next_addr: u64) -> [u8; 48] {
    let mut b = [0u8; 48];
    b[0..8].copy_from_slice(&0x4942_534F_u64.to_le_bytes()); // magic "OSBI"
    b[8..16].copy_from_slice(&2_u64.to_le_bytes()); // version 2
    b[16..24].copy_from_slice(&next_addr.to_le_bytes()); // next_addr
    b[24..32].copy_from_slice(&1_u64.to_le_bytes()); // next_mode = PRV_S
    // options = 0，boot_hart = 0
    b
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut trace = false;
    let mut stats = false;
    let mut mem_mb: usize = 128;
    let mut flat_bin = false;
    let mut sbi = false;
    let mut image: Option<String> = None;
    let mut bios: Option<String> = None;
    let mut kernel: Option<String> = None;
    let mut dtb: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--trace" => trace = true,
            "--stats" => stats = true,
            "--bin" => flat_bin = true,
            "--sbi" => sbi = true,
            "--bios" | "--kernel" | "--dtb" => {
                let tag = args[i][2..].to_string();
                i += 1;
                let Some(path) = args.get(i) else {
                    eprintln!("--{tag} 需要一个文件参数");
                    return ExitCode::FAILURE;
                };
                match tag.as_str() {
                    "bios" => bios = Some(path.clone()),
                    "kernel" => kernel = Some(path.clone()),
                    "dtb" => dtb = Some(path.clone()),
                    _ => unreachable!(),
                }
            }
            "--mem" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse().ok()) {
                    Some(mb) => mem_mb = mb,
                    None => {
                        eprintln!("--mem 需要一个数字参数");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("未知参数: {other}\n");
                print!("{USAGE}");
                return ExitCode::FAILURE;
            }
            _ => {
                if image.replace(args[i].clone()).is_some() {
                    eprintln!("只能指定一个镜像文件");
                    return ExitCode::FAILURE;
                }
            }
        }
        i += 1;
    }

    let read_file = |tag: &str, path: &str| -> Result<Vec<u8>, ExitCode> {
        std::fs::read(path).map_err(|e| {
            eprintln!("无法读取 {tag} {path}: {e}");
            ExitCode::FAILURE
        })
    };

    // 固件引导流程（QEMU 同款）
    if let Some(bios_path) = &bios {
        if image.is_some() {
            eprintln!("--bios 模式下 payload 用 --kernel 指定");
            return ExitCode::FAILURE;
        }
        let fw = match read_file("固件", bios_path) {
            Ok(d) => d,
            Err(c) => return c,
        };
        let kernel_data = match &kernel {
            Some(p) => match read_file("内核", p) {
                Ok(d) => Some(d),
                Err(c) => return c,
            },
            None => None,
        };
        let dtb_data = match read_file("DTB", dtb.as_deref().unwrap_or("board/virt.dtb")) {
            Ok(d) => d,
            Err(c) => return c,
        };
        return boot_firmware(trace, stats, mem_mb, fw, kernel_data, dtb_data);
    }
    if kernel.is_some() || dtb.is_some() {
        eprintln!("--kernel/--dtb 需要 --bios");
        return ExitCode::FAILURE;
    }

    let Some(image) = image else {
        print!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let data = match std::fs::read(&image) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("无法读取 {image}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut machine = Machine::new(mem_mb);
    machine.cpu.trace = trace;
    machine.cpu.sbi = sbi;
    let entry = if flat_bin {
        match elf::load_flat_bin(&data, &mut machine.bus) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match elf::load_elf(&data, &mut machine.bus) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("加载 ELF 失败: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    machine.cpu.pc = entry;
    run_and_report(machine, stats)
}

/// 固件引导：加载 fw_dynamic.elf + payload + DTB，按 QEMU 语义摆好
/// a0/a1/a2 与 fw_dynamic_info，从固件入口开始执行。
fn boot_firmware(
    trace: bool,
    stats: bool,
    mem_mb: usize,
    fw: Vec<u8>,
    kernel: Option<Vec<u8>>,
    dtb: Vec<u8>,
) -> ExitCode {
    let mut machine = Machine::new(mem_mb);
    machine.cpu.trace = trace;

    // QEMU 的默认固件是 fw_dynamic.bin（裸二进制装到 0x8000_0000，入口同址）；
    // OpenSBI 的 fw_dynamic.elf 链接在 0、QEMU 也加载不了，所以 bin 是基准形态。
    let (fw_entry, is_elf) = if fw.len() >= 4 && fw[..4] == [0x7f, b'E', b'L', b'F'] {
        match elf::load_elf(&fw, &mut machine.bus) {
            Ok(e) => (e, true),
            Err(e) => {
                eprintln!("加载固件 ELF 失败: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        (riscv_emu::bus::DRAM_BASE, false)
    };
    if !is_elf && !machine.bus.write_dram(riscv_emu::bus::DRAM_BASE, &fw) {
        eprintln!("固件大于 RAM");
        return ExitCode::FAILURE;
    }

    let mut kernel_entry = 0;
    if let Some(data) = &kernel {
        // QEMU 同款：ELF 按其段地址加载，非 ELF（Image 裸二进制）放到
        // DRAM_BASE + 0x200000（QEMU 的 kernel_start_addr 对齐规则）
        if data.len() >= 4 && data[..4] == [0x7f, b'E', b'L', b'F'] {
            match elf::load_elf(data, &mut machine.bus) {
                Ok(e) => kernel_entry = e,
                Err(e) => {
                    eprintln!("加载内核 ELF 失败: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            kernel_entry = riscv_emu::bus::DRAM_BASE + 0x20_0000;
            if !machine.bus.write_dram(kernel_entry, data) {
                eprintln!("内核大于 RAM");
                return ExitCode::FAILURE;
            }
        }
    }

    // DTB 放置对齐 QEMU riscv_compute_fdt_addr：align_down(RAM 顶 - fdt大小, 2MB)
    let fdt_addr = align_down(machine.bus.dram_end() - dtb.len() as u64, 0x20_0000);
    if !machine.bus.write_dram(fdt_addr, &dtb) {
        eprintln!("DTB 写入失败");
        return ExitCode::FAILURE;
    }
    // fw_dynamic_info 放 FDT 下方一页，8 字节对齐
    let fw_dyn_addr = fdt_addr - 0x1000;
    machine
        .bus
        .write_dram(fw_dyn_addr, &fw_dynamic_info(kernel_entry));

    // QEMU reset vector 的寄存器约定：a0=hartid, a1=FDT, a2=&fw_dynamic_info
    machine.cpu.regs = [0; 32];
    machine.cpu.regs[10] = 0; // a0 = mhartid
    machine.cpu.regs[11] = fdt_addr; // a1
    machine.cpu.regs[12] = fw_dyn_addr; // a2
    machine.cpu.pc = fw_entry;
    run_and_report(machine, stats)
}

fn align_down(v: u64, a: u64) -> u64 {
    v & !(a - 1)
}

fn run_and_report(mut machine: Machine, stats: bool) -> ExitCode {
    // 后台线程阻塞读 stdin，每次读 1 字节送进通道；主循环按块运行模拟器，
    // 块间把收到的字节注入 UART RX（busybox shell 的交互输入）。
    let (tx, rx) = std::sync::mpsc::sync_channel::<u8>(0);
    std::thread::spawn(move || {
        use std::io::Read;
        // 绕开 Stdin 的 BufReader（1 字节读会被 8KB 缓冲填满阻塞），
        // 用 /dev/stdin 的无缓冲 fd 逐字节读
        let Ok(mut stdin) = std::fs::File::open("/dev/stdin") else {
            return;
        };
        let mut buf = [0u8; 1];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.send(buf[0]).is_err() {
                return; // EOF 或宿主已退出
            }
        }
    });

    let start = std::time::Instant::now();
    let halt = loop {
        let halt = machine.run(200_000);
        match halt {
            Halt::Timeout => {}
            other => break other,
        }
        // 块间排空输入通道（非阻塞）
        while let Ok(byte) = rx.try_recv() {
            machine.bus.uart.receive(byte);
        }
    };
    let elapsed = start.elapsed();
    if stats {
        let mips = machine.cpu.instret as f64 / elapsed.as_secs_f64() / 1e6;
        eprintln!(
            "retired {} 条指令，用时 {:.3}s（{:.1} MIPS）",
            machine.cpu.instret,
            elapsed.as_secs_f64(),
            mips
        );
    }

    match halt {
        Halt::Exit(code) => ExitCode::from(code as u8),
        Halt::Timeout => {
            eprintln!("超过最大指令数限制");
            ExitCode::from(124)
        }
        Halt::Trap(t) => {
            report_trap(&t, &machine);
            ExitCode::FAILURE
        }
    }
}

fn report_trap(t: &TrapInfo, m: &Machine) {
    eprintln!(
        "致命异常: {} @ pc={:#x}, tval={:#x}",
        t.cause.name(),
        t.pc,
        t.val
    );
    eprintln!(
        "  (guest 未设置 mtvec trap handler；mepc={:#x} mcause={:#x} mtval={:#x})",
        m.cpu.csr.mepc, m.cpu.csr.mcause, m.cpu.csr.mtval
    );
    let mut regs = String::new();
    for (i, r) in m.cpu.regs.iter().enumerate() {
        if i % 4 == 0 {
            regs.push_str("\n  ");
        }
        regs.push_str(&format!("x{:02}({:>#18x})  ", i, r));
    }
    eprintln!("寄存器现场:{regs}");
}
