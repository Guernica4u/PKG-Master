//! Synthetic ELF/SELF builders and scratch folders shared by the tests.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use pkgmaster::elf::parse_elf;

pub const PT_LOAD: u32 = 1;
pub const PHDR_SIZE: usize = 0x38;

/// A minimal ELF64 with one PT_LOAD per payload. Each segment's offset and
/// vaddr agree modulo its 0x1000 alignment but are not page aligned, as in
/// real SCE executables. `misalign` shifts the last vaddr to break that.
pub fn build_elf(payloads: &[&[u8]], misalign: bool) -> Vec<u8> {
    let phoff = 0x40usize;
    let mut phdrs = Vec::new();
    let mut cursor = 0x1000usize;
    for (i, p) in payloads.iter().enumerate() {
        let offset = cursor + 0x40 * i;
        let mut vaddr = 0x10_0000 * (i as u64 + 1) + (offset as u64 % 0x1000);
        if misalign && i + 1 == payloads.len() {
            vaddr += 8;
        }
        phdrs.push((offset, vaddr, p.len()));
        cursor = (offset + p.len() + 0xFFF) & !0xFFF;
    }
    let total = phdrs.iter().map(|(o, _, n)| o + n).max().unwrap();
    let mut out = vec![0u8; total];
    out[..4].copy_from_slice(b"\x7fELF");
    out[4] = 2;
    out[5] = 1;
    out[6] = 1;
    out[7] = 9;
    out[0x10..0x12].copy_from_slice(&0xFE10u16.to_le_bytes());
    out[0x12..0x14].copy_from_slice(&0x3Eu16.to_le_bytes());
    out[0x18..0x20].copy_from_slice(&(phdrs[0].1 + 0x10).to_le_bytes());
    out[0x20..0x28].copy_from_slice(&(phoff as u64).to_le_bytes());
    out[0x34..0x36].copy_from_slice(&0x40u16.to_le_bytes());
    out[0x36..0x38].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
    out[0x38..0x3A].copy_from_slice(&(phdrs.len() as u16).to_le_bytes());
    for (i, ((offset, vaddr, size), payload)) in phdrs.iter().zip(payloads).enumerate() {
        let b = phoff + i * PHDR_SIZE;
        out[b..b + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        out[b + 4..b + 8].copy_from_slice(&5u32.to_le_bytes());
        for (at, v) in [(0x08, *offset as u64), (0x10, *vaddr), (0x18, *vaddr), (0x20, *size as u64), (0x28, *size as u64), (0x30, 0x1000)] {
            out[b + at..b + at + 8].copy_from_slice(&v.to_le_bytes());
        }
        out[*offset..offset + size].copy_from_slice(payload);
    }
    out
}

/// Wrap an ELF in a PS4 SELF, one segment per program header.
pub fn build_self(elf: &[u8], encrypt_first: bool) -> Vec<u8> {
    let info = parse_elf(elf, 0).unwrap();
    let segs: Vec<_> = info.phdrs.iter().filter(|p| p.p_filesz != 0).collect();
    let table_end = 0x20 + segs.len() * 0x20;
    let headers = 0x40 + info.e_phnum as usize * PHDR_SIZE;
    let mut cursor = (table_end + headers + 0xF) & !0xF;
    let mut out = vec![0u8; cursor];
    out[..4].copy_from_slice(&0x1D3D154Fu32.to_le_bytes());
    out[0x18..0x1A].copy_from_slice(&(segs.len() as u16).to_le_bytes());
    out[table_end..table_end + headers].copy_from_slice(&elf[..headers]);
    for (i, p) in segs.iter().enumerate() {
        let b = 0x20 + i * 0x20;
        let props: u64 = if encrypt_first && i == 0 { 2 } else { 0 };
        out[b..b + 8].copy_from_slice(&props.to_le_bytes());
        out[b + 8..b + 16].copy_from_slice(&(cursor as u64).to_le_bytes());
        out[b + 16..b + 24].copy_from_slice(&p.p_filesz.to_le_bytes());
        out[b + 24..b + 32].copy_from_slice(&p.p_filesz.to_le_bytes());
        out.resize(cursor, 0);
        out.extend_from_slice(&elf[p.p_offset as usize..(p.p_offset + p.p_filesz) as usize]);
        cursor = (out.len() + 0xF) & !0xF;
    }
    let size = out.len() as u64;
    out[0x10..0x18].copy_from_slice(&size.to_le_bytes());
    out
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pkgmaster-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn write(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}
