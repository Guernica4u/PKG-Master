"""PS4 ``\\x7FCNT`` and PS5 ``\\x7FFIH`` package containers.

The two generations share an entry-table model but differ in packaging:

* PS4 - a single ``\\x7FCNT`` header (big-endian) plus an entry table, with the
  PFS image located by ``pfs_image_offset``/``pfs_image_size``.
* PS5 - a little-endian ``\\x7FFIH`` finalized image holding an outer PFS image,
  an embedded ``\\x7FCNT`` metadata container, and a trailing install segment.
  The application filesystem lives inside the outer PFS as ``pfs_image.dat``.
"""

from __future__ import annotations

import io
import os
from dataclasses import dataclass, field
from typing import Iterator

from .binary import Reader, WindowReader, ascii_z, u32be, u64be
from .crypto import compute_keys
from .errors import (
    CorruptImage,
    EncryptedContent,
    KeyRequired,
    NotAPackage,
    TruncatedFile,
    UnsupportedFeature,
    WrongKey,
)

CNT_MAGIC = b"\x7fCNT"
FIH_MAGIC = b"\x7fFIH"
HEADER_SIZE = 0x5A0
ENTRY_SIZE = 0x20
PFS_NEW_CRYPT_FLAG = 0x2000000000000000
ENCRYPTED_ENTRY_FLAG = 0x80000000

#: Well-known entry ids, as documented for PS4/PS5 metadata containers.
ENTRY_IDS: dict[int, str] = {
    0x0001: "digests",
    0x0010: "entry_keys",
    0x0020: "image_key",
    0x0080: "general_digests",
    0x0100: "metas",
    0x0200: "entry_names",
    0x0400: "license.dat",
    0x0401: "license.info",
    0x0402: "nptitle.dat",
    0x0403: "npbind.dat",
    0x0404: "selfinfo.dat",
    0x0407: "target-deltainfo.dat",
    0x0408: "origin-deltainfo.dat",
    0x040A: "imagedigs.dat",
    0x1000: "param.sfo",
    0x1001: "playgo-chunk.dat",
    0x1002: "playgo-chunk.sha",
    0x1003: "playgo-manifest.xml",
    0x1004: "pronunciation.xml",
    0x1005: "pronunciation.sig",
    0x1007: "pubtoolinfo.dat",
    0x1200: "icon0.png",
    0x1220: "pic0.png",
    0x1240: "snd0.at9",
    0x1260: "changeinfo/changeinfo.xml",
    0x1280: "icon0.dds",
    0x12A0: "pic0.dds",
    0x12C0: "pic1.dds",
    0x2000: "param.json",
    0x2010: "playgo-hash-table.dat",
    0x2011: "playgo-ficm.dat",
}

CONTENT_TYPES: dict[int, str] = {
    0x1A: "GD (game data)",
    0x1B: "AC (additional content)",
    0x1C: "AL (additional license)",
    0x1E: "DP (delta patch)",
}

DRM_TYPES: dict[int, str] = {
    0x0: "none",
    0xF: "PS4",
    0x1: "PS4 (debug)",
}


@dataclass
class Entry:
    """One 0x20-byte record from a package entry table."""

    id: int
    name_offset: int
    flags1: int
    flags2: int
    data_offset: int
    data_size: int
    raw: bytes = field(repr=False, default=b"")

    @property
    def key_index(self) -> int:
        return (self.flags2 & 0xF000) >> 12

    @property
    def encrypted(self) -> bool:
        return bool(self.flags1 & ENCRYPTED_ENTRY_FLAG)

    def to_dict(self) -> dict:
        return {
            "id": f"0x{self.id:04X}",
            "name": ENTRY_IDS.get(self.id, ""),
            "name_offset": self.name_offset,
            "data_offset": self.data_offset,
            "data_size": self.data_size,
            "encrypted": self.encrypted,
            "key_index": self.key_index,
        }


