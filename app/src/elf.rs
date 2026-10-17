//! ELF parsing and SELF -> ELF reconstruction.
//!
//! A SELF wraps an ELF64 image: a 32-byte header, a segment table, then the
//! original ELF header and program headers. Rebuilding copies those headers
//! verbatim and places each segment payload at its program header's offset.
//! Encrypted or compressed segments need console keys, so they are refused
//! rather than written out as a corrupt ELF.

use crate::binary::{u16le, u32le, u64le};
use crate::error::{Error, Result};

pub const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const SELF_MAGICS: [u32; 2] = [0x1D3D154F, 0xEEF51454];
const SELF_HEADER_SIZE: usize = 0x20;
const SEGMENT_ENTRY_SIZE: usize = 0x20;
const PROP_ENCRYPTED: u64 = 1 << 1;
const PROP_COMPRESSED: u64 = 1 << 3;

pub fn is_elf(d: &[u8]) -> bool {
    d.len() >= 4 && &d[..4] == ELF_MAGIC
}

pub fn is_self(d: &[u8]) -> bool {
    d.len() >= 4 && SELF_MAGICS.contains(&u32le(d, 0))
}

fn corrupt<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Corrupt(msg.into()))
}

#[derive(Clone, Debug)]
pub struct ProgramHeader {
    pub index: usize,
    pub p_type: u32,
    pub p_offset: u64,
    pub p_filesz: u64,
}

#[derive(Clone, Debug)]
pub struct ElfInfo {
    pub os_abi: u8,
    pub e_type: u16,
    pub e_machine: u16,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shoff: u64,
    pub phdrs: Vec<ProgramHeader>,
}

impl ElfInfo {
    pub fn type_name(&self) -> String {
        match self.e_type {
            0x0 => "ET_NONE".into(),
            0x1 => "ET_REL".into(),
            0x2 => "ET_EXEC".into(),
            0x3 => "ET_DYN".into(),
            0x4 => "ET_CORE".into(),
            0xFE00 => "ET_SCE_EXEC".into(),
            0xFE0C => "ET_SCE_STUBLIB".into(),
            0xFE10 => "ET_SCE_EXEC_ASLR".into(),
            0xFE18 => "ET_SCE_DYNAMIC".into(),
            t => format!("0x{t:X}"),
        }
    }

    pub fn machine_name(&self) -> String {
        match self.e_machine {
            0x03 => "EM_386".into(),
            0x3E => "EM_X86_64".into(),
            0x28 => "EM_ARM".into(),
            0xB7 => "EM_AARCH64".into(),
            m => format!("0x{m:X}"),
        }
    }

    pub fn osabi_name(&self) -> String {
        match self.os_abi {
            0x0 => "System V".into(),
            0x9 => "FreeBSD".into(),
            0x0C => "OpenBSD".into(),
            a => format!("0x{a:X}"),
        }
    }
}

pub fn parse_elf(d: &[u8], off: usize) -> Result<ElfInfo> {
    if d.len() < off + 0x40 {
        return corrupt("input is too small to contain an ELF header");
    }
    if &d[off..off + 4] != ELF_MAGIC {
        return corrupt(format!("no ELF magic at offset 0x{off:X}"));
    }
    let class = d[off + 4];
    if d[off + 5] != 1 {
        return corrupt("only little-endian ELF images are supported");
    }
    let is64 = match class {
        2 => true,
        1 => false,
        c => return corrupt(format!("unsupported ELF class {c}")),
    };
    let os_abi = d[off + 7];
    let e_type = u16le(d, off + 0x10);
    let e_machine = u16le(d, off + 0x12);
    let (e_entry, e_phoff, e_shoff, e_phentsize, e_phnum, fmt_size) = if is64 {
        (
            u64le(d, off + 0x18),
            u64le(d, off + 0x20),
            u64le(d, off + 0x28),
            u16le(d, off + 0x36),
            u16le(d, off + 0x38),
            56usize,
        )
    } else {
        (
            u32le(d, off + 0x18) as u64,
            u32le(d, off + 0x1C) as u64,
            u32le(d, off + 0x20) as u64,
            u16le(d, off + 0x2A),
            u16le(d, off + 0x2C),
            32usize,
        )
    };

    let mut phdrs = Vec::new();
    if e_phnum > 0 {
        let size = if e_phentsize == 0 { fmt_size } else { e_phentsize as usize };
        if size < fmt_size {
            return corrupt(format!("program header entry size {size} is too small"));
        }
        let table_end = (off as u64)
            .checked_add(e_phoff)
            .and_then(|v| v.checked_add(e_phnum as u64 * size as u64));
        if table_end.map_or(true, |e| e > d.len() as u64) {
            return corrupt(format!(
                "program header table runs past the 0x{:X} bytes available",
                d.len()
            ));
        }
        for i in 0..e_phnum as usize {
            let b = off + e_phoff as usize + i * size;
            phdrs.push(if is64 {
                ProgramHeader {
                    index: i,
                    p_type: u32le(d, b),
                    p_offset: u64le(d, b + 0x08),
                    p_filesz: u64le(d, b + 0x20),
                }
            } else {
                ProgramHeader {
                    index: i,
                    p_type: u32le(d, b),
                    p_offset: u32le(d, b + 0x04) as u64,
                    p_filesz: u32le(d, b + 0x10) as u64,
                }
            });
        }
    }
    Ok(ElfInfo { os_abi, e_type, e_machine, e_entry, e_phoff, e_phentsize, e_phnum, e_shoff, phdrs })
}

#[derive(Clone, Debug)]
struct SelfSegment {
    index: usize,
    props: u64,
    offset: u64,
    file_size: u64,
}

