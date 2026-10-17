"""PlayStation File System (PFS) reader.

PFS is little-endian and uses 64 KiB blocks by default. Three inode layouts
exist and all are handled here:

===========  ======  ==========================================
mode         size    layout
===========  ======  ==========================================
unsigned     0xA8    100-byte header + 17 x u32 block pointers
signed-32    0x2C8    100-byte header + 17 x (32-byte sig + u32)
signed-64    0x310    104-byte header + 17 x (32-byte sig + u64)
===========  ======  ==========================================

The signed inode header is shorter than the unsigned one because ``blocks``
ends the common header at offset 0x64; the unsigned layout simply keeps the
same 100 bytes and follows it with plain pointers.

Files whose inode has flag 0x1 set (or ``size != size_compressed``) store a
PFSC block-compressed stream instead of raw bytes; :class:`PfscReader`
decodes it one block at a time.
"""

from __future__ import annotations

import io
import zlib
from dataclasses import dataclass, field
from typing import Iterator

from .binary import (
    Reader,
    WindowReader,
    i32le,
    i64le,
    u16le,
    u32le,
    u64le,
)
from .crypto import XTS_SECTOR_SIZE, XtsDecryptReader, pfs_gen_xts_key
from .errors import CorruptImage, KeyRequired, PkgMasterError, TruncatedFile

PFS_MAGIC = 20130315
SUPERBLOCK_READ = 0x400
SEED_OFFSET = 0x370
HEADER_SIZE = 0x380

MODE_SIGNED = 0x1
MODE_64BIT = 0x2
MODE_ENCRYPTED = 0x4
MODE_CASE_INSENSITIVE = 0x8

INODE_MODE_DIR = 0x4000
INODE_MODE_FILE = 0x8000
INODE_FLAG_COMPRESSED = 0x1

DIRENT_FILE = 2
DIRENT_DIR = 3
DIRENT_DOT = 4
DIRENT_DOTDOT = 5

CONTIGUOUS_MARKERS = frozenset({0xFFFFFFFF, 0xFFFFFFFFFFFFFFFF})

PFSC_MAGIC = b"PFSC"
PFSC_HEADER_SIZE = 0x30
PFSC_MAX_BLOCK = 0x100000

MAX_INODE_BLOCKS = 1 << 20
MAX_BLOCK_INDEX = 1 << 26


@dataclass
class PfsHeader:
    version: int
    magic: int
    image_id: int
    fmode: int
    clean: int
    readonly: int
    mode: int
    block_size: int
    nbackup: int
    nblock: int
    inode_count: int
    data_block_count: int
    inode_block_count: int
    superroot_inode: int
    seed: bytes

    @property
    def signed(self) -> bool:
        return bool(self.mode & MODE_SIGNED)

    @property
    def is_64bit(self) -> bool:
        return bool(self.mode & MODE_64BIT)

    @property
    def encrypted(self) -> bool:
        return bool(self.mode & MODE_ENCRYPTED)

    @property
    def inode_size(self) -> int:
        if not self.signed:
            return 0xA8
        return 0x310 if self.is_64bit else 0x2C8

    def to_dict(self) -> dict:
        return {
            "version": self.version,
            "format_magic": self.magic,
            "mode": f"0x{self.mode:X}",
            "signed": self.signed,
            "64bit_inodes": self.is_64bit,
            "encrypted": self.encrypted,
            "block_size": self.block_size,
            "block_count": self.nblock,
            "inode_count": self.inode_count,
            "inode_block_count": self.inode_block_count,
            "data_block_count": self.data_block_count,
            "superroot_inode": self.superroot_inode,
            "inode_size": self.inode_size,
            "seed": self.seed.hex(),
        }