@dataclass
class PkgHeader:
    """The big-endian ``\\x7FCNT`` header (PS4, or embedded in a PS5 image)."""

    magic: bytes
    flags: int
    entry_count: int
    sc_entry_count: int
    entry_table_offset: int
    main_ent_data_size: int
    body_offset: int
    body_size: int
    content_id: str
    drm_type: int
    content_type: int
    content_flags: int
    version_date: int
    version_hash: int
    pfs_image_count: int
    pfs_flags: int
    pfs_image_offset: int
    pfs_image_size: int
    mount_image_offset: int
    mount_image_size: int
    package_size: int
    pfs_signed_size: int
    pfs_cache_size: int

    @property
    def new_crypt(self) -> bool:
        return bool(self.pfs_flags & PFS_NEW_CRYPT_FLAG)

    @classmethod
    def parse(cls, data: bytes) -> "PkgHeader":
        if len(data) < HEADER_SIZE:
            raise TruncatedFile(f"package header needs 0x{HEADER_SIZE:X} bytes")
        if data[:4] != CNT_MAGIC:
            raise NotAPackage(f"expected {CNT_MAGIC!r} magic, got {data[:4]!r}")
        return cls(
            magic=data[:4],
            flags=u32be(data, 0x04),
            entry_count=u32be(data, 0x10),
            sc_entry_count=int.from_bytes(data[0x14:0x16], "big"),
            entry_table_offset=u32be(data, 0x18),
            main_ent_data_size=u32be(data, 0x1C),
            body_offset=u64be(data, 0x20),
            body_size=u64be(data, 0x28),
            content_id=ascii_z(data, 0x40, 0x30),
            drm_type=u32be(data, 0x70),
            content_type=u32be(data, 0x74),
            content_flags=u32be(data, 0x78),
            version_date=u32be(data, 0x80),
            version_hash=u32be(data, 0x84),
            pfs_image_count=u32be(data, 0x404),
            pfs_flags=u64be(data, 0x408),
            pfs_image_offset=u64be(data, 0x410),
            pfs_image_size=u64be(data, 0x418),
            mount_image_offset=u64be(data, 0x420),
            mount_image_size=u64be(data, 0x428),
            package_size=u64be(data, 0x430),
            pfs_signed_size=u32be(data, 0x438),
            pfs_cache_size=u32be(data, 0x43C),
        )

    def to_dict(self) -> dict:
        return {
            "content_id": self.content_id,
            "flags": f"0x{self.flags:08X}",
            "entry_count": self.entry_count,
            "entry_table_offset": f"0x{self.entry_table_offset:X}",
            "body_offset": f"0x{self.body_offset:X}",
            "body_size": self.body_size,
            "drm_type": f"0x{self.drm_type:X}",
            "drm_name": DRM_TYPES.get(self.drm_type, "unknown"),
            "content_type": f"0x{self.content_type:02X}",
            "content_type_name": CONTENT_TYPES.get(self.content_type, "unknown"),
            "content_flags": f"0x{self.content_flags:X}",
            "version_date": f"0x{self.version_date:08X}",
            "version_hash": f"0x{self.version_hash:08X}",
            "pfs_image_offset": f"0x{self.pfs_image_offset:X}",
            "pfs_image_size": self.pfs_image_size,
            "pfs_flags": f"0x{self.pfs_flags:016X}",
            "new_crypt": self.new_crypt,
            "package_size": self.package_size,
        }


