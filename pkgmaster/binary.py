"""Small random-access binary helpers.

All parsing in pkgmaster works through :class:`Reader`, which wraps a seekable
stream and exposes ``read_at(offset, length)`` relative to a window. The same
class is used for the package file, a PFS image inside it, or a single file
inside a PFS image, so nested containers compose without copying multi-GB data.
"""

from __future__ import annotations

import io
import struct
from typing import BinaryIO

from .errors import TruncatedFile


class Reader:
    """A bounds-checked, positional view over a seekable stream."""

    __slots__ = ("stream", "base", "size", "name")

    def __init__(
        self,
        stream: BinaryIO,
        base: int = 0,
        size: int | None = None,
        name: str = "",
    ) -> None:
        self.stream = stream
        self.base = base
        self.name = name
        if size is None:
            current = stream.tell()
            stream.seek(0, io.SEEK_END)
            size = stream.tell() - base
            stream.seek(current)
        if size < 0:
            raise TruncatedFile(f"{name or 'input'}: negative window size")
        self.size = size

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0:
            raise TruncatedFile(f"{self.name or 'input'}: negative read ({offset}, {length})")
        if offset + length > self.size:
            raise TruncatedFile(
                f"{self.name or 'input'}: need {length} bytes at 0x{offset:X}, "
                f"but the window is only 0x{self.size:X} bytes"
            )
        self.stream.seek(self.base + offset)
        data = self.stream.read(length)
        if len(data) != length:
            raise TruncatedFile(
                f"{self.name or 'input'}: short read at 0x{offset:X} "
                f"(wanted {length}, got {len(data)})"
            )
        return data

    def slice(self, offset: int, size: int, name: str = "") -> "WindowReader":
        return WindowReader(self, offset, size, name or self.name)

    def __len__(self) -> int:
        return self.size

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"Reader(base=0x{self.base:X}, size=0x{self.size:X}, name={self.name!r})"


class WindowReader:
    """A bounded sub-window over any object that implements ``read_at``.

    This lets a file inside a PFS image, or an embedded PS5 container, be handed
    to another parser as if it were a standalone readable region.
    """

    __slots__ = ("parent", "offset", "size", "name")

    def __init__(self, parent, offset: int, size: int, name: str = "") -> None:
        if offset < 0 or size < 0 or offset + size > len(parent):
            raise TruncatedFile(
                f"{name or getattr(parent, 'name', 'input')}: window "
                f"0x{offset:X}+0x{size:X} exceeds 0x{len(parent):X}"
            )
        self.parent = parent
        self.offset = offset
        self.size = size
        self.name = name or getattr(parent, "name", "")

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0 or offset + length > self.size:
            raise TruncatedFile(
                f"{self.name or 'window'}: need {length} bytes at 0x{offset:X}, "
                f"window is 0x{self.size:X} bytes"
            )
        return self.parent.read_at(self.offset + offset, length)

    def slice(self, offset: int, size: int, name: str = "") -> "WindowReader":
        return WindowReader(self, offset, size, name or self.name)

    def __len__(self) -> int:
        return self.size

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"WindowReader(offset=0x{self.offset:X}, size=0x{self.size:X})"


def u8(data: bytes, offset: int = 0) -> int:
    return data[offset]


def u16le(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from("<H", data, offset)[0]


def u32le(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def u64le(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from("<Q", data, offset)[0]


def i32le(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from("<i", data, offset)[0]


def i64le(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from("<q", data, offset)[0]


def u16be(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from(">H", data, offset)[0]


def u32be(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from(">I", data, offset)[0]


def u64be(data: bytes, offset: int = 0) -> int:
    return struct.unpack_from(">Q", data, offset)[0]


def ascii_z(data: bytes, offset: int = 0, length: int | None = None) -> str:
    """Decode a NUL-terminated ASCII string from a fixed-size field."""
    end = len(data) if length is None else min(len(data), offset + length)
    raw = data[offset:end]
    nul = raw.find(b"\x00")
    if nul >= 0:
        raw = raw[:nul]
    return raw.decode("ascii", "replace")


def jsonable(value):
    """Recursively turn bytes and sets into JSON-friendly values."""
    if isinstance(value, bytes):
        return value.hex()
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if hasattr(value, "to_dict"):
        return jsonable(value.to_dict())
    return value