def parse_header(data: bytes) -> PfsHeader:
    if len(data) < HEADER_SIZE:
        raise TruncatedFile(f"PFS superblock needs 0x{HEADER_SIZE:X} bytes")
    version = u64le(data, 0x00)
    magic = u64le(data, 0x08)
    if magic != PFS_MAGIC:
        raise CorruptImage(
            f"PFS superblock magic is {magic} (expected {PFS_MAGIC}); "
            "the image is not a plaintext PFS or the key is wrong"
        )
    if version not in (1, 2):
        raise CorruptImage(f"unsupported PFS version {version}")
    return PfsHeader(
        version=version,
        magic=magic,
        image_id=u64le(data, 0x10),
        fmode=data[0x18],
        clean=data[0x19],
        readonly=data[0x1A],
        mode=u16le(data, 0x1C),
        block_size=u32le(data, 0x20),
        nbackup=u32le(data, 0x24),
        nblock=u64le(data, 0x28),
        inode_count=u64le(data, 0x30),
        data_block_count=u64le(data, 0x38),
        inode_block_count=u64le(data, 0x40),
        superroot_inode=u64le(data, 0x48),
        seed=data[SEED_OFFSET : SEED_OFFSET + 16],
    )


def peek_header(reader: Reader) -> PfsHeader:
    """Read the plaintext superblock without decrypting anything."""
    return parse_header(reader.read_at(0, HEADER_SIZE))


@dataclass
class Inode:
    number: int
    mode: int
    nlink: int
    flags: int
    size: int
    size_compressed: int
    blocks: int
    direct: list[int]
    indirect: list[int]
    uid: int = 0
    gid: int = 0
    time_sec: int = 0

    @property
    def is_dir(self) -> bool:
        return bool(self.mode & INODE_MODE_DIR)

    @property
    def is_file(self) -> bool:
        return bool(self.mode & INODE_MODE_FILE)

    @property
    def is_compressed(self) -> bool:
        if self.flags & INODE_FLAG_COMPRESSED:
            return True
        return bool(self.size_compressed and self.size_compressed != self.size)

    @property
    def label(self) -> str:
        if self.is_dir:
            return "dir"
        if self.is_file:
            return "file"
        return "special"


def parse_inode(data: bytes, number: int, header: PfsHeader) -> Inode:
    if len(data) < header.inode_size:
        raise CorruptImage(f"inode {number} record is short")
    mode = u16le(data, 0x00)
    nlink = u16le(data, 0x02)
    flags = u32le(data, 0x04)
    size = i64le(data, 0x08)
    size_compressed = i64le(data, 0x10)
    time_sec = i64le(data, 0x18)
    uid = u32le(data, 0x48)
    gid = u32le(data, 0x4C)
    blocks = u32le(data, 0x60)

    direct: list[int] = []
    indirect: list[int] = []
    if not header.signed:
        for i in range(12):
            direct.append(u32le(data, 0x64 + i * 4))
        for i in range(5):
            indirect.append(u32le(data, 0x94 + i * 4))
    elif header.is_64bit:
        base = 0x68
        for i in range(12):
            direct.append(u64le(data, base + i * 40 + 32))
        for i in range(5):
            indirect.append(u64le(data, base + 12 * 40 + i * 40 + 32))
    else:
        base = 0x64
        for i in range(12):
            direct.append(u32le(data, base + i * 36 + 32))
        for i in range(5):
            indirect.append(u32le(data, base + 12 * 36 + i * 36 + 32))

    return Inode(
        number=number,
        mode=mode,
        nlink=nlink,
        flags=flags,
        size=size,
        size_compressed=size_compressed,
        blocks=blocks,
        direct=direct,
        indirect=indirect,
        uid=uid,
        gid=gid,
        time_sec=time_sec,
    )


@dataclass
class Dirent:
    inode_number: int
    type_code: int
    name: str

    @property
    def is_dir(self) -> bool:
        return self.type_code == DIRENT_DIR

    @property
    def is_file(self) -> bool:
        return self.type_code == DIRENT_FILE