@dataclass
class FihHeader:
    """The little-endian PS5 ``\\x7FFIH`` finalized-image envelope."""

    magic: bytes
    signed_flag: int
    pfs_offset: int
    pfs_size: int
    cnt_offset: int
    cnt_size: int

    @property
    def retail(self) -> bool:
        return bool(self.signed_flag & 0x80)

    @classmethod
    def parse(cls, data: bytes) -> "FihHeader":
        if len(data) < 0xB0:
            raise TruncatedFile("PS5 FIH header needs at least 0xB0 bytes")
        if data[:4] != FIH_MAGIC:
            raise NotAPackage(f"expected {FIH_MAGIC!r} magic, got {data[:4]!r}")
        return cls(
            magic=data[:4],
            signed_flag=data[5],
            pfs_offset=int.from_bytes(data[0x10:0x18], "little"),
            pfs_size=int.from_bytes(data[0x18:0x20], "little"),
            cnt_offset=int.from_bytes(data[0x58:0x60], "little"),
            cnt_size=int.from_bytes(data[0xA0:0xA8], "little"),
        )

    def to_dict(self) -> dict:
        return {
            "variant": "retail" if self.retail else "debug",
            "signed_flag": f"0x{self.signed_flag:02X}",
            "pfs_offset": f"0x{self.pfs_offset:X}",
            "pfs_size": self.pfs_size,
            "cnt_offset": f"0x{self.cnt_offset:X}",
            "cnt_size": self.cnt_size,
        }


class Container:
    """Entry table, name table, and entry payload access for a ``\\x7FCNT``."""

    def __init__(self, reader: Reader) -> None:
        self.reader = reader
        self.header = PkgHeader.parse(reader.read_at(0, HEADER_SIZE))
        if self.header.entry_count > 1_000_000:
            raise CorruptImage(f"implausible entry count {self.header.entry_count}")
        table_end = self.header.entry_table_offset + self.header.entry_count * ENTRY_SIZE
        if table_end > len(reader):
            raise CorruptImage(
                f"entry table at 0x{self.header.entry_table_offset:X} for "
                f"{self.header.entry_count} entries runs past the container"
            )
        self.entries: list[Entry] = list(self._read_entries())
        self._names: list[str] = []
        self._load_names()

    def _read_entries(self) -> Iterator[Entry]:
        raw = self.reader.read_at(
            self.header.entry_table_offset, self.header.entry_count * ENTRY_SIZE
        )
        for i in range(self.header.entry_count):
            rec = raw[i * ENTRY_SIZE : (i + 1) * ENTRY_SIZE]
            yield Entry(
                id=u32be(rec, 0x00),
                name_offset=u32be(rec, 0x04),
                flags1=u32be(rec, 0x08),
                flags2=u32be(rec, 0x0C),
                data_offset=u32be(rec, 0x10),
                data_size=u32be(rec, 0x14),
                raw=bytes(rec),
            )

    def _load_names(self) -> None:
        for entry in self.entries:
            if entry.id == 0x0200:
                if entry.data_offset + entry.data_size > len(self.reader):
                    raise CorruptImage("entry name table is out of bounds")
                blob = self.reader.read_at(entry.data_offset, entry.data_size)
                self._names = [part.decode("ascii", "replace") for part in blob.split(b"\x00")]
                return

    @property
    def content_id(self) -> str:
        return self.header.content_id

    def name_of(self, entry: Entry) -> str:
        if self._names and 0 < entry.name_offset < len(self._names):
            name = self._names[entry.name_offset]
            if name:
                return name
        return ENTRY_IDS.get(entry.id, "")

    def find(self, key: "int | str") -> Entry | None:
        """Look up an entry by numeric id, known name, or entry-table name."""
        for entry in self.entries:
            if isinstance(key, int):
                if entry.id == key:
                    return entry
            elif self.name_of(entry) == key or ENTRY_IDS.get(entry.id) == key:
                return entry
        return None

    def entry_bytes(self, entry: Entry, passcode: str | None = None) -> bytes:
        """Return the entry payload, decrypting it when needed and possible.

        Entry payloads that use the passcode-derived key can be decrypted with
        just the passcode; index-3 entries (licensed content) need RSA key
        material this tool does not ship.
        """
        if entry.data_size == 0:
            return b""
        if entry.data_offset + entry.data_size > len(self.reader):
            raise CorruptImage(
                f"entry 0x{entry.id:04X} at 0x{entry.data_offset:X} "
                f"runs past the container"
            )
        data = self.reader.read_at(entry.data_offset, entry.data_size)
        if not entry.encrypted:
            return data
        if not passcode:
            raise KeyRequired(
                f"entry 0x{entry.id:04X} ({self.name_of(entry)}) is encrypted; "
                "supply --passcode to decrypt it"
            )
        return _decrypt_entry(data, entry, self.content_id, passcode)


