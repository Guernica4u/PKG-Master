"""PFSC (block-compressed) file reads.

Every corrupt-input test asserts :class:`CorruptImage`: a damaged stream must
never come back as plausible-looking bytes.
"""

from __future__ import annotations

import io
import random
import struct
import zlib

import pytest

from pkgmaster.binary import Reader
from pkgmaster.errors import CorruptImage
from pkgmaster.pfs import PfsImage, PfscReader

from pkgmaster import synth as b

BS = b.PFS_BLOCK
NOISE = random.Random(1234).randbytes(BS)
# Block 0 text, block 1 incompressible (stored raw), block 2 text, partial block 3.
PAYLOAD = (
    b"compressible text block " * (BS // 24) + b"x" * (BS % 24)
    + NOISE
    + bytes(range(256)) * (BS // 256)
    + b"tail" * 0x500
)
assert len(PAYLOAD) == 3 * BS + 0x1400


def open_image(tree: dict) -> PfsImage:
    return PfsImage(Reader(io.BytesIO(b.build_pfs(tree)), name="test.pfs"))


def open_file(value: b.Compressed):
    pfs = open_image({"f.bin": value})
    return pfs, pfs.resolve("f.bin"), pfs.find("f.bin")


def table(stored: bytes) -> list[int]:
    count = -(-struct.unpack_from("<Q", stored, 0x28)[0] // struct.unpack_from("<I", stored, 0x0C)[0])
    table_offset = struct.unpack_from("<Q", stored, 0x18)[0]
    return list(struct.unpack_from(f"<{count + 1}Q", stored, table_offset))


def set_table(stored: bytes, index: int, value: int) -> bytes:
    out = bytearray(stored)
    struct.pack_into("<Q", out, struct.unpack_from("<Q", stored, 0x18)[0] + index * 8, value)
    return bytes(out)


# -- good streams ------------------------------------------------------------


def test_inode_is_marked_compressed():
    pfs, inode, _ = open_file(b.Compressed(PAYLOAD))
    assert inode.is_compressed
    assert inode.flags & 0x1
    assert inode.size == len(PAYLOAD)
    assert inode.size_compressed < len(PAYLOAD)


def test_whole_file_read():
    pfs, inode, handle = open_file(b.Compressed(PAYLOAD))
    assert pfs.read_file(inode) == PAYLOAD
    assert handle.read_at(0, len(PAYLOAD) + 100) == PAYLOAD

    out = io.BytesIO()
    assert pfs.copy_file(inode, out, chunk_size=0x3333) == len(PAYLOAD)
    assert out.getvalue() == PAYLOAD


@pytest.mark.parametrize(
    "offset,length",
    [
        (0x10, 0x20),                 # inside block 0
        (BS - 7, 14),                 # straddles blocks 0/1
        (BS // 2, 2 * BS),            # mid block 0 to mid block 2
        (BS + 123, BS),               # starts inside the raw block
        (3 * BS - 5, 10),             # into the partial last block
        (3 * BS + 0x100, 0x200),      # entirely inside the last block
    ],
)
def test_reads_that_start_and_end_mid_block(offset, length):
    _, _, handle = open_file(b.Compressed(PAYLOAD))
    assert handle.read_at(offset, length) == PAYLOAD[offset : offset + length]


def test_last_partial_block_and_eof_clamping():
    _, _, handle = open_file(b.Compressed(PAYLOAD))
    assert handle.read_at(3 * BS, 0x1400) == PAYLOAD[3 * BS :]
    assert handle.read_at(len(PAYLOAD) - 4, 100) == b"tail"
    assert handle.read_at(len(PAYLOAD), 10) == b""


def test_uncompressed_blocks_are_stored_raw():
    stored = b.build_pfsc(PAYLOAD, raw_blocks={0, 3})
    sizes = [end - start for start, end in zip(table(stored), table(stored)[1:])]
    # 0 and 3 forced raw, 1 is noise so the encoder falls back to raw by itself.
    assert sizes[0] == sizes[1] == sizes[3] == BS
    assert sizes[2] < BS
    _, _, handle = open_file(b.Compressed(PAYLOAD, stored=stored))
    assert handle.read_at(0, len(PAYLOAD)) == PAYLOAD
    assert handle.read_at(3 * BS + 1, 0x10) == PAYLOAD[3 * BS + 1 : 3 * BS + 0x11]


def test_raw_deflate_and_small_block_size():
    data = PAYLOAD[: 5 * 0x1000 + 77]
    for options in ({"zlib_header": False}, {"block_size": 0x1000}):
        _, _, handle = open_file(b.Compressed(data, **options))
        assert handle.read_at(0, len(data)) == data
        assert handle.read_at(0x1FFF, 3) == data[0x1FFF:0x2002]


def test_last_block_padded_to_full_size_is_accepted():
    padded = PAYLOAD + b"\x00" * (-len(PAYLOAD) % BS)
    stored = bytearray(b.build_pfsc(padded))
    struct.pack_into("<Q", stored, 0x28, len(PAYLOAD))
    _, _, handle = open_file(b.Compressed(stored=bytes(stored), size=len(PAYLOAD)))
    assert handle.read_at(0, len(PAYLOAD) + 50) == PAYLOAD


def test_empty_compressed_file():
    pfs, inode, handle = open_file(b.Compressed(b""))
    assert handle.read_at(0, 10) == b""


def test_reads_are_streaming():
    """No read of the stored stream exceeds one block, however much is asked."""

    class Recorder:
        def __init__(self, data):
            self.data, self.name, self.reads = data, "rec", []

        def read_at(self, offset, length):
            self.reads.append(length)
            return self.data[offset : offset + length]

        def __len__(self):
            return len(self.data)

    stored = Recorder(b.build_pfsc(PAYLOAD))
    reader = PfscReader(stored, len(PAYLOAD))
    assert reader.read_at(0, len(PAYLOAD)) == PAYLOAD
    assert max(stored.reads) <= BS


def test_nested_image_inside_compressed_file():
    inner = b.build_pfs({"eboot.bin": PAYLOAD[:5000]}, version=2)
    outer = open_image({"pfs_image.dat": b.Compressed(inner, block_size=0x4000)})
    nested = PfsImage(outer.find("pfs_image.dat"))
    assert nested.read_file(nested.resolve("eboot.bin")) == PAYLOAD[:5000]


# -- corrupt streams ---------------------------------------------------------


GOOD = b.build_pfsc(PAYLOAD)


def corrupt_cases():
    t = table(GOOD)
    yield "no magic", b"XXXX" + GOOD[4:]
    yield "truncated header", GOOD[:0x20]
    yield "truncated table", GOOD[: 0x400 + 16]
    yield "table offset past end", GOOD[:0x18] + struct.pack("<Q", len(GOOD)) + GOOD[0x20:]
    yield "zero block size", GOOD[:0x0C] + b"\x00" * 4 + GOOD[0x10:]
    yield "block sizes disagree", GOOD[:0x10] + struct.pack("<Q", BS * 2) + GOOD[0x18:]
    yield "offsets go backwards", set_table(set_table(GOOD, 1, t[2]), 2, t[1])
    yield "offset past stream end", set_table(GOOD, 4, len(GOOD) + 1)
    yield "offset inside table", set_table(GOOD, 0, 0x400)
    yield "block larger than block size", set_table(GOOD, 2, t[1] + BS + 1)
    yield "empty block", set_table(GOOD, 1, t[0])
    flipped = bytearray(GOOD)
    flipped[t[0] + (t[1] - t[0]) // 2] ^= 0x55
    yield "corrupt deflate data", bytes(flipped)
    yield "stream cut short", GOOD[:-100]


@pytest.mark.parametrize("label,stored", list(corrupt_cases()), ids=lambda v: v if isinstance(v, str) else "")
def test_corrupt_stream_raises(label, stored):
    pfs, inode, handle = open_file(b.Compressed(stored=stored, size=len(PAYLOAD)))
    with pytest.raises(CorruptImage):
        pfs.read_file(inode)
    with pytest.raises(CorruptImage):
        pfs.copy_file(inode, io.BytesIO())


def test_cut_stream_fails_only_where_data_is_missing():
    """The intact blocks still read; the damaged tail raises instead of padding."""
    _, _, handle = open_file(b.Compressed(stored=GOOD[:-100], size=len(PAYLOAD)))
    assert handle.read_at(0, 0x100) == PAYLOAD[:0x100]
    with pytest.raises(CorruptImage):
        handle.read_at(len(PAYLOAD) - 10, 10)


def test_size_mismatch_with_header_raises():
    pfs, inode, _ = open_file(b.Compressed(stored=GOOD, size=len(PAYLOAD) - 1))
    with pytest.raises(CorruptImage):
        pfs.read_file(inode)


def test_block_decoding_to_wrong_length_raises():
    stored = bytearray(b.build_pfsc(PAYLOAD[:2 * BS]))
    t = table(bytes(stored))
    short = zlib.compress(b"short block")
    stored = bytes(stored[: t[0]]) + short + bytes(stored[t[1] : t[2]])
    stored = set_table(stored, 1, t[0] + len(short))
    stored = set_table(stored, 2, len(stored))
    pfs, inode, handle = open_file(b.Compressed(stored=stored, size=2 * BS))
    with pytest.raises(CorruptImage):
        handle.read_at(0, 10)


def test_trailing_garbage_after_deflate_stream_raises():
    stored = bytearray(b.build_pfsc(PAYLOAD[:BS + 10]))
    t = table(bytes(stored))
    # Overwrite the slack after block 1's stream end by growing its extent.
    block1 = bytes(stored[t[1] : t[2]]) + b"JUNK"
    stored = bytes(stored[: t[1]]) + block1
    stored = set_table(stored, 2, len(stored))
    pfs, inode, handle = open_file(b.Compressed(stored=stored, size=BS + 10))
    with pytest.raises(CorruptImage):
        handle.read_at(BS, 10)


def test_compressed_inode_without_stored_data_raises():
    pfs, inode, _ = open_file(b.Compressed(PAYLOAD))
    inode.size_compressed = 0
    pfs._pfsc.clear()
    with pytest.raises(CorruptImage):
        pfs.read_file(inode)