@dataclass
class FileHandle:
    """A readable PFS file, usable as a ``read_at`` source for nested images."""

    name: str
    path: str
    inode: Inode
    header: PfsHeader
    image: "PfsImage" = field(repr=False)

    @property
    def size(self) -> int:
        return self.inode.size

    def read_at(self, offset: int, length: int) -> bytes:
        """Read file bytes, clamping at EOF like a normal file handle."""
        if offset < 0 or length < 0:
            raise TruncatedFile(f"{self.path}: negative read ({offset}, {length})")
        if offset >= self.size:
            return b""
        length = min(length, self.size - offset)
        return self.image.read_file_range(self.inode, offset, length)

    def slice(self, offset: int, size: int, name: str = "") -> WindowReader:
        return WindowReader(self, offset, size, name or self.path)

    def __len__(self) -> int:
        return self.size


class StoredData:
    """Strict ``read_at`` over the bytes an inode stores on disk.

    For a compressed inode this is the PFSC stream (``size_compressed`` bytes),
    not the decoded file. Unlike :class:`FileHandle` it never clamps or pads:
    a read past the stored length, or past the end of the block map, is
    corruption.
    """

    def __init__(self, image: "PfsImage", inode: Inode, size: int) -> None:
        self.image = image
        self.size = size
        self.name = f"inode {inode.number}"
        self.blocks = image.block_map(inode)
        if len(self.blocks) * image.header.block_size < size:
            raise CorruptImage(
                f"inode {inode.number}: stores {size} bytes but maps only "
                f"{len(self.blocks)} block(s)"
            )

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0 or offset + length > self.size:
            raise CorruptImage(
                f"{self.name}: need {length} bytes at 0x{offset:X} of a "
                f"0x{self.size:X}-byte stored stream"
            )
        block_size = self.image.header.block_size
        out = bytearray()
        while len(out) < length:
            index, skip = divmod(offset + len(out), block_size)
            take = min(block_size - skip, length - len(out))
            out += self.image.reader.read_at(self.blocks[index] * block_size + skip, take)
        return bytes(out)

    def __len__(self) -> int:
        return self.size


