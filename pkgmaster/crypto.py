"""Key derivation and AES-XTS handling for PFS images.

Two independent pieces live here:

* ``compute_keys`` - the FPKG passcode -> derived key function (SHA-256).
* PFS XTS - per-image tweak/data key derivation, and a positional decrypting
  reader used by :class:`pkgmaster.pfs.PfsImage`.

No console keys, RSA private keys, or content keys are embedded. A passcode
derived key only works on packages that were built with the same passcode
(fake/debug packages); retail packages need key material this tool cannot and
does not provide.
"""

from __future__ import annotations

import hashlib
import hmac
import struct

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

from .binary import Reader, WindowReader
from .errors import TruncatedFile

XTS_SECTOR_SIZE = 0x1000


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def hmac_sha256(key: bytes, data: bytes) -> bytes:
    return hmac.new(key, data, hashlib.sha256).digest()


def compute_keys(content_id: str, passcode: str, index: int = 1) -> bytes:
    """Derive the FPKG key for ``index`` from a content id and passcode.

    Mirrors the documented scheme::

        SHA256( SHA256(BE32(index)) || SHA256(content_id padded to 48) || passcode )

    ``index == 1`` is the EKPFS used to derive PFS XTS keys.
    """
    if len(content_id) != 36:
        raise ValueError(f"content id must be 36 characters, got {len(content_id)}")
    if len(passcode) != 32:
        raise ValueError(f"passcode must be 32 characters, got {len(passcode)}")
    material = (
        sha256(struct.pack(">I", index))
        + sha256(content_id.encode("ascii").ljust(48, b"\x00"))
        + passcode.encode("ascii")
    )
    return sha256(material)


def compute_keys_sha3(content_id: str, passcode: str, index: int = 1) -> bytes:
    """SHA3-256 variant used by some PS5 debug images."""
    if len(content_id) != 36 or len(passcode) != 32:
        raise ValueError("content id must be 36 characters and passcode 32")
    material = (
        hashlib.sha3_256(struct.pack(">I", index)).digest()
        + hashlib.sha3_256(content_id.encode("ascii").ljust(48, b"\x00")).digest()
        + passcode.encode("ascii")
    )
    return hashlib.sha3_256(material).digest()


def pfs_gen_enc_keys(ekpfs: bytes, seed: bytes, new_crypt: bool = False) -> tuple[bytes, bytes]:
    """Return ``(tweak_key, data_key)`` for a PFS image.

    ``new_crypt`` is the alternate PS5-era schedule where the HMAC key itself is
    ``HMAC-SHA256(EKPFS, seed)``.
    """
    base = hmac_sha256(ekpfs, seed) if new_crypt else ekpfs
    enc_key = hmac_sha256(base, struct.pack("<I", 1) + seed)
    return enc_key[:16], enc_key[16:32]


def pfs_gen_xts_key(ekpfs: bytes, seed: bytes, new_crypt: bool = False) -> bytes:
    """Combined 32-byte XTS key in the order ``data_key || tweak_key``."""
    tweak_key, data_key = pfs_gen_enc_keys(ekpfs, seed, new_crypt)
    return data_key + tweak_key


def pfs_xts_tweak(sector_number: int) -> bytes:
    return struct.pack("<QQ", sector_number, 0)


def xts_crypt_sector(
    sector_data: bytes, xts_key: bytes, sector_number: int, *, encrypt: bool
) -> bytes:
    """Encrypt or decrypt one 4 KiB XTS sector."""
    if len(xts_key) != 32:
        raise ValueError("XTS key must be 32 bytes (data key followed by tweak key)")
    if len(sector_data) % 16:
        raise ValueError("XTS input must be a multiple of 16 bytes")
    cipher = Cipher(algorithms.AES(xts_key), modes.XTS(pfs_xts_tweak(sector_number)))
    transform = cipher.encryptor() if encrypt else cipher.decryptor()
    return transform.update(sector_data) + transform.finalize()


class XtsDecryptReader:
    """A :class:`Reader`-compatible view that decrypts PFS XTS sectors.

    The first ``start_sector`` sectors (the plaintext superblock block) are
    passed through unchanged, matching the on-disk layout.
    """

    __slots__ = ("base", "xts_key", "start_sector", "size", "name", "sector_size")

    def __init__(
        self,
        base: Reader,
        xts_key: bytes,
        start_sector: int,
        sector_size: int = XTS_SECTOR_SIZE,
    ) -> None:
        self.base = base
        self.xts_key = xts_key
        self.start_sector = start_sector
        self.sector_size = sector_size
        self.size = base.size
        self.name = base.name

    def read_at(self, offset: int, length: int) -> bytes:
        if offset < 0 or length < 0:
            raise TruncatedFile("negative XTS read")
        if offset + length > self.size:
            raise TruncatedFile(
                f"{self.name or 'pfs'}: need {length} bytes at 0x{offset:X}, "
                f"but the image is only 0x{self.size:X} bytes"
            )
        if length == 0:
            return b""

        ss = self.sector_size
        first = offset // ss
        last = (offset + length - 1) // ss
        out = bytearray()

        for sector in range(first, last + 1):
            sector_start = sector * ss
            available = min(ss, self.size - sector_start)
            if available <= 0:
                break
            chunk = self.base.read_at(sector_start, available)
            if sector >= self.start_sector:
                if len(chunk) % 16:
                    padded = chunk + b"\x00" * (16 - len(chunk) % 16)
                    chunk = xts_crypt_sector(
                        padded, self.xts_key, sector, encrypt=False
                    )[: len(chunk)]
                else:
                    chunk = xts_crypt_sector(
                        chunk, self.xts_key, sector, encrypt=False
                    )
            out += chunk

        begin = offset - first * ss
        return bytes(out[begin : begin + length])

    def slice(self, offset: int, size: int, name: str = "") -> WindowReader:
        return WindowReader(self, offset, size, name or self.name)

    def __len__(self) -> int:
        return self.size
