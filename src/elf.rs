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
    if e_type != 2 && e_type != 3 {
        return Err(format!("不支持的 ELF 类型 e_type={e_type}"));
    }
    // ET_DYN（OpenSBI fw_dynamic 等）：与 QEMU 的 load_elf 一致，直接按
    // p_vaddr 链接地址加载（固件链接在 0x8000_0000，不实际重定位）。

    let e_entry = u64_at(image, 24);
    let phoff = u64_at(image, 32) as usize;
    let phentsize = u16_at(image, 54) as usize;
    let phnum = u16_at(image, 56) as usize;
    if phoff + phnum * phentsize > image.len() {
        return Err("program header 越界".into());
    }

    // 内核 vmlinux（QEMU load_elf 语义）：段按 p_paddr（物理地址）装入；
    // 入口是内核虚拟地址时按 e_entry - p_vaddr + p_paddr 换算。
    let mut entry = e_entry;
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
        let p_paddr = u64_at(image, ph + 24);
        let p_filesz = u64_at(image, ph + 32) as usize;
        let p_memsz = u64_at(image, ph + 40);
        if p_offset + p_filesz > image.len() {
            return Err(format!("PT_LOAD #{i} 文件数据越界"));
        }
        let p_end = match p_paddr.checked_add(p_memsz) {
            Some(e) => e,
            None => {
                return Err(format!(
                    "PT_LOAD #{i} 段大小溢出（paddr={p_paddr:#x}, memsz={p_memsz:#x}）"
                ))
            }
        };
        if p_paddr < DRAM_BASE || p_end > bus.dram_end() {
            return Err(format!(
                "PT_LOAD #{i} 不在 RAM 范围内（paddr={p_paddr:#x}, memsz={p_memsz:#x}，RAM: {DRAM_BASE:#x}..{:#x}）",
                bus.dram_end()
            ));
        }
        if !bus.write_dram(p_paddr, &image[p_offset..p_offset + p_filesz]) {
            return Err(format!("PT_LOAD #{i} 写入失败"));
        }
        if p_vaddr != p_paddr && e_entry >= p_vaddr && e_entry < p_vaddr + p_filesz as u64 {
            entry = e_entry - p_vaddr + p_paddr;
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
