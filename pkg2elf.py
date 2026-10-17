#!/usr/bin/env python3
"""
pkg2elf.py - standalone PS4/PS5 package -> ELF extractor.

One file, one job: open a PS4 `\\x7FCNT` or PS5 `\\x7FFIH` package, find the
executable inside the PFS filesystem, and write it out as a normal ELF64 that
readelf/objdump/Ghidra can open. SELF containers are unwrapped in the process.

    python pkg2elf.py game.pkg -o eboot.elf
    python pkg2elf.py game.pkg --passcode 0123456789abcdef0123456789abcdef
    python pkg2elf.py game.pkg --info
    python pkg2elf.py game.pkg --list
    python pkg2elf.py eboot.bin -o eboot.elf      # raw SELF/ELF also works

Only dependency: `pip install cryptography` (used for AES-XTS).

No keys, content keys or RSA private keys are included. A passcode only opens
fake/debug packages; retail packages need console-side key material that is not
derivable from the package, and this tool will say so instead of guessing.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import struct
import sys

try:
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
except ImportError:  # pragma: no cover
    sys.exit("pkg2elf.py needs the 'cryptography' package:  pip install cryptography")

XTS_SECTOR = 0x1000
CNT_MAGIC = b"\x7fCNT"
FIH_MAGIC = b"\x7fFIH"
ELF_MAGIC = b"\x7fELF"
PFS_MAGIC = 20130315
SELF_MAGICS = {0x1D3D154F: "PS4", 0xEEF51454: "PS5"}


class PkgError(Exception):
    """Any expected failure, reported to the user as a clean message."""


class KeyNeeded(PkgError):
    """Encrypted content with no usable key."""


# ==========================================================================
# crypto
# ==========================================================================


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def compute_keys(content_id: str, passcode: str, index: int = 1) -> bytes:
    """FPKG passcode -> derived key: SHA256(SHA256(BE32(i)) || SHA256(cid48) || passcode)."""
    if len(content_id) != 36:
        raise KeyNeeded(f"content id must be 36 characters, got {len(content_id)}")
    if len(passcode) != 32:
        raise KeyNeeded(f"passcode must be 32 characters, got {len(passcode)}")
    material = (
        sha256(struct.pack(">I", index))
        + sha256(content_id.encode("ascii").ljust(48, b"\x00"))
        + passcode.encode("ascii")
    )
    return sha256(material)


def compute_keys_sha3(content_id: str, passcode: str, index: int = 1) -> bytes:
    """SHA3-256 variant used by some PS5 debug images."""
    if len(content_id) != 36 or len(passcode) != 32:
        raise KeyNeeded("content id must be 36 characters and passcode 32")
    material = (
        hashlib.sha3_256(struct.pack(">I", index)).digest()
        + hashlib.sha3_256(content_id.encode("ascii").ljust(48, b"\x00")).digest()
        + passcode.encode("ascii")
    )
    return hashlib.sha3_256(material).digest()


def pfs_xts_key(ekpfs: bytes, seed: bytes, new_crypt: bool = False) -> bytes:
    """32-byte XTS key ordered data||tweak."""
    base = hmac.new(ekpfs, seed, hashlib.sha256).digest() if new_crypt else ekpfs
    enc = hmac.new(base, struct.pack("<I", 1) + seed, hashlib.sha256).digest()
    return enc[16:32] + enc[:16]  # data key, then tweak key


def xts_sector(data: bytes, key: bytes, sector: int, encrypt: bool) -> bytes:
    if len(data) % 16:
        raise PkgError("XTS input must be a multiple of 16 bytes")
    tweak = struct.pack("<QQ", sector, 0)
    cipher = Cipher(algorithms.AES(key), modes.XTS(tweak))
    ctx = cipher.encryptor() if encrypt else cipher.decryptor()
    return ctx.update(data) + ctx.finalize()


class XtsReader:
    """Reads a region, transparently decrypting XTS sectors past the plaintext header."""

    def __init__(self, base, key: bytes, start_sector: int, name: str = ""):
        self.base = base
        self.key = key
        self.start_sector = start_sector
        self.size = len(base)
        self.name = name

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0 or offset + length > self.size:
            raise PkgError(
                f"{self.name}: need {length} bytes at 0x{offset:X}, "
                f"only 0x{self.size:X} available"
            )
        if length == 0:
            return b""
        first, last = offset // XTS_SECTOR, (offset + length - 1) // XTS_SECTOR
        out = bytearray()
        for sector in range(first, last + 1):
            start = sector * XTS_SECTOR
            available = min(XTS_SECTOR, self.size - start)
            if available <= 0:
                break
            chunk = self.base.read_at(start, available)
            if sector >= self.start_sector:
                pad = (-len(chunk)) % 16
                plain = xts_sector(chunk + b"\x00" * pad, self.key, sector, False)
                chunk = plain[: len(chunk)]
            out += chunk
        begin = offset - first * XTS_SECTOR
        return bytes(out[begin : begin + length])

    def __len__(self) -> int:
        return self.size


# ==========================================================================
# byte access
# ==========================================================================


class Reader:
    """Positional, bounds-checked reader over a seekable stream window."""

    def __init__(self, stream, base: int = 0, size: int | None = None, name: str = ""):
        self.stream = stream
        self.base = base
        self.name = name
        if size is None:
            stream.seek(0, os.SEEK_END)
            size = stream.tell() - base
        self.size = size

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0 or offset + length > self.size:
            raise PkgError(
                f"{self.name or 'input'}: need {length} bytes at 0x{offset:X}, "
                f"only 0x{self.size:X} available"
            )
        self.stream.seek(self.base + offset)
        data = self.stream.read(length)
        if len(data) != length:
            raise PkgError(f"{self.name or 'input'}: short read at 0x{offset:X}")
        return data

    def window(self, offset: int, size: int, name: str = "") -> "Reader":
        if offset < 0 or size < 0 or offset + size > self.size:
            raise PkgError(f"{self.name}: window 0x{offset:X}+0x{size:X} is out of range")
        return Reader(self.stream, self.base + offset, size, name or self.name)

    def __len__(self) -> int:
        return self.size


def u16(d, o=0, endian="<"):
    return struct.unpack_from(endian + "H", d, o)[0]


def u32(d, o=0, endian="<"):
    return struct.unpack_from(endian + "I", d, o)[0]


def u64(d, o=0, endian="<"):
    return struct.unpack_from(endian + "Q", d, o)[0]


def i32(d, o=0):
    return struct.unpack_from("<i", d, o)[0]


def i64(d, o=0):
    return struct.unpack_from("<q", d, o)[0]


def ascii_z(d: bytes, o: int, n: int) -> str:
    raw = d[o : o + n]
    nul = raw.find(b"\x00")
    return raw[: nul if nul >= 0 else len(raw)].decode("ascii", "replace")


# ==========================================================================
# package containers
# ==========================================================================

ENTRY_NAMES = {
    0x0001: "digests",
    0x0010: "entry_keys",
    0x0020: "image_key",
    0x0080: "general_digests",
    0x0100: "metas",
    0x0200: "entry_names",
    0x0400: "license.dat",
    0x040A: "imagedigs.dat",
    0x1000: "param.sfo",
    0x1001: "playgo-chunk.dat",
    0x1200: "icon0.png",
    0x2000: "param.json",
}
CONTENT_TYPES = {0x1A: "GD (game data)", 0x1B: "AC", 0x1C: "AL", 0x1E: "DP (delta patch)"}


class Entry:
    def __init__(self, raw: bytes):
        self.raw = raw
        self.id = u32(raw, 0, ">")
        self.name_offset = u32(raw, 4, ">")
        self.flags1 = u32(raw, 8, ">")
        self.flags2 = u32(raw, 12, ">")
        self.data_offset = u32(raw, 16, ">")
        self.data_size = u32(raw, 20, ">")

    @property
    def encrypted(self) -> bool:
        return bool(self.flags1 & 0x80000000)

    @property
    def key_index(self) -> int:
        return (self.flags2 & 0xF000) >> 12


class Ps4Header:
    def __init__(self, d: bytes):
        if d[:4] != CNT_MAGIC:
            raise PkgError("not a PS4 package (bad CNT magic)")
        self.entry_count = u32(d, 0x10, ">")
        self.entry_table_offset = u32(d, 0x18, ">")
        self.content_id = ascii_z(d, 0x40, 0x30)
        self.drm_type = u32(d, 0x70, ">")
        self.content_type = u32(d, 0x74, ">")
        self.version_date = u32(d, 0x80, ">")
        self.pfs_flags = u64(d, 0x408, ">")
        self.pfs_offset = u64(d, 0x410, ">")
        self.pfs_size = u64(d, 0x418, ">")
        self.package_size = u64(d, 0x430, ">")


class Ps5Header:
    def __init__(self, d: bytes):
        if d[:4] != FIH_MAGIC:
            raise PkgError("not a PS5 package (bad FIH magic)")
        self.signed_flag = d[5]
        self.pfs_offset = u64(d, 0x10)
        self.pfs_size = u64(d, 0x18)
        self.cnt_offset = u64(d, 0x58)
        self.cnt_size = u64(d, 0xA0)


class Container:
    """A `\\x7FCNT` metadata container: header, entry table, name table."""

    def __init__(self, reader, kind: str):
        self.reader = reader
        self.kind = kind
        self.header = Ps4Header(reader.read_at(0, 0x5A0))
        if self.header.entry_count > 1_000_000:
            raise PkgError(f"implausible entry count {self.header.entry_count}")
        table = reader.read_at(
            self.header.entry_table_offset, self.header.entry_count * 0x20
        )
        self.entries = [Entry(table[i * 0x20 : (i + 1) * 0x20]) for i in range(self.header.entry_count)]
        self._names: list[str] = []
        for entry in self.entries:
            if entry.id == 0x0200:
                blob = reader.read_at(entry.data_offset, entry.data_size)
                self._names = [p.decode("ascii", "replace") for p in blob.split(b"\x00")]
                break

    @property
    def content_id(self) -> str:
        return self.header.content_id

    def name_of(self, entry: Entry) -> str:
        if self._names and 0 < entry.name_offset < len(self._names):
            if self._names[entry.name_offset]:
                return self._names[entry.name_offset]
        return ENTRY_NAMES.get(entry.id, f"0x{entry.id:04X}")

    def find(self, key):
        for entry in self.entries:
            if isinstance(key, int):
                if entry.id == key:
                    return entry
            elif self.name_of(entry) == key:
                return entry
        return None


def decrypt_entry(data: bytes, entry: Entry, content_id: str, passcode: str) -> bytes:
    if len(data) % 16:
        raise PkgError(f"encrypted entry 0x{entry.id:04X} is not AES block aligned")
    seed = compute_keys(content_id, passcode, entry.key_index)
    digest = sha256(entry.raw + seed)
    ctx = Cipher(algorithms.AES(digest[16:32]), modes.CBC(digest[:16])).decryptor()
    return ctx.update(data) + ctx.finalize()


# ==========================================================================
# PFS
# ==========================================================================

DIRENT_FILE, DIRENT_DIR, DIRENT_DOT, DIRENT_DOTDOT = 2, 3, 4, 5
INODE_MODE_DIR, INODE_MODE_FILE = 0x4000, 0x8000
CONTIG = {0xFFFFFFFF, 0xFFFFFFFFFFFFFFFF}


class PfsHeader:
    def __init__(self, d: bytes):
        self.version = u64(d, 0x00)
        self.magic = u64(d, 0x08)
        if self.magic != PFS_MAGIC:
            raise PkgError("bad PFS superblock magic (corrupt image or wrong key)")
        if self.version not in (1, 2):
            raise PkgError(f"unsupported PFS version {self.version}")
        self.mode = u16(d, 0x1C)
        self.block_size = u32(d, 0x20)
        self.nblock = u64(d, 0x28)
        self.inode_count = u64(d, 0x30)
        self.inode_block_count = u64(d, 0x40)
        self.superroot = u64(d, 0x48)
        self.seed = d[0x370:0x380]
        if not 0x1000 <= self.block_size <= 0x100000:
            raise PkgError(f"implausible PFS block size 0x{self.block_size:X}")
        if self.inode_count > 50_000_000 or self.inode_block_count > (1 << 20):
            raise PkgError("implausible PFS inode geometry")

    @property
    def signed(self) -> bool:
        return bool(self.mode & 0x1)

    @property
    def is64(self) -> bool:
        return bool(self.mode & 0x2)

    @property
    def encrypted(self) -> bool:
        return bool(self.mode & 0x4)

    @property
    def inode_size(self) -> int:
        if not self.signed:
            return 0xA8
        return 0x310 if self.is64 else 0x2C8


class Inode:
    def __init__(self, d: bytes, number: int, h: PfsHeader):
        self.number = number
        self.mode = u16(d, 0x00)
        self.flags = u32(d, 0x04)
        self.size = i64(d, 0x08)
        self.size_compressed = i64(d, 0x10)
        self.blocks = u32(d, 0x60)
        self.direct: list[int] = []
        self.indirect: list[int] = []
        if not h.signed:
            for i in range(12):
                self.direct.append(u32(d, 0x64 + i * 4))
            for i in range(5):
                self.indirect.append(u32(d, 0x94 + i * 4))
        elif h.is64:
            for i in range(12):
                self.direct.append(u64(d, 0x68 + i * 40 + 32))
            for i in range(5):
                self.indirect.append(u64(d, 0x68 + 12 * 40 + i * 40 + 32))
        else:
            for i in range(12):
                self.direct.append(u32(d, 0x64 + i * 36 + 32))
            for i in range(5):
                self.indirect.append(u32(d, 0x64 + 12 * 36 + i * 36 + 32))

    @property
    def is_dir(self) -> bool:
        return bool(self.mode & INODE_MODE_DIR)

    @property
    def is_file(self) -> bool:
        return bool(self.mode & INODE_MODE_FILE)

    @property
    def is_compressed(self) -> bool:
        return bool(self.flags & 0x1) or bool(
            self.size_compressed and self.size_compressed != self.size
        )


class Pfs:
    def __init__(self, reader, ekpfs: bytes | None = None, new_crypt: bool = False):
        self.name = getattr(reader, "name", "pfs")
        self.header = PfsHeader(reader.read_at(0, 0x380))
        if self.header.encrypted:
            if ekpfs is None:
                raise KeyNeeded("PFS image is encrypted and no key was supplied")
            reader = XtsReader(
                reader,
                pfs_xts_key(ekpfs, self.header.seed, new_crypt),
                self.header.block_size // XTS_SECTOR,
                self.name,
            )
        self.reader = reader
        self.inodes = self._load_inodes()
        plausible = sum(1 for i in self.inodes if i.is_dir or i.is_file)
        if plausible * 2 < len(self.inodes):
            raise PkgError("inode table is not a filesystem (wrong key?)")
        self.root = self._resolve_root()

    def _load_inodes(self) -> list[Inode]:
        h = self.header
        per_block = h.block_size // h.inode_size
        out: list[Inode] = []
        for block in range(h.inode_block_count):
            base = h.block_size * (1 + block)
            if base + h.block_size > len(self.reader):
                raise PkgError("inode table runs past the end of the image")
            chunk = self.reader.read_at(base, h.block_size)
            for j in range(per_block):
                if len(out) >= h.inode_count:
                    return out
                off = j * h.inode_size
                out.append(Inode(chunk[off : off + h.inode_size], len(out), h))
        return out

    def inode(self, n: int) -> Inode:
        if n < 0 or n >= len(self.inodes):
            raise PkgError(f"inode {n} out of range")
        return self.inodes[n]

    def block_map(self, inode: Inode) -> list[int]:
        h = self.header
        count = inode.blocks
        if count == 0:
            return []
        if count == 1:
            return [inode.direct[0]]
        if inode.direct[1] in CONTIG:
            return list(range(inode.direct[0], inode.direct[0] + count))
        blocks = list(inode.direct[:count])
        if h.signed:
            entry_size = 40 if h.is64 else 36
            value_size = 8 if h.is64 else 4
        else:
            entry_size, value_size = 4, 4
        per_block = h.block_size // entry_size

        def chain(block_number: int) -> list[int]:
            data = self.reader.read_at(block_number * h.block_size, h.block_size)
            out = []
            for i in range(per_block):
                off = i * entry_size + (32 if h.signed else 0)
                value = int.from_bytes(data[off : off + value_size], "little")
                if value == 0:
                    break
                out.append(value)
            return out

        if len(blocks) < count and inode.indirect[0]:
            blocks.extend(chain(inode.indirect[0]))
        if len(blocks) < count and inode.indirect[1]:
            for sub in chain(inode.indirect[1]):
                if len(blocks) >= count:
                    break
                blocks.extend(chain(sub))
        if len(blocks) < count:
            raise PkgError(f"inode {inode.number} block map is incomplete")
        return blocks[:count]

    def listdir(self, inode: Inode) -> list[tuple[int, int, str]]:
        h = self.header
        span = inode.blocks * h.block_size or h.block_size
        data = bytearray()
        for block in self.block_map(inode):
            if len(data) >= span:
                break
            data += self.reader.read_at(block * h.block_size, h.block_size)
        out = []
        offset = 0
        while offset + 16 <= len(data):
            ino = u32(data, offset)
            type_code = i32(data, offset + 4)
            name_length = i32(data, offset + 8)
            entry_size = i32(data, offset + 12)
            if entry_size == 0:
                break
            if entry_size < 16 or name_length < 0 or entry_size > len(data) - offset:
                raise PkgError(f"malformed dirent in inode {inode.number}")
            name = data[offset + 16 : offset + 16 + min(name_length, entry_size - 16)]
            out.append((ino, type_code, name.decode("ascii", "replace")))
            offset += entry_size
        return out

    def _resolve_root(self) -> Inode:
        superroot = self.inode(self.header.superroot if self.header.superroot < len(self.inodes) else 0)
        entries = self.listdir(superroot)
        for ino, type_code, name in entries:
            if type_code == DIRENT_DIR and name == "uroot":
                return self.inode(ino)
        if entries:
            return superroot
        raise PkgError("PFS root directory is empty")

    def resolve(self, path: str) -> Inode | None:
        parts = [p for p in path.replace("\\", "/").split("/") if p not in ("", ".")]
        if parts and parts[0] == "uroot":
            parts = parts[1:]
        inode = self.root
        for part in parts:
            if not inode.is_dir:
                return None
            match = None
            for ino, type_code, name in self.listdir(inode):
                if type_code in (DIRENT_DOT, DIRENT_DOTDOT):
                    continue
                if name == part:
                    match = ino
                    break
            if match is None:
                return None
            inode = self.inode(match)
        return inode

    def walk(self, inode: Inode | None = None, prefix: str = "", seen=None):
        seen = set() if seen is None else seen
        inode = inode or self.root
        for ino, type_code, name in self.listdir(inode):
            if type_code in (DIRENT_DOT, DIRENT_DOTDOT):
                continue
            path = f"{prefix}/{name}" if prefix else name
            child = self.inode(ino)
            yield path, child
            if child.is_dir and child.number not in seen:
                seen.add(child.number)
                yield from self.walk(child, path, seen)

    def read_range(self, inode: Inode, offset: int, length: int) -> bytes:
        """Read a byte range of a file without materialising the whole thing."""
        if inode.is_dir:
            raise PkgError(f"inode {inode.number} is a directory")
        if inode.is_compressed:
            raise PkgError(
                f"{inode.number}: file is PFSC/compressed - not supported by pkg2elf.py"
            )
        if offset >= inode.size:
            return b""
        length = min(length, inode.size - offset)
        blocks = self.block_map(inode)
        if not blocks:
            return b"\x00" * length
        bs = self.header.block_size
        out = bytearray()
        skip = offset % bs
        remaining = length
        for index in range(offset // bs, len(blocks)):
            if remaining <= 0:
                break
            take = min(bs - skip, remaining)
            out += self.reader.read_at(blocks[index] * bs + skip, take)
            remaining -= take
            skip = 0
        if remaining > 0:
            out += b"\x00" * remaining
        return bytes(out)

    def read_file(self, inode: Inode) -> bytes:
        return self.read_range(inode, 0, inode.size)


def open_pfs(region, content_id: str, passcode: str | None, ekpfs: str | None,
             new_crypt: bool | None) -> Pfs:
    """Try every key/flag combination we have, then say what was wrong."""
    if ekpfs:
        text = ekpfs[2:] if ekpfs.startswith("0x") else ekpfs
        try:
            keys: list[bytes | None] = [bytes.fromhex(text.strip())]
        except ValueError as exc:
            raise PkgError("--ekpfs must be 64 hex characters") from exc
        if len(keys[0]) != 32:
            raise PkgError("--ekpfs must be 32 bytes (64 hex characters)")
    elif passcode:
        keys = [compute_keys(content_id, passcode, 1), compute_keys_sha3(content_id, passcode, 1)]
    else:
        keys = [None]

    flags = [new_crypt] if new_crypt is not None else [False, True]
    last: Exception | None = None
    for key in keys:
        for flag in flags:
            try:
                return Pfs(region, ekpfs=key, new_crypt=bool(flag))
            except KeyNeeded:
                raise
            except Exception as exc:  # bad key or corrupt image
                last = exc
    if all(k is None for k in keys):
        raise KeyNeeded("the PFS image is encrypted; pass --passcode ...")
    raise PkgError(f"no supplied key could read the PFS image ({last})")


# ==========================================================================
# SELF -> ELF
# ==========================================================================


def rebuild_elf(data: bytes) -> tuple[bytes, list[str]]:
    """Return `(elf_bytes, warnings)` for an ELF or SELF input."""
    if data[:4] == ELF_MAGIC:
        return data, []

    magic = u32(data, 0)
    if magic not in SELF_MAGICS:
        raise PkgError("input is neither an ELF nor a SELF container")
    entry_count = u16(data, 0x18)
    if entry_count > 4096:
        raise PkgError(f"implausible SELF segment count {entry_count}")

    segments = []
    for i in range(entry_count):
        base = 0x20 + i * 0x20
        props, offset, file_size, mem_size = struct.unpack_from("<QQQQ", data, base)
        segments.append((props, offset, file_size, mem_size))

    blocked = [
        i for i, (props, _, _, _) in enumerate(segments) if props & 0x2 or props & 0x8
    ]
    if blocked:
        raise PkgError(
            "SELF segments "
            + ", ".join(f"#{i}" for i in blocked)
            + " are encrypted or compressed; rebuilding a plain ELF needs "
            "console-side keys, which this script does not include"
        )

    search_from = 0x20 + entry_count * 0x20
    elf_offset = data.find(ELF_MAGIC, search_from, min(len(data), search_from + 0x1000))
    if elf_offset < 0:
        elf_offset = data.find(ELF_MAGIC, search_from)
    if elf_offset < 0:
        raise PkgError("SELF container has no embedded ELF header")

    e_type, e_machine = u16(data, elf_offset + 0x10), u16(data, elf_offset + 0x12)
    e_entry = u64(data, elf_offset + 0x18)
    e_phoff = u64(data, elf_offset + 0x20)
    e_shoff = u64(data, elf_offset + 0x28)
    e_phentsize = u16(data, elf_offset + 0x36)
    e_phnum = u16(data, elf_offset + 0x38)
    if e_machine != 0x3E and e_type < 0xFE00:
        raise PkgError("embedded ELF is not an x86-64 SCE module")

    phdr_size = e_phentsize or 56
    phdrs = []
    for i in range(e_phnum):
        base = elf_offset + e_phoff + i * phdr_size
        p_type = u32(data, base)
        p_offset = u64(data, base + 0x08)
        p_filesz = u64(data, base + 0x20)
        phdrs.append((p_type, p_offset, p_filesz))

    phdr_bytes = e_phnum * phdr_size
    total = elf_offset + e_phoff + phdr_bytes
    for _, p_offset, p_filesz in phdrs:
        if p_filesz:
            total = max(total, p_offset + p_filesz)
    out = bytearray(total)
    out[0:0x40] = data[elf_offset : elf_offset + 0x40]
    phdr_src = elf_offset + e_phoff
    out[e_phoff : e_phoff + phdr_bytes] = data[phdr_src : phdr_src + phdr_bytes]

    unused = [i for i, (_, _, file_size, _) in enumerate(segments) if file_size]
    warnings: list[str] = []
    copied = 0
    for p_type, p_offset, p_filesz in phdrs:
        if not p_filesz:
            continue
        match = next((i for i in unused if segments[i][2] == p_filesz), None)
        if match is None:
            warnings.append(f"phdr type 0x{p_type:X} has no matching SELF segment")
            continue
        unused.remove(match)
        _, seg_offset, seg_size, _ = segments[match]
        out[p_offset : p_offset + seg_size] = data[seg_offset : seg_offset + seg_size]
        copied += 1

    if unused:
        warnings.append(f"{len(unused)} SELF segment(s) were unused")
    if e_shoff:
        out[0x28:0x30] = b"\x00" * 8
        out[0x3C:0x3E] = b"\x00" * 2
        warnings.append("section header table dropped (absent from SELF payloads)")
    if not copied:
        raise PkgError("no SELF payload could be mapped onto the ELF program headers")
    if e_entry == 0:
        warnings.append("entry point is zero")
    return bytes(out), warnings


# ==========================================================================
# driver
# ==========================================================================

ELF_CANDIDATES = ["eboot.bin", "sce_sys/about/right.sprx", "sce_sys/eboot.bin"]


def looks_like_module(pfs: Pfs, inode: Inode) -> bool:
    if inode.size < 4:
        return False
    try:
        first = pfs.read_file(inode)[:4]
    except PkgError:
        return False
    return first == ELF_MAGIC or u32(first, 0) in SELF_MAGICS


def find_executable(pfs: Pfs, wanted: str | None):
    if wanted:
        inode = pfs.resolve(wanted)
        if inode is None or inode.is_dir:
            raise PkgError(f"{wanted} was not found in the package filesystem")
        return wanted, inode
    for name in ELF_CANDIDATES:
        inode = pfs.resolve(name)
        if inode is not None and inode.is_file:
            return name, inode
    for path, inode in pfs.walk():
        if inode.is_file and looks_like_module(pfs, inode):
            return path, inode
    raise PkgError("no executable was found in the package; try --list or --path")


def load_package(path: str, passcode, ekpfs, new_crypt):
    stream = open(path, "rb")
    reader = Reader(stream, name=os.path.basename(path))
    magic = reader.read_at(0, 4)

    if magic == CNT_MAGIC:
        container = Container(reader, "PS4")
        region = reader.window(container.header.pfs_offset, container.header.pfs_size, "ps4-pfs")
        inspect = {"format": "PS4", "container": container, "pfs_region": region,
                   "new_crypt": bool(container.header.pfs_flags & 0x2000000000000000)}
    elif magic == FIH_MAGIC:
        fih = Ps5Header(reader.read_at(0, 0x100))
        container = Container(reader.window(fih.cnt_offset, fih.cnt_size, "ps5-cnt"), "PS5")
        inspect = {
            "format": "PS5",
            "container": container,
            "fih": fih,
            "pfs_region": reader.window(fih.pfs_offset, fih.pfs_size, "ps5-outer-pfs"),
            "new_crypt": False,
        }
    else:
        raise PkgError(f"{path} is not a PS4 or PS5 package")

    # PS4: the PFS image is the content. PS5: the outer PFS holds pfs_image.dat.
    forced = new_crypt or None
    if container.kind == "PS5":
        # The application filesystem is pfs_image.dat inside the outer PFS.
        outer = open_pfs(inspect["pfs_region"], container.content_id, passcode, ekpfs, forced)
        node = outer.resolve("pfs_image.dat")
        if node is None or not node.is_file:
            raise PkgError("outer PS5 image has no pfs_image.dat (compressed inner image?)")
        # Read the inner image through the outer one - never load it into RAM.
        pfs = open_pfs(PfsFileView(outer, node), container.content_id, passcode,
                       ekpfs, forced)
    else:
        pfs = open_pfs(inspect["pfs_region"], container.content_id, passcode, ekpfs,
                       inspect["new_crypt"] or forced)
    return container, pfs, inspect


class PfsFileView:
    """A random-access view over one PFS file, used for nested PS5 images.

    Implements the same ``read_at`` interface as Reader, so an inner PFS can be
    parsed straight out of the outer image without copying it to memory or disk.
    """

    def __init__(self, pfs: Pfs, inode: Inode):
        self.pfs = pfs
        self.inode = inode
        self.size = inode.size
        self.name = "inner-pfs"
        if inode.is_compressed:
            raise PkgError("inner PFS image is PFSC/compressed - not supported yet")

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0 or offset + length > self.size:
            raise PkgError(
                f"inner-pfs: need {length} bytes at 0x{offset:X}, "
                f"only 0x{self.size:X} available"
            )
        if length == 0:
            return b""
        return self.pfs.read_range(self.inode, offset, length)

    def __len__(self) -> int:
        return self.size


def cmd_info(container: Container, inspect: dict, path: str) -> None:
    header = container.header
    print(f"file            {path}")
    print(f"format          {container.kind}")
    print(f"content id      {container.content_id}")
    print(f"content type    {CONTENT_TYPES.get(header.content_type, hex(header.content_type))}")
    print(f"version date    0x{header.version_date:08X}")
    print(f"entries         {len(container.entries)}")
    if "fih" in inspect:
        fih = inspect["fih"]
        print(f"variant         {'retail' if fih.signed_flag & 0x80 else 'debug'}")
        print(f"outer pfs       {fih.pfs_offset} ({fih.pfs_size} bytes)")
    else:
        print(f"pfs image       {header.pfs_offset} ({header.pfs_size} bytes)")


def cmd_list(container: Container, pfs: Pfs) -> None:
    for entry in container.entries:
        print(f"  0x{entry.id:04X}  {entry.data_size:>10}  {container.name_of(entry)}")
    print("---- package filesystem ----")
    for relpath, inode in pfs.walk():
        kind = "dir " if inode.is_dir else "file"
        print(f"  {kind} {inode.size:>12}  {relpath}")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Convert the executable inside a PS4/PS5 package to a standard ELF."
    )
    parser.add_argument("input", help="package file, or a standalone ELF/SELF file")
    parser.add_argument("-o", "--output", help="output ELF path (default: <name>.elf)")
    parser.add_argument("--passcode", help="32-character FPKG passcode")
    parser.add_argument("--ekpfs", help="explicit 32-byte EKPFS key, 64 hex characters")
    parser.add_argument("--new-crypt", action="store_true", help="force new key schedule")
    parser.add_argument("--path", help="file inside the package to convert")
    parser.add_argument("--info", action="store_true", help="print package info and stop")
    parser.add_argument("--list", action="store_true", help="list entries and files, then stop")
    args = parser.parse_args(argv)

    try:
        with open(args.input, "rb") as probe:
            magic = probe.read(4)

        if magic not in (CNT_MAGIC, FIH_MAGIC):
            # Standalone module.
            data = open(args.input, "rb").read()
            elf, warnings = rebuild_elf(data)
            out_path = args.output or os.path.splitext(args.input)[0] + ".elf"
            with open(out_path, "wb") as handle:
                handle.write(elf)
            print(f"converted {args.input} -> {out_path} ({len(elf)} bytes)")
            for warning in warnings:
                print(f"warning: {warning}")
            return 0

        container, pfs, inspect = load_package(
            args.input, args.passcode, args.ekpfs, args.new_crypt or None
        )
        if args.info:
            cmd_info(container, inspect, args.input)
            return 0
        if args.list:
            cmd_list(container, pfs)
            return 0

        name, inode = find_executable(pfs, args.path)
        source = pfs.read_file(inode)
        elf, warnings = rebuild_elf(source)
        out_path = args.output or os.path.splitext(os.path.basename(name))[0] + ".elf"
        with open(out_path, "wb") as handle:
            handle.write(elf)
        print(f"source          {name} ({inode.size} bytes)")
        print(f"output          {out_path} ({len(elf)} bytes)")
        print(f"type            0x{u16(elf, 0x10):04X} / machine 0x{u16(elf, 0x12):02X}")
        print(f"entry point     0x{u64(elf, 0x18):X}")
        for warning in warnings:
            print(f"warning: {warning}")
        return 0

    except KeyNeeded as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 3
    except PkgError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except (OSError, struct.error) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
