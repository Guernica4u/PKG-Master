"""SELF (signed ELF) container handling.

``eboot.bin`` inside a package is often a SELF wrapper around an ELF64 image.
The wrapper adds a 32-byte header and a segment table whose entries point at
payload blobs; those blobs are copied back into the embedded ELF's program
headers to produce a normal ELF.

Segment property bits (from the documented PS4 SELF layout)::

    bit 0  order
    bit 1  encrypted
    bit 2  signed
    bit 3  compressed
    bits 8-10 window bits, 11-14 block size, 16 digest, 17 extent, 20 meta

Encrypted or compressed segments cannot be rebuilt without console keys, so
they are rejected explicitly instead of emitting a corrupt ELF.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .binary import u16le, u32le, u64le
from .elf import ELF_MAGIC, SELF_MAGICS, ElfInfo, is_elf, is_self, parse_elf
from .errors import CorruptImage, ConversionError, UnsupportedFeature

SELF_HEADER_SIZE = 0x20
SEGMENT_ENTRY_SIZE = 0x20

PROP_ENCRYPTED = 1 << 1
PROP_SIGNED = 1 << 2
PROP_COMPRESSED = 1 << 3


@dataclass
class SelfSegment:
    index: int
    props: int
    offset: int
    file_size: int
    memory_size: int

    @property
    def encrypted(self) -> bool:
        return bool(self.props & PROP_ENCRYPTED)

    @property
    def compressed(self) -> bool:
        return bool(self.props & PROP_COMPRESSED)

    def to_dict(self) -> dict:
        return {
            "index": self.index,
            "props": f"0x{self.props:X}",
            "encrypted": self.encrypted,
            "compressed": self.compressed,
            "offset": f"0x{self.offset:X}",
            "file_size": self.file_size,
            "memory_size": self.memory_size,
        }


@dataclass
class SelfHeader:
    magic: int
    variant: str
    version: int
    mode: int
    endian: int
    content_type: int
    key_type: int
    header_size: int
    meta_size: int
    file_size: int
    entry_count: int
    flags: int
    segments: list[SelfSegment] = field(default_factory=list)
    elf_offset: int = 0

    def to_dict(self) -> dict:
        return {
            "variant": self.variant,
            "version": self.version,
            "mode": f"0x{self.mode:X}",
            "endian": self.endian,
            "content_type": f"0x{self.content_type:X}",
            "key_type": f"0x{self.key_type:X}",
            "header_size": f"0x{self.header_size:X}",
            "meta_size": self.meta_size,
            "file_size": self.file_size,
            "entry_count": self.entry_count,
            "flags": f"0x{self.flags:X}",
            "elf_offset": f"0x{self.elf_offset:X}",
            "segments": [s.to_dict() for s in self.segments],
        }


@dataclass
class SelfConversion:
    """Result of rebuilding an ELF out of a SELF."""

    data: bytes
    info: ElfInfo
    warnings: list[str] = field(default_factory=list)
    copied_segments: int = 0
    zero_filled_segments: int = 0

    def to_dict(self) -> dict:
        return {
            "elf": self.info.to_dict(),
            "size": len(self.data),
            "copied_segments": self.copied_segments,
            "zero_filled_segments": self.zero_filled_segments,
            "warnings": list(self.warnings),
        }


def parse_self(data: bytes) -> tuple[SelfHeader, ElfInfo, int]:
    """Parse a SELF header and the embedded ELF, returning ``(header, elf, elf_offset)``."""
    if not is_self(data):
        raise CorruptImage("input does not start with a SELF magic")
    magic = u32le(data, 0)
    variant = "PS4" if magic == 0x1D3D154F else "PS5"

    header = SelfHeader(
        magic=magic,
        variant=variant,
        version=data[4],
        mode=data[5],
        endian=data[6],
        content_type=data[8],
        key_type=data[9],
        header_size=u16le(data, 0x0C),
        meta_size=u16le(data, 0x0E),
        file_size=u64le(data, 0x10),
        entry_count=u16le(data, 0x18),
        flags=u16le(data, 0x1A),
    )
    if header.entry_count > 4096:
        raise CorruptImage(f"implausible SELF segment count {header.entry_count}")

    table_end = SELF_HEADER_SIZE + header.entry_count * SEGMENT_ENTRY_SIZE
    if table_end > len(data):
        raise CorruptImage("SELF segment table extends past the end of the file")
    for i in range(header.entry_count):
        base = SELF_HEADER_SIZE + i * SEGMENT_ENTRY_SIZE
        header.segments.append(
            SelfSegment(
                index=i,
                props=u64le(data, base + 0x00),
                offset=u64le(data, base + 0x08),
                file_size=u64le(data, base + 0x10),
                memory_size=u64le(data, base + 0x18),
            )
        )

    elf_offset = _locate_elf(data, table_end)
    header.elf_offset = elf_offset
    info = parse_elf(data, elf_offset)
    return header, info, elf_offset


def _locate_elf(data: bytes, start: int) -> int:
    """Find the embedded ELF header that follows the SELF segment table."""
    search_end = min(len(data), start + 0x1000)
    found = data.find(ELF_MAGIC, start, search_end)
    if found < 0:
        found = data.find(ELF_MAGIC, start)
    if found < 0:
        raise CorruptImage("SELF container has no embedded ELF header")
    return found


def describe(data: bytes) -> dict:
    """Describe a SELF or ELF blob without converting it."""
    if is_self(data):
        header, info, _ = parse_self(data)
        return {"container": "SELF", "self": header.to_dict(), "elf": info.to_dict()}
    if is_elf(data):
        info = parse_elf(data)
        return {"container": "ELF", "elf": info.to_dict()}
    raise CorruptImage("input is neither SELF nor ELF")


def to_elf(data: bytes) -> SelfConversion:
    """Return a standard ELF for ``data``.

    Plain ELF input is passed through unchanged. SELF input is rebuilt by
    mapping each segment payload onto the embedded ELF program headers.
    """
    if is_elf(data):
        return SelfConversion(data=data, info=parse_elf(data), warnings=[])

    header, info, elf_offset = parse_self(data)

    blocked = [s for s in header.segments if s.encrypted or s.compressed]
    if blocked:
        details = ", ".join(
            f"#{s.index} ({'encrypted' if s.encrypted else ''}"
            f"{'/' if s.encrypted and s.compressed else ''}"
            f"{'compressed' if s.compressed else ''})"
            for s in blocked
        )
        raise UnsupportedFeature(
            "SELF segments " + details + " are encrypted or compressed; rebuilding a "
            "plain ELF needs console-side keys, which pkgmaster does not include"
        )

    warnings: list[str] = []
    if header.file_size and header.file_size != len(data):
        warnings.append(
            f"SELF header declares {header.file_size} bytes but the file is {len(data)}"
        )

    phdr_bytes = info.e_phnum * (info.e_phentsize or 56)
    header_bytes = elf_offset + info.e_phoff + phdr_bytes
    total = header_bytes
    for phdr in info.phdrs:
        if phdr.p_filesz:
            total = max(total, phdr.p_offset + phdr.p_filesz)
    out = bytearray(total)

    # Copy the ELF header and the program header table exactly as they appear.
    ehdr_len = 0x40
    out[0:ehdr_len] = data[elf_offset : elf_offset + ehdr_len]
    phdr_src = elf_offset + info.e_phoff
    out[info.e_phoff : info.e_phoff + phdr_bytes] = data[
        phdr_src : phdr_src + phdr_bytes
    ]

    unused = [s for s in header.segments if s.file_size]
    copied = 0
    zero_filled = 0
    for phdr in info.phdrs:
        if not phdr.p_filesz:
            continue
        match = _match_segment(unused, phdr.p_filesz)
        if match is None:
            zero_filled += 1
            warnings.append(
                f"program header #{phdr.index} ({phdr.type_name}) has no matching "
                "SELF segment; left zero-filled"
            )
            continue
        unused.remove(match)
        out[phdr.p_offset : phdr.p_offset + match.file_size] = data[
            match.offset : match.offset + match.file_size
        ]
        copied += 1

    if unused:
        warnings.append(f"{len(unused)} SELF segment(s) were not referenced by any phdr")
    if info.e_shoff:
        # Section headers usually live in a segment that we may not have copied;
        # clearing them keeps the output self-consistent.
        out[0x28:0x30] = b"\x00" * 8
        out[0x3C:0x3E] = b"\x00" * 2
        warnings.append("section header table dropped (not present in the SELF payloads)")

    if not copied:
        raise ConversionError("no SELF segment payloads could be mapped to ELF headers")

    return SelfConversion(
        data=bytes(out),
        info=parse_elf(bytes(out)),
        warnings=warnings,
        copied_segments=copied,
        zero_filled_segments=zero_filled,
    )


def _match_segment(segments: list[SelfSegment], file_size: int) -> SelfSegment | None:
    """Pick the SELF segment that best corresponds to a program header."""
    for segment in segments:
        if segment.file_size == file_size:
            return segment
    return None
