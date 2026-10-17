"""ELF parsing and SCE-flavoured naming.

PS4 and PS5 executables are ELF64 x86-64 images with Sony-specific
``e_type`` values and ``p_type`` values. This module only *reads* ELF; the
SELF container logic lives in :mod:`pkgmaster.self`.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .binary import u16le, u32le, u64le
from .errors import CorruptImage

ELF_MAGIC = b"\x7fELF"
SELF_MAGICS = frozenset({0x1D3D154F, 0xEEF51454})  # PS4 4F153D1D, PS5 5414F5EE

ELF_TYPE_NAMES = {
    0x0: "ET_NONE",
    0x1: "ET_REL",
    0x2: "ET_EXEC",
    0x3: "ET_DYN",
    0x4: "ET_CORE",
    0xFE00: "ET_SCE_EXEC",
    0xFE0C: "ET_SCE_STUBLIB",
    0xFE10: "ET_SCE_EXEC_ASLR",
    0xFE18: "ET_SCE_DYNAMIC",
}

MACHINE_NAMES = {
    0x03: "EM_386",
    0x3E: "EM_X86_64",
    0x28: "EM_ARM",
    0xB7: "EM_AARCH64",
}

PROGRAM_TYPE_NAMES = {
    0x0: "PT_NULL",
    0x1: "PT_LOAD",
    0x2: "PT_DYNAMIC",
    0x3: "PT_INTERP",
    0x4: "PT_NOTE",
    0x5: "PT_SHLIB",
    0x6: "PT_PHDR",
    0x7: "PT_TLS",
    0x6474E550: "PT_GNU_EH_FRAME",
    0x6474E551: "PT_GNU_STACK",
    0x6474E552: "PT_GNU_RELRO",
    0x60000000: "PT_SCE_RELA",
    0x61000000: "PT_SCE_DYNLIBDATA",
    0x61000001: "PT_SCE_PROCPARAM",
    0x61000002: "PT_SCE_MODULE_PARAM",
    0x61000010: "PT_SCE_RELRO",
    0x6FFFFF00: "PT_SCE_COMMENT",
    0x6FFFFF01: "PT_SCE_VERSION",
}

OSABI_NAMES = {
    0x0: "System V",
    0x9: "FreeBSD",
    0x0C: "OpenBSD",
}


@dataclass
class ProgramHeader:
    index: int
    p_type: int
    p_flags: int
    p_offset: int
    p_vaddr: int
    p_paddr: int
    p_filesz: int
    p_memsz: int
    p_align: int

    @property
    def type_name(self) -> str:
        return PROGRAM_TYPE_NAMES.get(self.p_type, f"0x{self.p_type:X}")

    def to_dict(self) -> dict:
        return {
            "index": self.index,
            "type": f"0x{self.p_type:X}",
            "type_name": self.type_name,
            "flags": f"0x{self.p_flags:X}",
            "offset": self.p_offset,
            "vaddr": f"0x{self.p_vaddr:X}",
            "filesz": self.p_filesz,
            "memsz": self.p_memsz,
            "align": self.p_align,
        }


@dataclass
class ElfInfo:
    class_bits: int
    little_endian: bool
    os_abi: int
    e_type: int
    e_machine: int
    e_entry: int
    e_phoff: int
    e_phentsize: int
    e_phnum: int
    e_shoff: int
    e_shnum: int
    e_shstrndx: int
    phdrs: list[ProgramHeader] = field(default_factory=list)
    header_offset: int = 0

    @property
    def type_name(self) -> str:
        return ELF_TYPE_NAMES.get(self.e_type, f"0x{self.e_type:X}")

    @property
    def machine_name(self) -> str:
        return MACHINE_NAMES.get(self.e_machine, f"0x{self.e_machine:X}")

    @property
    def osabi_name(self) -> str:
        return OSABI_NAMES.get(self.os_abi, f"0x{self.os_abi:X}")

    @property
    def is_sce(self) -> bool:
        return self.e_type >= 0xFE00

    def load_segments(self) -> list[ProgramHeader]:
        return [p for p in self.phdrs if p.p_type == 0x1 and p.p_filesz]

    def to_dict(self) -> dict:
        return {
            "class": f"ELF{self.class_bits}",
            "endian": "little" if self.little_endian else "big",
            "os_abi": self.osabi_name,
            "type": f"0x{self.e_type:X}",
            "type_name": self.type_name,
            "machine": self.machine_name,
            "entry": f"0x{self.e_entry:X}",
            "phoff": self.e_phoff,
            "phnum": self.e_phnum,
            "shoff": self.e_shoff,
            "shnum": self.e_shnum,
            "sce": self.is_sce,
            "phdrs": [p.to_dict() for p in self.phdrs],
        }


def is_elf(data: bytes) -> bool:
    return len(data) >= 4 and data[:4] == ELF_MAGIC


def is_self(data: bytes) -> bool:
    return len(data) >= 4 and u32le(data, 0) in SELF_MAGICS


def parse_elf(data: bytes, offset: int = 0, header_offset: int | None = None) -> ElfInfo:
    """Parse an ELF header and program header table from ``data``.

    ``offset`` is where the ELF header begins; ``header_offset`` records where
    that ELF lives inside a larger container (used when rebuilding a SELF).
    """
    if len(data) < offset + 0x40:
        raise CorruptImage("input is too small to contain an ELF header")
    if data[offset : offset + 4] != ELF_MAGIC:
        raise CorruptImage(f"no ELF magic at offset 0x{offset:X}")

    elf_class = data[offset + 4]
    endian = data[offset + 5]
    if endian != 1:
        raise CorruptImage("only little-endian ELF images are supported")
    if elf_class not in (1, 2):
        raise CorruptImage(f"unsupported ELF class {elf_class}")

    os_abi = data[offset + 7]
    if elf_class == 2:
        e_type = u16le(data, offset + 0x10)
        e_machine = u16le(data, offset + 0x12)
        e_entry = u64le(data, offset + 0x18)
        e_phoff = u64le(data, offset + 0x20)
        e_shoff = u64le(data, offset + 0x28)
        e_phentsize = u16le(data, offset + 0x36)
        e_phnum = u16le(data, offset + 0x38)
        e_shnum = u16le(data, offset + 0x3C)
        e_shstrndx = u16le(data, offset + 0x3E)
        phdr_fmt_size = 56
    else:
        e_type = u16le(data, offset + 0x10)
        e_machine = u16le(data, offset + 0x12)
        e_entry = u32le(data, offset + 0x18)
        e_phoff = u32le(data, offset + 0x1C)
        e_shoff = u32le(data, offset + 0x20)
        e_phentsize = u16le(data, offset + 0x2A)
        e_phnum = u16le(data, offset + 0x2C)
        e_shnum = u16le(data, offset + 0x30)
        e_shstrndx = u16le(data, offset + 0x32)
        phdr_fmt_size = 32

    info = ElfInfo(
        class_bits=64 if elf_class == 2 else 32,
        little_endian=True,
        os_abi=os_abi,
        e_type=e_type,
        e_machine=e_machine,
        e_entry=e_entry,
        e_phoff=e_phoff,
        e_phentsize=e_phentsize,
        e_phnum=e_phnum,
        e_shoff=e_shoff,
        e_shnum=e_shnum,
        e_shstrndx=e_shstrndx,
        header_offset=offset if header_offset is None else header_offset,
    )

    if e_phnum:
        size = e_phentsize or phdr_fmt_size
        if size < phdr_fmt_size:
            raise CorruptImage(f"program header entry size {size} is too small")
        table_end = offset + e_phoff + e_phnum * size
        if table_end > len(data):
            raise CorruptImage(
                f"program header table needs 0x{table_end:X} bytes but only "
                f"0x{len(data):X} are available"
            )
        for i in range(e_phnum):
            base = offset + e_phoff + i * size
            if elf_class == 2:
                info.phdrs.append(
                    ProgramHeader(
                        index=i,
                        p_type=u32le(data, base + 0x00),
                        p_flags=u32le(data, base + 0x04),
                        p_offset=u64le(data, base + 0x08),
                        p_vaddr=u64le(data, base + 0x10),
                        p_paddr=u64le(data, base + 0x18),
                        p_filesz=u64le(data, base + 0x20),
                        p_memsz=u64le(data, base + 0x28),
                        p_align=u64le(data, base + 0x30),
                    )
                )
            else:
                info.phdrs.append(
                    ProgramHeader(
                        index=i,
                        p_type=u32le(data, base + 0x00),
                        p_offset=u32le(data, base + 0x04),
                        p_vaddr=u32le(data, base + 0x08),
                        p_paddr=u32le(data, base + 0x0C),
                        p_filesz=u32le(data, base + 0x10),
                        p_memsz=u32le(data, base + 0x14),
                        p_flags=u32le(data, base + 0x18),
                        p_align=u32le(data, base + 0x1C),
                    )
                )
    return info
