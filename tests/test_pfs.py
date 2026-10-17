from __future__ import annotations

import io

import pytest

from pkgmaster.binary import Reader
from pkgmaster.crypto import compute_keys, compute_keys_sha3
from pkgmaster.errors import CorruptImage
from pkgmaster.pfs import PfsImage

from pkgmaster import synth as b


def open_image(data: bytes, **kwargs) -> PfsImage:
    return PfsImage(Reader(io.BytesIO(data), name="test.pfs"), **kwargs)


TREE = {
    "eboot.bin": b"ELF-ish payload " * 500,
    "sce_sys": {"param.sfo": b"PSF" * 40, "trophy2": {"trophy00.ucp": b"U" * 900}},
    "data.bin": bytes(range(256)) * 300,
    "empty.dat": b"",
}


@pytest.mark.parametrize("signed,bits", [(False, 32), (True, 32), (True, 64)])
def test_reads_all_inode_layouts(signed, bits):
    image = b.build_pfs(TREE, signed=signed, inode_bits=bits)
    pfs = open_image(image)
    assert pfs.header.signed is signed
    assert pfs.header.is_64bit is (bits == 64)

    for path, expected in [
        ("eboot.bin", TREE["eboot.bin"]),
        ("sce_sys/param.sfo", TREE["sce_sys"]["param.sfo"]),
        ("sce_sys/trophy2/trophy00.ucp", TREE["sce_sys"]["trophy2"]["trophy00.ucp"]),
        ("data.bin", TREE["data.bin"]),
    ]:
        handle = pfs.find(path)
        assert handle is not None, path
        assert handle.size == len(expected)
        assert handle.read_at(0, len(expected)) == expected

    assert pfs.find("empty.dat").size == 0
    assert pfs.find("missing.bin") is None


def test_walk_and_directories():
    pfs = open_image(b.build_pfs(TREE))
    paths = [path for path, _ in pfs.walk()]
    assert "sce_sys" in paths
    assert "sce_sys/trophy2" in paths
    assert "sce_sys/trophy2/trophy00.ucp" in paths
    assert sorted(p for p, _ in pfs.all_files()) == [
        "data.bin",
        "eboot.bin",
        "empty.dat",
        "sce_sys/param.sfo",
        "sce_sys/trophy2/trophy00.ucp",
    ]


def test_range_reads_span_blocks():
    pfs = open_image(b.build_pfs(TREE))
    handle = pfs.find("data.bin")
    body = TREE["data.bin"]
    assert handle.read_at(0x10000, 0x100) == body[0x10000 : 0x10100]
    assert handle.read_at(len(body) - 16, 16) == body[-16:]
    assert handle.read_at(len(body), 16) == b""


def test_multi_block_file_across_blocks():
    payload = b"Z" * (0x10000 * 3 + 1234)
    pfs = open_image(b.build_pfs({"big.bin": payload}))
    assert pfs.read_file(pfs.resolve("big.bin")) == payload


def test_encrypted_image_round_trip():
    image = b.build_pfs(TREE, encrypted=True, ekpfs=None)
    ekpfs = compute_keys_sha3(b.CONTENT_ID, b.PASSCODE, 1)
    pfs = open_image(image, ekpfs=ekpfs)
    assert pfs.header.encrypted
    assert pfs.read_file(pfs.resolve("eboot.bin")) == TREE["eboot.bin"]


def test_encrypted_image_requires_key():
    image = b.build_pfs(TREE, encrypted=True)
    with pytest.raises(Exception) as excinfo:
        open_image(image)
    assert "encrypted" in str(excinfo.value)


def test_wrong_key_is_reported():
    image = b.build_pfs(TREE, encrypted=True)
    with pytest.raises(CorruptImage):
        open_image(image, ekpfs=bytes(32))


def test_bad_magic_is_rejected():
    with pytest.raises(CorruptImage):
        open_image(b"\x00" * 0x1000)


def test_compressed_flag_without_pfsc_stream_is_rejected():
    image = bytearray(b.build_pfs({"a.bin": b"A" * 0x1000}))
    # Mark the first file inode (index 2) compressed.
    inode_size = 0xA8
    offset = 0x10000 + 2 * inode_size
    image[offset + 4] |= 0x1
    pfs = open_image(bytes(image))
    with pytest.raises(CorruptImage):
        pfs.read_file(pfs.resolve("a.bin"))
