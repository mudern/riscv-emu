//! 最小 ELF64 加载器：只支持静态非 PIE（ET_EXEC）的 RISC-V 镜像。

use crate::bus::{Bus, DRAM_BASE};

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

/// 把 PT_LOAD 段装入 RAM，返回入口地址。
pub fn load_elf(image: &[u8], bus: &mut Bus) -> Result<u64, String> {
    if image.len() < 64 || image[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err("不是 ELF 文件".into());
    }
    if image[4] != 2 {
        return Err("只支持 ELF64".into());
    }
    if image[5] != 1 {
        return Err("只支持小端字节序".into());
    }
    if u16_at(image, 18) != 243 {
        return Err("不是 RISC-V 镜像（e_machine != EM_RISCV）".into());
    }
    let e_type = u16_at(image, 16);
    if e_type == 3 {
        return Err("暂不支持 PIE（ET_DYN），请用 -static -no-pie 重新编译".into());
    }
    if e_type != 2 {
        return Err(format!("不支持的 ELF 类型 e_type={e_type}"));
    }

    let entry = u64_at(image, 24);
    let phoff = u64_at(image, 32) as usize;
    let phentsize = u16_at(image, 54) as usize;
    let phnum = u16_at(image, 56) as usize;
    if phoff + phnum * phentsize > image.len() {
        return Err("program header 越界".into());
    }

    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if ph + 56 > image.len() {
            return Err("program header 越界".into());
        }
        if u32_at(image, ph) != 1 {
            continue; // 只处理 PT_LOAD
        }
        let p_offset = u64_at(image, ph + 8) as usize;
        let p_vaddr = u64_at(image, ph + 16);
        let p_filesz = u64_at(image, ph + 32) as usize;
        let p_memsz = u64_at(image, ph + 40);
        if p_offset + p_filesz > image.len() {
            return Err(format!("PT_LOAD #{i} 文件数据越界"));
        }
        if p_vaddr < DRAM_BASE || p_vaddr + p_memsz > bus.dram_end() {
            return Err(format!(
                "PT_LOAD #{i} 不在 RAM 范围内（vaddr={p_vaddr:#x}, memsz={p_memsz:#x}，RAM: {DRAM_BASE:#x}..{:#x}）",
                bus.dram_end()
            ));
        }
        if !bus.write_dram(p_vaddr, &image[p_offset..p_offset + p_filesz]) {
            return Err(format!("PT_LOAD #{i} 写入失败"));
        }
    }
    Ok(entry)
}

/// 加载纯二进制镜像到 RAM 基址。
pub fn load_flat_bin(image: &[u8], bus: &mut Bus) -> Result<u64, String> {
    if !bus.write_dram(DRAM_BASE, image) {
        return Err("镜像大于 RAM".into());
    }
    Ok(DRAM_BASE)
}