def _decrypt_entry(data: bytes, entry: Entry, content_id: str, passcode: str) -> bytes:
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

    if len(data) % 16:
        raise UnsupportedFeature(
            f"encrypted entry 0x{entry.id:04X} size {len(data)} is not AES block aligned"
        )
    from .crypto import compute_keys, sha256

    try:
        seed = compute_keys(content_id, passcode, entry.key_index)
    except ValueError as exc:
        raise KeyRequired(str(exc)) from exc
    digest = sha256(entry.raw + seed)
    key, iv = digest[16:32], digest[:16]
    decryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).decryptor()
    return decryptor.update(data) + decryptor.finalize()


class Pkg:
    """A parsed PS4 or PS5 package, backed by a random-access reader."""

    def __init__(
        self,
        path: str,
        reader: Reader,
        kind: str,
        container: Container,
        header: PkgHeader | None = None,
        fih: FihHeader | None = None,
    ) -> None:
        self.path = path
        self.reader = reader
        self.kind = kind
        self.container = container
        self.header = header
        self.fih = fih

    # -- construction ----------------------------------------------------
    @classmethod
    def open(cls, path: str) -> "Pkg":
        stream = open(path, "rb")
        reader = Reader(stream, name=os.path.basename(path))
        try:
            magic = reader.read_at(0, 4)
        except TruncatedFile as exc:
            stream.close()
            raise NotAPackage(f"{path}: file is too small to be a package") from exc

        try:
            if magic == CNT_MAGIC:
                container = Container(reader)
                return cls(path, reader, "PS4", container, header=container.header)
            if magic == FIH_MAGIC:
                fih = FihHeader.parse(reader.read_at(0, 0x100))
                if fih.cnt_size == 0 or fih.cnt_offset + fih.cnt_size > len(reader):
                    raise CorruptImage(
                        "PS5 image has no usable embedded metadata container"
                    )
                cnt = Container(
                    reader.slice(fih.cnt_offset, fih.cnt_size, "ps5-cnt")
                )
                return cls(path, reader, "PS5", cnt, fih=fih)
        except Exception:
            stream.close()
            raise
        raise NotAPackage(
            f"{path}: magic {magic!r} is neither PS4 {CNT_MAGIC!r} nor PS5 {FIH_MAGIC!r}"
        )

    def close(self) -> None:
        try:
            self.reader.stream.close()
        except Exception:  # pragma: no cover - best effort
            pass

    def __enter__(self) -> "Pkg":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # -- properties ------------------------------------------------------
    @property
    def content_id(self) -> str:
        return self.container.content_id

    def pfs_region(self) -> Reader | None:
        """The region that holds (outermost) PFS data."""
        if self.kind == "PS4":
            assert self.header is not None
            if self.header.pfs_image_size:
                end = self.header.pfs_image_offset + self.header.pfs_image_size
                if end <= len(self.reader):
                    return self.reader.slice(
                        self.header.pfs_image_offset,
                        self.header.pfs_image_size,
                        "ps4-pfs",
                    )
            if self.header.body_size:
                end = self.header.body_offset + self.header.body_size
                if end <= len(self.reader):
                    return self.reader.slice(
                        self.header.body_offset, self.header.body_size, "ps4-body"
                    )
            return None
        assert self.fih is not None
        if not self.fih.pfs_size:
            return None
        return self.reader.slice(self.fih.pfs_offset, self.fih.pfs_size, "ps5-outer-pfs")

    # -- content ---------------------------------------------------------
    def key_candidates(
        self, passcode: str | None = None, ekpfs: "bytes | str | None" = None
    ) -> list[bytes | None]:
        """Possible EKPFS values, in the order they should be tried."""
        keys: list[bytes | None] = []
        if ekpfs is not None:
            keys.append(_coerce_ekpfs(ekpfs))
        if passcode:
            from .crypto import compute_keys_sha3

            try:
                keys.append(compute_keys(self.content_id, passcode, 1))
            except ValueError as exc:
                raise KeyRequired(str(exc)) from exc
            keys.append(compute_keys_sha3(self.content_id, passcode, 1))
        if not keys:
            keys.append(None)
        return keys

    def open_pfs(
        self,
        passcode: str | None = None,
        ekpfs: "bytes | str | None" = None,
        new_crypt: bool | None = None,
    ) -> "PfsImage":
        """Open the application filesystem, trying available keys.

        For PS4 this is the PFS image referenced by the header. For PS5 the
        outer PFS is opened first and the inner ``pfs_image.dat`` is followed,
        which is where ``eboot.bin`` and ``sce_sys/`` actually live.
        """
        from .pfs import PfsImage

        region = self.pfs_region()
        if region is None:
            raise CorruptImage("package does not reference a PFS image")

        keys = self.key_candidates(passcode, ekpfs)
        flags = [new_crypt] if new_crypt is not None else [False, True]

        last_error: Exception | None = None
        for key in keys:
            for flag in flags:
                try:
                    if self.kind == "PS4":
                        return PfsImage(region, ekpfs=key, new_crypt=bool(flag))
                    return self._open_ps5_inner(region, key, bool(flag))
                except (KeyRequired, CorruptImage) as exc:
                    last_error = exc
                    continue

        if all(k is None for k in keys):
            raise KeyRequired(
                "the PFS image is encrypted; supply --passcode (or --ekpfs) to read it"
            )
        if isinstance(last_error, EncryptedContent):
            raise last_error
        if last_error is not None:
            raise WrongKey(
                f"no supplied key could read the PFS image: {last_error}"
            ) from last_error
        raise CorruptImage("unable to open the PFS image")

    def _open_ps5_inner(self, region: Reader, key: bytes | None, flag: bool) -> "PfsImage":
        from .pfs import PfsImage

        outer = PfsImage(region, ekpfs=key, new_crypt=flag)
        node = outer.find("pfs_image.dat")
        if node is None:
            node = outer.find_any(["pfs_image.dat", "image0/pfs_image.dat"])
        if node is None:
            raise CorruptImage(
                "outer PS5 image has no pfs_image.dat (is the inner image compressed?)"
            )
        inner_reader = outer.file_reader(node)
        return PfsImage(inner_reader, ekpfs=key, new_crypt=flag)

    def describe(self) -> dict:
        info: dict = {"path": self.path, "kind": self.kind, "content_id": self.content_id}
        if self.header is not None:
            info["header"] = self.header.to_dict()
        if self.fih is not None:
            info["fih"] = self.fih.to_dict()
        info["entry_count"] = len(self.container.entries)
        return info


def _coerce_ekpfs(ekpfs: "bytes | str") -> bytes:
    if isinstance(ekpfs, str):
        text = ekpfs.strip().replace(" ", "")
        if text.startswith("0x"):
            text = text[2:]
        try:
            raw = bytes.fromhex(text)
        except ValueError as exc:
            raise KeyRequired("EKPFS must be hex or 32 raw bytes") from exc
    else:
        raw = bytes(ekpfs)
    if len(raw) != 32:
        raise KeyRequired(f"EKPFS must be 32 bytes, got {len(raw)}")
    return raw


def open_package(path: str) -> Pkg:
    """Open a PS4 or PS5 package from ``path``."""
    return Pkg.open(path)