pub struct Conversion {
    pub data: Vec<u8>,
    pub info: ElfInfo,
    pub warnings: Vec<String>,
    pub copied_segments: usize,
    pub zero_filled_segments: usize,
}

/// Turn a SELF (or plain ELF, passed through) into a standard ELF.
pub fn to_elf(d: &[u8]) -> Result<Conversion> {
    if is_elf(d) {
        return Ok(Conversion {
            data: d.to_vec(),
            info: parse_elf(d, 0)?,
            warnings: Vec::new(),
            copied_segments: 0,
            zero_filled_segments: 0,
        });
    }
    if !is_self(d) {
        return corrupt("input does not start with a SELF magic");
    }
    if d.len() < SELF_HEADER_SIZE {
        return corrupt("SELF header is truncated");
    }
    let declared_size = u64le(d, 0x10);
    let entry_count = u16le(d, 0x18) as usize;
    if entry_count > 4096 {
        return corrupt(format!("implausible SELF segment count {entry_count}"));
    }
    let table_end = SELF_HEADER_SIZE + entry_count * SEGMENT_ENTRY_SIZE;
    if table_end > d.len() {
        return corrupt("SELF segment table extends past the end of the file");
    }
    let segments: Vec<SelfSegment> = (0..entry_count)
        .map(|i| {
            let b = SELF_HEADER_SIZE + i * SEGMENT_ENTRY_SIZE;
            SelfSegment {
                index: i,
                props: u64le(d, b),
                offset: u64le(d, b + 0x08),
                file_size: u64le(d, b + 0x10),
            }
        })
        .collect();

    let elf_off = locate_elf(d, table_end)?;
    let info = parse_elf(d, elf_off)?;

    let blocked: Vec<String> = segments
        .iter()
        .filter(|s| s.props & (PROP_ENCRYPTED | PROP_COMPRESSED) != 0)
        .map(|s| {
            let enc = s.props & PROP_ENCRYPTED != 0;
            let comp = s.props & PROP_COMPRESSED != 0;
            let what = match (enc, comp) {
                (true, true) => "encrypted/compressed",
                (true, false) => "encrypted",
                _ => "compressed",
            };
            format!("#{} ({what})", s.index)
        })
        .collect();
    if !blocked.is_empty() {
        return Err(Error::Unsupported(format!(
            "SELF segments {} are encrypted or compressed; rebuilding a plain ELF needs \
             console-side keys, which pkgmaster does not include",
            blocked.join(", ")
        )));
    }

    let mut warnings = Vec::new();
    if declared_size != 0 && declared_size != d.len() as u64 {
        warnings.push(format!(
            "SELF header declares {declared_size} bytes but the file is {}",
            d.len()
        ));
    }

    let phent = if info.e_phentsize == 0 { 56 } else { info.e_phentsize as u64 };
    let phdr_bytes = info.e_phnum as u64 * phent;
    let mut total = (elf_off as u64 + info.e_phoff + phdr_bytes).max(0x40);
    for p in &info.phdrs {
        if p.p_filesz != 0 {
            let end = p.p_offset.checked_add(p.p_filesz);
            total = total.max(end.ok_or_else(|| Error::Corrupt("program header overflows".into()))?);
        }
    }
    if total > 1 << 32 {
        return corrupt(format!("rebuilt ELF would be {total} bytes, which is implausible"));
    }
    let mut out = vec![0u8; total as usize];

    // Copy the ELF header and program header table exactly as they appear.
    out[..0x40].copy_from_slice(&d[elf_off..elf_off + 0x40]);
    let src = elf_off + info.e_phoff as usize;
    let dst = info.e_phoff as usize;
    out[dst..dst + phdr_bytes as usize].copy_from_slice(&d[src..src + phdr_bytes as usize]);

    let mut unused: Vec<SelfSegment> = segments.into_iter().filter(|s| s.file_size != 0).collect();
    let (mut copied, mut zero_filled) = (0, 0);
    for p in &info.phdrs {
        if p.p_filesz == 0 {
            continue;
        }
        let Some(pos) = unused.iter().position(|s| s.file_size == p.p_filesz) else {
            zero_filled += 1;
            warnings.push(format!(
                "program header #{} (0x{:X}) has no matching SELF segment; left zero-filled",
                p.index, p.p_type
            ));
            continue;
        };
        let seg = unused.remove(pos);
        let end = seg.offset.checked_add(seg.file_size).filter(|&e| e <= d.len() as u64);
        if end.is_none() {
            return corrupt(format!("SELF segment #{} runs past the end of the file", seg.index));
        }
        let (so, n, po) = (seg.offset as usize, seg.file_size as usize, p.p_offset as usize);
        out[po..po + n].copy_from_slice(&d[so..so + n]);
        copied += 1;
    }

    if !unused.is_empty() {
        warnings.push(format!("{} SELF segment(s) were not referenced by any phdr", unused.len()));
    }
    if info.e_shoff != 0 {
        // Section headers are not in the SELF payloads; clear them so the
        // output stays self-consistent.
        out[0x28..0x30].fill(0);
        out[0x3C..0x3E].fill(0);
        warnings.push("section header table dropped (not present in the SELF payloads)".into());
    }
    if copied == 0 {
        return Err(Error::Conversion("no SELF segment payloads could be mapped to ELF headers".into()));
    }
    let info = parse_elf(&out, 0)?;
    Ok(Conversion { data: out, info, warnings, copied_segments: copied, zero_filled_segments: zero_filled })
}

fn locate_elf(d: &[u8], start: usize) -> Result<usize> {
    let find = |end: usize| d[start..end].windows(4).position(|w| w == ELF_MAGIC).map(|p| p + start);
    find((start + 0x1000).min(d.len()))
        .or_else(|| find(d.len()))
        .ok_or_else(|| Error::Corrupt("SELF container has no embedded ELF header".into()))
}
