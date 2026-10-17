"""Synthetic sample generation.

Shippable package samples do not exist (real ones are copyrighted and often
encrypted), so this module builds structurally valid PFS images, PS4/PS5
containers, ELF modules and SELF wrappers. Tests use it as a fixture source,
and ``python -m pkgmaster.synth out.pkg`` writes a demo package you can run the
CLI against.
"""

from __future__ import annotations

import struct

from pkgmaster.crypto import compute_keys_sha3, pfs_gen_xts_key, xts_crypt_sector
from pkgmaster.elf import parse_elf
from pkgmaster.pfs import PFS_MAGIC

PFS_BLOCK = 0x10000
CONTENT_ID = "EP0000-CUSA00000_00-0000000000000000"
PASSCODE = "0123456789abcdef0123456789abcdef"
PARAM_SFO = b"\x00PSF" + b"\x01\x01\x00\x00" + b"\x00" * 16

INODE_D32 = 0xA8
INODE_S32 = 0x2C8
INODE_S64 = 0x310

CONTIG_MARKER_32 = 0xFFFFFFFF
CONTIG_MARKER_64 = 0xFFFFFFFFFFFFFFFF


# --------------------------------------------------------------------------
# PFS


class _Node:
    def __init__(self, number: int, name: str, parent: int | None, is_dir: bool):
        self.number = number
        self.name = name
        self.parent = parent
        self.is_dir = is_dir
        self.children: list[int] = []
        self.data = b""


def _flatten(tree: dict) -> tuple[list[_Node], int]:
    nodes: list[_Node] = [_Node(0, "", None, True), _Node(1, "uroot", 0, True)]
    nodes[0].children = [1]

    def add(children: dict, parent: int) -> None:
        for name, value in children.items():
            number = len(nodes)
            if isinstance(value, dict):
                node = _Node(number, name, parent, True)
                nodes.append(node)
                nodes[parent].children.append(number)
                add(value, number)
            else:
                node = _Node(number, name, parent, False)
                node.data = value
                nodes.append(node)
                nodes[parent].children.append(number)

    add(tree, 1)
    return nodes, len(nodes)


def _dirent(inode: int, type_code: int, name: str) -> bytes:
    name_bytes = name.encode("ascii")
    entsize = len(name_bytes) + 17
    if entsize % 8:
        entsize += 8 - entsize % 8
    out = struct.pack("<Iiii", inode, type_code, len(name_bytes), entsize)
    out += name_bytes
    out += b"\x00" * (entsize - len(out))
    return out


def _dir_payload(nodes: list[_Node], number: int) -> bytes:
    node = nodes[number]
    out = _dirent(number, 4, ".") + _dirent(node.parent or 0, 5, "..")
    for child in node.children:
        child_node = nodes[child]
        out += _dirent(child, 3 if child_node.is_dir else 2, child_node.name)
    out += b"\x00" * (PFS_BLOCK - len(out))
    return out


def _inode_record(
    *,
    mode: int,
    nlink: int,
    flags: int,
    size: int,
    size_compressed: int,
    blocks: int,
    direct: list[int],
    indirect: list[int],
    signed: bool,
    is64: bool,
    time_sec: int = 1_600_000_000,
) -> bytes:
    header = struct.pack(
        "<HHIqqqqqqIIIIIIQQI",
        mode,
        nlink,
        flags,
        size,
        size_compressed,
        time_sec,
        time_sec,
        time_sec,
        time_sec,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        blocks,
    )
    assert len(header) == 0x64, len(header)

    values = list(direct) + list(indirect)
    if not signed:
        return header + struct.pack("<12I", *direct) + struct.pack("<5I", *indirect)
    if not is64:
        out = bytearray(header)
        for value in values:
            out += b"\x00" * 32 + struct.pack("<I", value)
        return bytes(out)
    out = bytearray(header)
    out += b"\x00" * 4  # align the 64-bit pointer table
    for value in values:
        out += b"\x00" * 32 + struct.pack("<Q", value)
    return bytes(out)


