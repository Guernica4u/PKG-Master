from __future__ import annotations

import pytest

from pkgmaster.errors import CorruptImage, KeyRequired, NotAPackage, TruncatedFile
from pkgmaster.pkg import Pkg, open_package

from pkgmaster import synth as b

TREE = {"eboot.bin": b"payload" * 200, "sce_sys": {"param.sfo": b"PSF" * 16}}


def write(tmp_path, name, data: bytes) -> str:
    path = tmp_path / name
    path.write_bytes(data)
    return str(path)


def test_ps4_header_and_entries(tmp_path):
    path = write(tmp_path, "game.pkg", b.build_pkg(b.build_pfs(TREE), icon=b"\x89PNG" + b"\x00" * 12))
    pkg = open_package(path)
    assert pkg.kind == "PS4"
    assert pkg.content_id == b.CONTENT_ID
    assert pkg.header.content_type == 0x1A
    assert pkg.header.pfs_image_size > 0

    names = {pkg.container.name_of(e) for e in pkg.container.entries}
    assert {"param.sfo", "icon0.png", "entry_names"} <= names
    assert pkg.container.find("param.sfo") is not None
    assert pkg.container.find(0x1200) is not None
    assert pkg.container.entry_bytes(pkg.container.find("param.sfo")) == b.PARAM_SFO
    pkg.close()


def test_entry_name_table_offsets_resolve(tmp_path):
    path = write(tmp_path, "game.pkg", b.build_pkg(b.build_pfs(TREE), icon=b"PNG"))
    pkg = open_package(path)
    for entry in pkg.container.entries:
        name = pkg.container.name_of(entry)
        assert name, f"entry 0x{entry.id:04X} has no name"
    pkg.close()


def test_encrypted_entry_needs_and_accepts_passcode(tmp_path):
    path = write(
        tmp_path,
        "game.pkg",
        b.build_pkg(
            b.build_pfs(TREE),
            encrypted_license=b"LICENSE" * 4,
            passcode=b.PASSCODE,
            license_key_index=2,
        ),
    )
    pkg = open_package(path)
    entry = pkg.container.find("license.dat")
    assert entry.encrypted and entry.key_index == 2
    with pytest.raises(KeyRequired):
        pkg.container.entry_bytes(entry)
    plain = pkg.container.entry_bytes(entry, b.PASSCODE)
    assert plain.startswith(b"LICENSE")
    pkg.close()


def test_ps4_content_extraction(tmp_path):
    path = write(tmp_path, "game.pkg", b.build_pkg(b.build_pfs(TREE)))
    with open_package(path) as pkg:
        pfs = pkg.open_pfs()
        assert pfs.find("eboot.bin").read_at(0, 7) == b"payload"
        assert pfs.find("sce_sys/param.sfo") is not None


def test_ps5_envelope_and_nested_image(tmp_path):
    path = write(tmp_path, "game.pkg", b.build_ps5(TREE))
    with open_package(path) as pkg:
        assert pkg.kind == "PS5"
        assert pkg.fih.to_dict()["variant"] == "debug"
        assert pkg.content_id == b.CONTENT_ID
        pfs = pkg.open_pfs()
        assert sorted(p for p, _ in pfs.all_files()) == [
            "eboot.bin",
            "sce_sys/param.sfo",
        ]


def test_ps4_encrypted_pfs_needs_passcode(tmp_path):
    path = write(tmp_path, "game.pkg", b.build_pkg(b.build_pfs(TREE, encrypted=True)))
    with open_package(path) as pkg:
        with pytest.raises(KeyRequired):
            pkg.open_pfs()
        # The SHA3 candidate from --passcode should open the image.
        pfs = pkg.open_pfs(passcode=b.PASSCODE)
        assert pfs.find("eboot.bin").size > 0


def test_ps4_encrypted_pfs_accepts_explicit_ekpfs(tmp_path):
    from pkgmaster.crypto import compute_keys_sha3

    path = write(tmp_path, "game.pkg", b.build_pkg(b.build_pfs(TREE, encrypted=True)))
    with open_package(path) as pkg:
        key = compute_keys_sha3(b.CONTENT_ID, b.PASSCODE, 1)
        pfs = pkg.open_pfs(ekpfs=key)
        assert pfs.find("eboot.bin").size > 0


def test_not_a_package(tmp_path):
    path = write(tmp_path, "nope.pkg", b"\x7fELF" + b"\x00" * 64)
    with pytest.raises(NotAPackage):
        open_package(path)


def test_truncated_package(tmp_path):
    data = b.build_pkg(b.build_pfs(TREE))
    path = write(tmp_path, "cut.pkg", data[:0x2000])
    with pytest.raises((TruncatedFile, NotAPackage, CorruptImage)):
        pkg = open_package(path)
        pkg.open_pfs()


def test_large_ekpfs_is_read_via_hex_string(tmp_path):
    from pkgmaster.crypto import compute_keys_sha3

    path = write(tmp_path, "game.pkg", b.build_pkg(b.build_pfs(TREE, encrypted=True)))
    key_hex = compute_keys_sha3(b.CONTENT_ID, b.PASSCODE, 1).hex()
    with open_package(path) as pkg:
        pfs = pkg.open_pfs(ekpfs=key_hex)
        assert pfs.find("eboot.bin") is not None
