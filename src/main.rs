use std::env;
use std::process::ExitCode;

use riscv_emu::{elf, exception::TrapInfo, machine::Halt, Machine};

const USAGE: &str = "\
用法: riscv-emu [--trace] [--stats] [--mem <MB>] [--bin] [--sbi] <image>

  <image>   RISC-V ELF64 可执行文件（静态、非 PIE），或 --bin 时的裸二进制
  --trace   打印每条指令
  --stats   退出时打印指令数和性能统计
  --mem     RAM 大小，单位 MB（默认 128）
  --bin     按裸二进制加载到 0x8000_0000
  --sbi     S 态 ecall 按内置 SBI 处理（putchar/timer/shutdown 等），而非异常交付
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut trace = false;
    let mut stats = false;
    let mut mem_mb: usize = 128;
    let mut flat_bin = false;
    let mut sbi = false;
    let mut image: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--trace" => trace = true,
            "--stats" => stats = true,
            "--bin" => flat_bin = true,
            "--sbi" => sbi = true,
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
    let start = std::time::Instant::now();
    let halt = machine.run(u64::MAX);
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