def build_pfs(
    tree: dict,
    *,
    signed: bool = False,
    inode_bits: int = 32,
    encrypted: bool = False,
    ekpfs: bytes | None = None,
    seed: bytes = b"\x11" * 16,
    version: int = 1,
) -> bytes:
    """Build a minimal but structurally valid PFS image."""
    nodes, count = _flatten(tree)
    inode_size = INODE_D32 if not signed else (INODE_S64 if inode_bits == 64 else INODE_S32)
    inode_blocks = max(1, -(-count * inode_size // PFS_BLOCK))

    # Block plan: 0 = header, 1..n = inode table, then dirs, then file data.
    next_block = 1 + inode_blocks
    dir_blocks: dict[int, int] = {}
    dir_payloads: dict[int, bytes] = {}
    for node in nodes:
        if node.is_dir:
            payload = _dir_payload(nodes, node.number)
            assert len(payload) <= PFS_BLOCK
            dir_payloads[node.number] = payload
            dir_blocks[node.number] = next_block
            next_block += 1

    file_blocks: dict[int, int] = {}
    file_block_counts: dict[int, int] = {}
    for node in nodes:
        if node.is_dir or not node.data:
            continue
        blocks = -(-len(node.data) // PFS_BLOCK)
        file_blocks[node.number] = next_block
        file_block_counts[node.number] = blocks
        next_block += blocks

    total_blocks = next_block
    marker = CONTIG_MARKER_64 if inode_bits == 64 else CONTIG_MARKER_32

    inode_blobs = bytearray()
    for node in nodes:
        if node.is_dir:
            payload = dir_payloads[node.number]
            direct = [dir_blocks[node.number]] + [0] * 11
            inode_blobs += _inode_record(
                mode=0x4000 | 0x16D,
                nlink=2,
                flags=0x10,
                size=len(payload),
                size_compressed=len(payload),
                blocks=1,
                direct=direct,
                indirect=[0] * 5,
                signed=signed,
                is64=inode_bits == 64,
            )
        else:
            blocks = file_block_counts.get(node.number, 0)
            direct = [file_blocks.get(node.number, 0)] + [0] * 11
            if blocks > 1:
                direct[1] = marker
            inode_blobs += _inode_record(
                mode=0x8000 | 0x16D,
                nlink=1,
                flags=0,
                size=len(node.data),
                size_compressed=len(node.data),
                blocks=blocks,
                direct=direct,
                indirect=[0] * 5,
                signed=signed,
                is64=inode_bits == 64,
            )
    assert len(inode_blobs) == count * inode_size

    mode = (0x8 if not signed else 0x0)
    if signed:
        mode |= 0x1
    if inode_bits == 64:
        mode |= 0x2
    if encrypted:
        mode |= 0x4
    else:
        mode |= 0x8

    header = bytearray(PFS_BLOCK)
    struct.pack_into("<qqq", header, 0x00, version, PFS_MAGIC, 0)
    struct.pack_into("<BBBB", header, 0x18, 0, 1, 0, 0)
    struct.pack_into("<HH", header, 0x1C, mode, 0)
    struct.pack_into("<II", header, 0x20, PFS_BLOCK, 0)
    struct.pack_into("<q", header, 0x28, 1)
    struct.pack_into(
        "<qqqq",
        header,
        0x30,
        count,
        total_blocks,
        inode_blocks,
        0,
    )
    struct.pack_into("<q", header, 0x48, 0)
    header[0x370 : 0x370 + 16] = seed

    image = bytearray(header)
    image += inode_blobs
    image += b"\x00" * ((inode_blocks * PFS_BLOCK) - len(inode_blobs))
    for node in nodes:
        if node.is_dir:
            image += dir_payloads[node.number]
    for node in sorted(file_blocks):
        body = nodes[node].data
        padded = body + b"\x00" * (-len(body) % PFS_BLOCK)
        image += padded

    assert len(image) == total_blocks * PFS_BLOCK, (len(image), total_blocks * PFS_BLOCK)

    if encrypted:
        key = ekpfs if ekpfs is not None else compute_keys_sha3(CONTENT_ID, PASSCODE, 1)
        xts_key = pfs_gen_xts_key(key, bytes(seed), False)
        start = PFS_BLOCK // 0x1000
        for sector in range(start, len(image) // 0x1000):
            offset = sector * 0x1000
            chunk = bytes(image[offset : offset + 0x1000])
            image[offset : offset + 0x1000] = xts_crypt_sector(
                chunk, xts_key, sector, encrypt=True
            )
    return bytes(image)


# --------------------------------------------------------------------------
# containers


def _entry_record(
    entry_id: int,
    name_offset: int,
    flags1: int,
    flags2: int,
    offset: int,
    size: int,
) -> bytes:
    return struct.pack(">IIIIII", entry_id, name_offset, flags1, flags2, offset, size) + b"\x00" * 8


def _align(value: int, alignment: int = 16) -> int:
    return (value + alignment - 1) & ~(alignment - 1)


def _encrypt_entry_payload(
    stored: bytes, meta: bytes, content_id: str, passcode: str, flags2: int
) -> bytes:
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

    from pkgmaster.crypto import compute_keys, sha256

    key_index = (flags2 & 0xF000) >> 12
    seed = compute_keys(content_id, passcode, key_index)
    digest = sha256(meta + seed)
    key, iv = digest[16:32], digest[:16]
    encryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    return encryptor.update(stored) + encryptor.finalize()


def build_container(
    *,
    content_id: str = CONTENT_ID,
    param_sfo: bytes = PARAM_SFO,
    icon: bytes | None = None,
    encrypted_license: bytes | None = None,
    passcode: str | None = None,
    license_key_index: int = 2,
    pfs_bytes: bytes | None = None,
    pfs_signed_digest: bytes | None = None,
) -> bytes:
    """Build a ``\\x7FCNT`` container, optionally with a PFS image appended."""
    specs: list[tuple[int, str, bytes, int, int]] = [
        (0x1000, "param.sfo", param_sfo, 0, 0)
    ]
    if icon is not None:
        specs.append((0x1200, "icon0.png", icon, 0, 0))
    if encrypted_license is not None:
        specs.append(
            (
                0x0400,
                "license.dat",
                encrypted_license,
                0x80000000,
                (license_key_index & 0xF) << 12,
            )
        )

    names: list[str] = [""]
    name_offsets: dict[str, int] = {"": 0}
    for _, name, *_ in specs:
        if name not in name_offsets:
            name_offsets[name] = sum(len(n) + 1 for n in names)
            names.append(name)
    name_table = b"".join(n.encode("ascii") + b"\x00" for n in names)

    entry_table_offset = 0x1000
    entry_count = 1 + len(specs)
    names_offset = _align(entry_table_offset + entry_count * 0x20)
    cursor = _align(names_offset + len(name_table))

    records: list[tuple[int, int, int, int, int, bytes]] = [
        (0x0200, 0, 0, 0, names_offset, name_table)
    ]
    for entry_id, name, payload, flags1, flags2 in specs:
        encrypted = bool(flags1 & 0x80000000)
        stored = payload + b"\x00" * (-len(payload) % 16) if encrypted else payload
        meta = _entry_record(
            entry_id, name_offsets[name], flags1, flags2, cursor, len(stored)
        )
        if encrypted:
            if passcode is None:
                raise ValueError("encrypted entries need a passcode")
            stored = _encrypt_entry_payload(stored, meta, content_id, passcode, flags2)
        records.append((entry_id, name_offsets[name], flags1, flags2, cursor, stored))
        cursor = _align(cursor + len(stored))

    if pfs_bytes is not None:
        pfs_offset = _align(cursor, 0x1000)
        package_size = pfs_offset + len(pfs_bytes)
    else:
        pfs_offset = 0
        package_size = cursor

    header = bytearray(0x5A0)
    header[0:4] = b"\x7fCNT"
    struct.pack_into(">I", header, 0x04, 0x80000000)
    struct.pack_into(">I", header, 0x10, len(records))
    struct.pack_into(">HH", header, 0x14, 0, len(records))
    struct.pack_into(">II", header, 0x18, entry_table_offset, len(records) * 0x20)
    struct.pack_into(">QQ", header, 0x20, 0, package_size)
    header[0x40 : 0x40 + len(content_id)] = content_id.encode("ascii")
    struct.pack_into(">III", header, 0x70, 0, 0x1A, 0)
    struct.pack_into(">II", header, 0x80, 0x20200722, 0x01FE52E9)
    struct.pack_into(">I", header, 0x404, 1 if pfs_bytes is not None else 0)
    struct.pack_into(">Q", header, 0x408, 0)
    struct.pack_into(">QQ", header, 0x410, pfs_offset, len(pfs_bytes) if pfs_bytes else 0)
    struct.pack_into(">QQ", header, 0x430, package_size, package_size)
    if pfs_signed_digest:
        header[0x460 : 0x460 + 32] = pfs_signed_digest

    out = bytearray(header)
    out += b"\x00" * (entry_table_offset - len(out))
    for entry_id, n_offset, flags1, flags2, offset, stored in records:
        out += _entry_record(entry_id, n_offset, flags1, flags2, offset, len(stored))
    for entry_id, n_offset, flags1, flags2, offset, stored in records:
        out += b"\x00" * (offset - len(out))
        out += stored
    if pfs_bytes is not None:
        out += b"\x00" * (pfs_offset - len(out))
        out += pfs_bytes
    return bytes(out)


def build_pkg(pfs_bytes: bytes, **kwargs) -> bytes:
    return build_container(pfs_bytes=pfs_bytes, **kwargs)


def build_ps5(inner_tree: dict, **kwargs) -> bytes:
    """Build a PS5 ``\\x7FFIH`` image with a plaintext outer PFS."""
    inner = build_pfs(inner_tree, version=2)
    outer = build_pfs({"pfs_image.dat": inner}, version=2)
    cnt = build_container(**kwargs)

    fih_size = 0x10000
    pfs_offset = fih_size
    cnt_offset = (pfs_offset + len(outer) + 0xFFF) & ~0xFFF

    header = bytearray(fih_size)
    header[0:4] = b"\x7fFIH"
    header[5] = 0x00  # debug
    struct.pack_into("<Q", header, 0x10, pfs_offset)
    struct.pack_into("<Q", header, 0x18, len(outer))
    struct.pack_into("<Q", header, 0x58, cnt_offset)
    struct.pack_into("<Q", header, 0xA0, len(cnt))

    out = bytearray(header)
    out += outer
    out += b"\x00" * (cnt_offset - len(out))
    out += cnt
    return bytes(out)


# --------------------------------------------------------------------------
# ELF / SELF


def build_elf(payloads: list[bytes], *, e_type: int = 0xFE10) -> bytes:
    """Build a minimal ELF64 with one PT_LOAD per payload plus an SCE comment."""
    phnum = len(payloads) + 1
    phoff = 0x40
    phsize = 0x38
    header_size = phoff + phnum * phsize
    cursor = (header_size + 0xFFF) & ~0xFFF

    segments = []
    for index, payload in enumerate(payloads):
        offset = cursor
        cursor += len(payload)
        cursor = (cursor + 0xF) & ~0xF
        segments.append(
            {
                "type": 0x1,
                "flags": 0x5 if index == 0 else 0x6,
                "offset": offset,
                "vaddr": 0x1000 * (index + 1),
                "filesz": len(payload),
                "memsz": len(payload),
                "align": 0x1000,
                "data": payload,
            }
        )
    comment = b"\x01\x00\x00\x00"
    segments.append(
        {
            "type": 0x6FFFFF00,
            "flags": 0x4,
            "offset": cursor,
            "vaddr": 0,
            "filesz": len(comment),
            "memsz": len(comment),
            "align": 4,
            "data": comment,
        }
    )
    total = cursor + len(comment)

    out = bytearray(total)
    out[0:4] = b"\x7fELF"
    out[4] = 2
    out[5] = 1
    out[6] = 1
    out[7] = 0x9  # FreeBSD, as on real SCE modules
    struct.pack_into("<HHI", out, 0x10, e_type, 0x3E, 1)
    struct.pack_into("<Q", out, 0x18, segments[0]["vaddr"] + 0x10)  # entry
    struct.pack_into("<Q", out, 0x20, phoff)
    struct.pack_into("<H", out, 0x34, 0x40)  # e_ehsize
    struct.pack_into("<H", out, 0x36, phsize)
    struct.pack_into("<H", out, 0x38, phnum)
    for index, segment in enumerate(segments):
        base = phoff + index * phsize
        struct.pack_into(
            "<IIQQQQQQ",
            out,
            base,
            segment["type"],
            segment["flags"],
            segment["offset"],
            segment["vaddr"],
            segment["vaddr"],
            segment["filesz"],
            segment["memsz"],
            segment["align"],
        )
        out[segment["offset"] : segment["offset"] + len(segment["data"])] = segment["data"]
    return bytes(out)


def build_self(
    elf: bytes,
    *,
    variant: str = "PS4",
    encrypt_first: bool = False,
    compress_first: bool = False,
) -> bytes:
    """Wrap an ELF in a SELF container, one segment per data-bearing phdr."""
    info = parse_elf(elf)
    data_phdrs = [p for p in info.phdrs if p.p_filesz]
    magic = 0x1D3D154F if variant == "PS4" else 0xEEF51454
    entry_count = len(data_phdrs)
    table_end = 0x20 + entry_count * 0x20
    elf_offset = table_end
    payload_start = elf_offset + 0x40 + info.e_phnum * (info.e_phentsize or 0x38)
    payload_start = (payload_start + 0xF) & ~0xF

    payloads = []
    cursor = payload_start
    for index, phdr in enumerate(data_phdrs):
        props = 0
        if index == 0 and encrypt_first:
            props |= 0x2
        if index == 0 and compress_first:
            props |= 0x8
        payloads.append(
            {
                "props": props,
                "offset": cursor,
                "size": phdr.p_filesz,
                "memsz": phdr.p_memsz,
                "data": elf[phdr.p_offset : phdr.p_offset + phdr.p_filesz],
            }
        )
        cursor += phdr.p_filesz
        cursor = (cursor + 0xF) & ~0xF

    total = cursor
    out = bytearray(total)
    struct.pack_into("<I6BHHH", out, 0, magic, 3, 0, 1, 0, 1, 1, 0, 0x90, 0x40)
    struct.pack_into("<Q", out, 0x10, total)
    struct.pack_into("<HHI", out, 0x18, entry_count, 0, 0)
    for index, segment in enumerate(payloads):
        base = 0x20 + index * 0x20
        struct.pack_into(
            "<QQQQ",
            out,
            base,
            segment["props"],
            segment["offset"],
            segment["size"],
            segment["memsz"],
        )
    out[elf_offset : elf_offset + 0x40 + info.e_phnum * (info.e_phentsize or 0x38)] = elf[
        : 0x40 + info.e_phnum * (info.e_phentsize or 0x38)
    ]
    for segment in payloads:
        out[segment["offset"] : segment["offset"] + segment["size"]] = segment["data"]
    return bytes(out)


DEFAULT_TREE = {
    "eboot.bin": build_self(build_elf([b"\x90" * 64, b"DATA" * 16])),
    "sce_sys": {"param.sfo": PARAM_SFO, "icon0.png": b"\x89PNG\r\n\x1a\n" + b"\x00" * 32},
    "data.bin": b"\xAB" * 4096,
}


# --------------------------------------------------------------------------
# demo package


def build_sample(*, ps5: bool = False, encrypted: bool = False) -> bytes:
    """Build a complete demo package whose ``eboot.bin`` is a SELF-wrapped ELF."""
    if ps5:
        return build_ps5(DEFAULT_TREE)
    ekpfs = compute_keys_sha3(CONTENT_ID, PASSCODE, 1) if encrypted else None
    pfs = build_pfs(DEFAULT_TREE, encrypted=encrypted, ekpfs=ekpfs)
    return build_pkg(pfs)


def main(argv: list[str] | None = None) -> int:
    import argparse
    import sys

    parser = argparse.ArgumentParser(
        prog="python -m pkgmaster.synth",
        description="Write a synthetic PS4/PS5 package for trying out pkgmaster.",
    )
    parser.add_argument("output", help="output .pkg path")
    parser.add_argument("--ps5", action="store_true", help="build a PS5 FIH image")
    parser.add_argument(
        "--encrypted", action="store_true", help="AES-XTS encrypt the PFS image"
    )
    args = parser.parse_args(argv)

    data = build_sample(ps5=args.ps5, encrypted=args.encrypted)
    with open(args.output, "wb") as handle:
        handle.write(data)
    print(f"wrote {args.output} ({len(data)} bytes)")
    if args.encrypted:
        print(f"passcode: {PASSCODE}")
    print("note: this is synthetic data, not a real game package")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