class PfscReader:
    """Random access to a PFSC block-compressed file.

    Layout (little-endian): magic ``PFSC`` @0x00, block size u32 @0x0C and
    again as u64 @0x10, block-offset table offset u64 @0x18, data start u64
    @0x20, decoded length u64 @0x28. The table holds ``nblocks + 1`` u64
    offsets into the stream; block ``i`` occupies ``[t[i], t[i+1])``. A block
    whose stored size equals the block size is raw, anything shorter is a
    zlib (or raw deflate) stream. Every inconsistency raises
    :class:`CorruptImage`; nothing is padded or guessed.

    Only the two table entries a block needs are read, and one decoded block
    is cached, so memory use is bounded by the block size.
    """

    def __init__(self, stored, size: int, name: str = "") -> None:
        self.stored = stored
        self.size = size
        self.name = name or getattr(stored, "name", "pfsc")
        stored_len = len(stored)
        if stored_len < PFSC_HEADER_SIZE:
            raise CorruptImage(f"{self.name}: PFSC header is truncated")
        header = stored.read_at(0, PFSC_HEADER_SIZE)
        if header[:4] != PFSC_MAGIC:
            raise CorruptImage(
                f"{self.name}: compressed file has no PFSC magic (got {header[:4]!r})"
            )
        block_size = u32le(header, 0x0C)
        if block_size == 0 or block_size > PFSC_MAX_BLOCK or u64le(header, 0x10) != block_size:
            raise CorruptImage(f"{self.name}: implausible PFSC block size 0x{block_size:X}")
        table = u64le(header, 0x18)
        data_start = u64le(header, 0x20)
        data_length = u64le(header, 0x28)
        if data_length != size:
            raise CorruptImage(
                f"{self.name}: PFSC header says {data_length} bytes, inode says {size}"
            )
        self.block_size = block_size
        self.count = -(-size // block_size)
        table_end = table + (self.count + 1) * 8
        if table < PFSC_HEADER_SIZE or table_end > stored_len:
            raise CorruptImage(
                f"{self.name}: PFSC block table (0x{table:X}..0x{table_end:X}) runs "
                f"past the 0x{stored_len:X}-byte stream"
            )
        if data_start < table_end or data_start > stored_len:
            raise CorruptImage(f"{self.name}: PFSC data start 0x{data_start:X} is out of range")
        self.table = table
        self.data_start = data_start
        self._cached: tuple[int, bytes] | None = None

    def _block_extent(self, index: int) -> tuple[int, int]:
        raw = self.stored.read_at(self.table + index * 8, 16)
        start, end = u64le(raw, 0), u64le(raw, 8)
        if not self.data_start <= start < end <= len(self.stored):
            raise CorruptImage(
                f"{self.name}: PFSC block {index} spans 0x{start:X}..0x{end:X}, "
                f"outside the data area 0x{self.data_start:X}..0x{len(self.stored):X}"
            )
        if end - start > self.block_size:
            raise CorruptImage(
                f"{self.name}: PFSC block {index} stores {end - start} bytes, "
                f"more than the 0x{self.block_size:X} block size"
            )
        return start, end

    def block(self, index: int) -> bytes:
        """Decoded bytes of block ``index`` (short only for the last block)."""
        if self._cached is not None and self._cached[0] == index:
            return self._cached[1]
        if not 0 <= index < self.count:
            raise CorruptImage(f"{self.name}: PFSC block {index} is out of range")
        expected = min(self.block_size, self.size - index * self.block_size)
        start, end = self._block_extent(index)
        data = self.stored.read_at(start, end - start)
        if len(data) == self.block_size:
            decoded = data
        else:
            decoded = _inflate(data, self.block_size, f"{self.name}: PFSC block {index}")
        # The final block may be padded out to the full block size; any other
        # length means the stream does not describe this file.
        if len(decoded) != expected and not (
            index == self.count - 1 and len(decoded) == self.block_size
        ):
            raise CorruptImage(
                f"{self.name}: PFSC block {index} decodes to {len(decoded)} bytes "
                f"(expected {expected})"
            )
        decoded = decoded[:expected]
        self._cached = (index, decoded)
        return decoded

    def read_at(self, offset: int, length: int) -> bytes:
        """Read decoded bytes, clamping at EOF like :class:`FileHandle`."""
        if offset < 0 or length < 0:
            raise TruncatedFile(f"{self.name}: negative read ({offset}, {length})")
        length = max(0, min(length, self.size - offset))
        out = bytearray()
        while len(out) < length:
            index, skip = divmod(offset + len(out), self.block_size)
            out += self.block(index)[skip : skip + length - len(out)]
        return bytes(out)

    def __len__(self) -> int:
        return self.size


def _looks_like_zlib(data: bytes) -> bool:
    if len(data) < 2:
        return False
    cmf, flg = data[0], data[1]
    return cmf & 0x0F == 8 and cmf >> 4 <= 7 and not flg & 0x20 and (cmf << 8 | flg) % 31 == 0


def _inflate(data: bytes, limit: int, label: str) -> bytes:
    """Decode one zlib/raw-deflate block of at most ``limit`` bytes, or raise."""
    if _looks_like_zlib(data):
        attempts = (zlib.MAX_WBITS, -zlib.MAX_WBITS)
    else:
        attempts = (-zlib.MAX_WBITS,)
    error = "not a deflate stream"
    for wbits in attempts:
        inflater = zlib.decompressobj(wbits)
        try:
            out = inflater.decompress(data, limit + 1)
        except zlib.error as exc:
            error = str(exc)
            continue
        if len(out) > limit or inflater.unconsumed_tail:
            raise CorruptImage(f"{label} inflates past the 0x{limit:X} block size")
        if not inflater.eof:
            error = "stream is truncated"
            continue
        if inflater.unused_data.strip(b"\x00"):
            error = "trailing bytes after the deflate stream"
            continue
        return out
    raise CorruptImage(f"{label} is not valid compressed data ({error})")


class PfsImage:
    """A parsed PFS image."""

    def __init__(
        self,
        reader,
        *,
        ekpfs: bytes | None = None,
        new_crypt: bool = False,
        name: str = "",
    ) -> None:
        self.name = name or getattr(reader, "name", "pfs")
        self.header = peek_header(_as_reader(reader))
        self._validate_header()

        if self.header.encrypted:
            if ekpfs is None:
                raise KeyRequired(
                    "PFS image is encrypted (mode bit 0x4) and no EKPFS key was supplied"
                )
            xts_key = pfs_gen_xts_key(ekpfs, self.header.seed, new_crypt)
            reader = XtsDecryptReader(
                _as_reader(reader),
                xts_key,
                self.header.block_size // XTS_SECTOR_SIZE,
                XTS_SECTOR_SIZE,
            )

        self.reader = reader
        self.inodes: list[Inode] = []
        self._pfsc: dict[int, PfscReader] = {}
        self._load_inode_table()
        self._validate_inodes()
        self.root = self._resolve_root()

    # -- setup ---------------------------------------------------------
    def _validate_header(self) -> None:
        h = self.header
        if h.block_size < 0x1000 or h.block_size > 0x100000:
            raise CorruptImage(f"implausible PFS block size 0x{h.block_size:X}")
        if h.block_size % XTS_SECTOR_SIZE:
            raise CorruptImage("PFS block size is not a multiple of the XTS sector size")
        if h.inode_count > 50_000_000:
            raise CorruptImage(f"implausible inode count {h.inode_count}")
        if h.inode_block_count > MAX_INODE_BLOCKS:
            raise CorruptImage(f"implausible inode block count {h.inode_block_count}")

    def _load_inode_table(self) -> None:
        h = self.header
        per_block = h.block_size // h.inode_size
        if per_block <= 0:
            raise CorruptImage("PFS block size is smaller than one inode")
        total = 0
        for block in range(h.inode_block_count):
            base = h.block_size * (1 + block)
            if base + h.block_size > len(self.reader):
                raise CorruptImage("inode table extends past the end of the image")
            chunk = self.reader.read_at(base, h.block_size)
            for j in range(per_block):
                if total >= h.inode_count:
                    return
                off = j * h.inode_size
                self.inodes.append(
                    parse_inode(chunk[off : off + h.inode_size], total, h)
                )
                total += 1

    def _validate_inodes(self) -> None:
        """Reject images whose inode table decrypted to noise."""
        if not self.inodes:
            raise CorruptImage("PFS image contains no inodes")
        plausible = sum(1 for i in self.inodes if i.is_dir or i.is_file)
        if plausible * 2 < len(self.inodes):
            raise CorruptImage(
                "inode table does not look like a filesystem "
                "(wrong key, or unsupported inode layout)"
            )

    def _resolve_root(self) -> Inode:
        root = self.superroot()
        try:
            entries = self.listdir(root)
        except PkgMasterError:
            raise
        except Exception as exc:
            raise CorruptImage(f"cannot read the PFS root directory: {exc}") from exc
        for entry in entries:
            if entry.is_dir and entry.name == "uroot":
                return self.inode(entry.inode_number)
        # Some images place the content tree directly in the super root.
        if entries:
            return root
        raise CorruptImage("PFS root directory is empty")

    # -- inode access ---------------------------------------------------
    def inode(self, number: int) -> Inode:
        if number < 0 or number >= len(self.inodes):
            raise CorruptImage(f"inode {number} is out of range (0..{len(self.inodes) - 1})")
        return self.inodes[number]

    def superroot(self) -> Inode:
        number = self.header.superroot_inode
        if number >= len(self.inodes):
            number = 0
        return self.inode(number)

    def block_map(self, inode: Inode) -> list[int]:
        h = self.header
        count = inode.blocks
        if count == 0:
            return []
        if count > MAX_BLOCK_INDEX:
            raise CorruptImage(f"inode {inode.number} declares {count} blocks")
        if count == 1:
            return [inode.direct[0]]
        if inode.direct[1] in CONTIGUOUS_MARKERS:
            start = inode.direct[0]
            return list(range(start, start + count))

        blocks: list[int] = []
        for value in inode.direct:
            if len(blocks) >= count:
                return blocks[:count]
            blocks.append(value)

        if h.signed:
            entry_size = 40 if h.is_64bit else 36
            value_offset = 32
            value_size = 8 if h.is_64bit else 4
        else:
            entry_size = 4
            value_offset = 0
            value_size = 4
        per_block = h.block_size // entry_size

        def read_chain(block_number: int) -> list[int]:
            data = self.reader.read_at(block_number * h.block_size, h.block_size)
            out = []
            for i in range(per_block):
                off = i * entry_size + value_offset
                value = int.from_bytes(data[off : off + value_size], "little")
                if value == 0:
                    break
                out.append(value)
            return out

        if len(blocks) < count and inode.indirect[0]:
            blocks.extend(read_chain(inode.indirect[0]))
        if len(blocks) < count and inode.indirect[1]:
            for sub in read_chain(inode.indirect[1]):
                if len(blocks) >= count:
                    break
                blocks.extend(read_chain(sub))
        if len(blocks) < count:
            raise CorruptImage(
                f"inode {inode.number} block map is incomplete "
                f"({len(blocks)} of {count})"
            )
        return blocks[:count]

    def _inode_bytes(self, inode: Inode, length: int) -> bytes:
        """Read ``length`` bytes of inode content (used for directory blocks)."""
        if length <= 0:
            return b""
        h = self.header
        out = bytearray()
        for block in self.block_map(inode):
            take = min(h.block_size, length - len(out))
            if take <= 0:
                break
            out += self.reader.read_at(block * h.block_size, take)
        return bytes(out)

    # -- directories ----------------------------------------------------
    def listdir(self, inode: Inode) -> list[Dirent]:
        if not inode.is_dir:
            raise CorruptImage(f"inode {inode.number} is not a directory")
        h = self.header
        span = inode.blocks * h.block_size
        if span <= 0:
            # Some images size directories but declare no block count.
            span = h.block_size if inode.size == 0 else inode.size
        data = self._inode_bytes(inode, span)
        entries: list[Dirent] = []
        offset = 0
        while offset + 16 <= len(data):
            inode_number = u32le(data, offset)
            type_code = i32le(data, offset + 4)
            name_length = i32le(data, offset + 8)
            ent_size = i32le(data, offset + 12)
            if ent_size == 0:
                break
            if ent_size < 16 or name_length < 0 or ent_size > len(data) - offset:
                raise CorruptImage(
                    f"directory entry at 0x{offset:X} in inode {inode.number} is malformed"
                )
            name_bytes = data[offset + 16 : offset + 16 + min(name_length, ent_size - 16)]
            entries.append(
                Dirent(
                    inode_number=inode_number,
                    type_code=type_code,
                    name=name_bytes.decode("ascii", "replace"),
                )
            )
            offset += ent_size
        return entries

    def iter_dir(self, inode: Inode) -> Iterator[Dirent]:
        for entry in self.listdir(inode):
            if entry.type_code in (DIRENT_DOT, DIRENT_DOTDOT):
                continue
            yield entry

    # -- lookup ---------------------------------------------------------
    def find(self, path: str) -> FileHandle | None:
        """Resolve a path relative to ``uroot``; returns None when missing."""
        try:
            inode = self.resolve(path)
        except KeyError:
            return None
        if inode is None or inode.is_dir:
            return None
        return FileHandle(
            name=path.rstrip("/").split("/")[-1],
            path=path,
            inode=inode,
            header=self.header,
            image=self,
        )

    def find_any(self, paths: list[str]) -> FileHandle | None:
        for path in paths:
            handle = self.find(path)
            if handle is not None:
                return handle
        return None

    def resolve(self, path: str) -> Inode | None:
        parts = [p for p in path.replace("\\", "/").split("/") if p not in ("", ".")]
        inode = self.root
        if parts and parts[0] == "uroot":
            parts = parts[1:]
        for part in parts:
            if not inode.is_dir:
                raise KeyError(part)
            match = None
            for entry in self.iter_dir(inode):
                if entry.name == part:
                    match = entry
                    break
            if match is None:
                raise KeyError(part)
            inode = self.inode(match.inode_number)
        return inode

    def walk(self, max_depth: int = 64) -> Iterator[tuple[str, Inode]]:
        """Depth-first walk of ``uroot`` yielding ``(relative_path, inode)``."""
        stack: list[tuple[str, Inode, int]] = [("", self.root, 0)]
        seen: set[int] = {self.root.number}
        while stack:
            prefix, inode, depth = stack.pop()
            if depth > max_depth:
                continue
            try:
                entries = list(self.iter_dir(inode))
            except PkgMasterError:
                raise
            except Exception as exc:
                raise CorruptImage(f"cannot list {prefix or 'uroot'}: {exc}") from exc
            dirs: list[tuple[str, Inode, int]] = []
            for entry in entries:
                child = self.inode(entry.inode_number)
                path = f"{prefix}/{entry.name}" if prefix else entry.name
                yield path, child
                if child.is_dir and child.number not in seen:
                    seen.add(child.number)
                    dirs.append((path, child, depth + 1))
            stack.extend(reversed(dirs))

    def all_files(self) -> Iterator[tuple[str, Inode]]:
        for path, inode in self.walk():
            if inode.is_file:
                yield path, inode

    # -- file content ---------------------------------------------------
    def file_reader(self, handle: FileHandle) -> FileHandle:
        return handle

    def compressed_reader(self, inode: Inode) -> PfscReader:
        """The (cached) PFSC decoder for a compressed file inode."""
        reader = self._pfsc.get(inode.number)
        if reader is None:
            if inode.size_compressed <= 0:
                raise CorruptImage(
                    f"inode {inode.number} is flagged compressed but stores no data"
                )
            stored = StoredData(self, inode, inode.size_compressed)
            reader = PfscReader(stored, inode.size, f"inode {inode.number}")
            self._pfsc[inode.number] = reader
        return reader

    def read_file_range(self, inode: Inode, offset: int, length: int) -> bytes:
        h = self.header
        if inode.is_dir:
            raise CorruptImage(f"inode {inode.number} is a directory")
        if offset >= inode.size:
            return b""
        length = min(length, inode.size - offset)
        if inode.is_compressed:
            return self.compressed_reader(inode).read_at(offset, length)
        blocks = self.block_map(inode)
        if not blocks:
            return b"\x00" * length

        out = bytearray()
        first = offset // h.block_size
        skip = offset % h.block_size
        remaining = length
        for index in range(first, len(blocks)):
            if remaining <= 0:
                break
            take = min(h.block_size - skip, remaining)
            out += self.reader.read_at(blocks[index] * h.block_size + skip, take)
            remaining -= take
            skip = 0
        if remaining > 0:
            out += b"\x00" * remaining
        return bytes(out)

    def read_file(self, inode: Inode) -> bytes:
        return self.read_file_range(inode, 0, inode.size)

    def copy_file(
        self,
        inode: Inode,
        out: "io.BufferedWriter",
        *,
        chunk_size: int = 1 << 20,
        progress=None,
    ) -> int:
        written = 0
        while written < inode.size:
            take = min(chunk_size, inode.size - written)
            chunk = self.read_file_range(inode, written, take)
            if not chunk:
                break
            out.write(chunk)
            written += len(chunk)
            if progress is not None:
                progress(written, inode.size)
        return written


def _as_reader(reader):
    """Accept a plain file-like object or any ``read_at`` provider."""
    if hasattr(reader, "read_at"):
        return reader
    if hasattr(reader, "read") and hasattr(reader, "seek"):
        return Reader(reader, name=getattr(reader, "name", ""))
    raise TypeError("PFS source must be a Reader or a seekable stream")
