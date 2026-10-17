"""Guard the standalone ``pkg2elf.py`` script against drift.

``pkg2elf.py`` reimplements the PKG -> PFS -> SELF -> ELF path in one file.
HANDOFF.md claims it produces byte-identical output to the ``pkgmaster``
library, so that claim is asserted here rather than left to manual `cmp`.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from pkgmaster import synth as b

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "pkg2elf.py"
PASSCODE = b.PASSCODE

CODE = b"\x90\x90\xcc\x90" * 40
RODATA = b"standalone-rodata" * 40
TREE = {"eboot.bin": b.build_self(b.build_elf([CODE, RODATA]))}


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, *args], cwd=ROOT, capture_output=True, text=True
    )


def library_elf(pkg: Path, out: Path, *extra: str) -> subprocess.CompletedProcess:
    return run("-m", "pkgmaster", "convert", str(pkg), "--to", "elf", "-o", str(out), *extra)


def script_elf(pkg: Path, out: Path, *extra: str) -> subprocess.CompletedProcess:
    return run(str(SCRIPT), str(pkg), "-o", str(out), *extra)


def test_script_is_committed_and_runnable():
    assert SCRIPT.exists()
    result = run(str(SCRIPT), "--help")
    assert result.returncode == 0
    assert "ELF" in result.stdout


@pytest.mark.parametrize("kind", ["ps4", "ps5"])
def test_matches_library_output(tmp_path, kind):
    pkg = tmp_path / f"{kind}.pkg"
    pkg.write_bytes(b.build_ps5(TREE) if kind == "ps5" else b.build_pkg(b.build_pfs(TREE)))

    lib_out, script_out = tmp_path / "lib.elf", tmp_path / "script.elf"
    lib = library_elf(pkg, lib_out)
    scr = script_elf(pkg, script_out)
    assert lib.returncode == 0, lib.stderr
    assert scr.returncode == 0, scr.stderr
    assert lib_out.read_bytes() == script_out.read_bytes()
    assert lib_out.read_bytes()[:4] == b"\x7fELF"


def test_matches_library_for_encrypted_package(tmp_path):
    pkg = tmp_path / "enc.pkg"
    pkg.write_bytes(b.build_pkg(b.build_pfs(TREE, encrypted=True)))

    lib_out, script_out = tmp_path / "lib.elf", tmp_path / "script.elf"
    assert library_elf(pkg, lib_out, "--passcode", PASSCODE).returncode == 0
    result = script_elf(pkg, script_out, "--passcode", PASSCODE)
    assert result.returncode == 0, result.stderr
    assert lib_out.read_bytes() == script_out.read_bytes()


def test_matches_library_for_raw_self(tmp_path):
    raw = tmp_path / "right.sprx"
    raw.write_bytes(b.build_self(b.build_elf([CODE, RODATA])))

    lib_out, script_out = tmp_path / "lib.elf", tmp_path / "script.elf"
    assert library_elf(raw, lib_out).returncode == 0
    assert script_elf(raw, script_out).returncode == 0
    assert lib_out.read_bytes() == script_out.read_bytes()


def test_script_exit_codes(tmp_path):
    """3 for a missing key, 2 for junk - the script must not crash or lie."""
    encrypted = tmp_path / "enc.pkg"
    encrypted.write_bytes(b.build_pkg(b.build_pfs(TREE, encrypted=True)))
    result = run(str(SCRIPT), str(encrypted), "-o", str(tmp_path / "x.elf"))
    assert result.returncode == 3
    assert not (tmp_path / "x.elf").exists()

    junk = tmp_path / "junk.bin"
    junk.write_bytes(b"\x00" * 256)
    assert run(str(SCRIPT), str(junk)).returncode == 2

    locked = tmp_path / "locked.bin"
    locked.write_bytes(
        b.build_self(b.build_elf([CODE, RODATA]), encrypt_first=True)
    )
    result = run(str(SCRIPT), str(locked), "-o", str(tmp_path / "y.elf"))
    assert result.returncode == 2
    assert "encrypted" in result.stderr


def test_script_info_and_list(tmp_path):
    pkg = tmp_path / "game.pkg"
    pkg.write_bytes(b.build_pkg(b.build_pfs(TREE)))

    info = run(str(SCRIPT), str(pkg), "--info")
    assert info.returncode == 0, info.stderr
    assert "PS4" in info.stdout

    listing = run(str(SCRIPT), str(pkg), "--list")
    assert listing.returncode == 0, listing.stderr
    assert "eboot.bin" in listing.stdout
    assert "param.sfo" in listing.stdout


@pytest.mark.parametrize("ps5", [False, True])
def test_matches_library_for_compressed_eboot(tmp_path, ps5):
    pkg = tmp_path / "pfsc.pkg"
    pkg.write_bytes(b.build_sample(ps5=ps5, compressed=True))

    lib_out, script_out = tmp_path / "lib.elf", tmp_path / "script.elf"
    lib = library_elf(pkg, lib_out)
    scr = script_elf(pkg, script_out)
    assert lib.returncode == 0, lib.stderr
    assert scr.returncode == 0, scr.stderr
    assert lib_out.read_bytes() == script_out.read_bytes()


def test_matches_library_for_compressed_inner_image(tmp_path):
    """A PS5 image whose pfs_image.dat and eboot.bin are both PFSC-compressed."""
    tree = {"eboot.bin": b.Compressed(TREE["eboot.bin"], block_size=0x1000)}
    pkg = tmp_path / "pfsc-inner.pkg"
    pkg.write_bytes(b.build_ps5(tree, compress_inner=True))

    lib_out, script_out = tmp_path / "lib.elf", tmp_path / "script.elf"
    lib = library_elf(pkg, lib_out)
    scr = script_elf(pkg, script_out)
    assert lib.returncode == 0, lib.stderr
    assert scr.returncode == 0, scr.stderr
    assert lib_out.read_bytes() == script_out.read_bytes()
    assert lib_out.read_bytes()[:4] == b"\x7fELF"


def test_script_rejects_corrupt_compressed_eboot(tmp_path):
    eboot = b.Compressed(TREE["eboot.bin"])
    stored = bytearray(eboot.stored)
    stored[-20] ^= 0xFF
    pkg = tmp_path / "bad.pkg"
    pkg.write_bytes(
        b.build_pkg(b.build_pfs({"eboot.bin": b.Compressed(stored=bytes(stored), size=eboot.size)}))
    )
    out = tmp_path / "bad.elf"
    result = script_elf(pkg, out)
    assert result.returncode == 2
    assert "PFSC" in result.stderr
    assert "Traceback" not in result.stderr
    assert not out.exists()
