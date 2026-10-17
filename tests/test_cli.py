from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from pkgmaster.elf import parse_elf

from pkgmaster import synth as b

ROOT = Path(__file__).resolve().parents[1]

CODE = b"\x90\x90\xcc\x90" * 40
RODATA = b"cli-rodata" * 60


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, "-m", "pkgmaster", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )


@pytest.fixture()
def package(tmp_path) -> str:
    elf = b.build_elf([CODE, RODATA])
    tree = {
        "eboot.bin": b.build_self(elf),
        "sce_sys": {"param.sfo": b.PARAM_SFO},
    }
    path = tmp_path / "game.pkg"
    path.write_bytes(b.build_pkg(b.build_pfs(tree)))
    return str(path)


def test_info_json(package):
    result = run("info", package, "--json")
    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["kind"] == "PS4"
    assert payload["content_id"] == b.CONTENT_ID
    assert payload["pfs"]["encrypted"] is False


def test_entries_and_list(package):
    result = run("entries", package)
    assert result.returncode == 0, result.stderr
    assert "param.sfo" in result.stdout

    result = run("list", package)
    assert result.returncode == 0, result.stderr
    assert "eboot.bin" in result.stdout
    assert "sce_sys/param.sfo" in result.stdout


def test_tree(package):
    result = run("tree", package)
    assert result.returncode == 0, result.stderr
    assert "uroot/" in result.stdout
    assert "sce_sys/" in result.stdout


def test_extract_all(package, tmp_path):
    out = tmp_path / "out"
    result = run("extract", package, "-o", str(out))
    assert result.returncode == 0, result.stderr
    assert (out / "eboot.bin").exists()
    assert (out / "sce_sys" / "param.sfo").read_bytes().startswith(b"\x00PSF")


def test_extract_single_path(package, tmp_path):
    out = tmp_path / "out"
    result = run("extract", package, "-o", str(out), "--path", "eboot.bin")
    assert result.returncode == 0, result.stderr
    assert (out / "eboot.bin").exists()
    assert not (out / "sce_sys").exists()


def test_convert_produces_loadable_elf(package, tmp_path):
    out = tmp_path / "eboot.elf"
    result = run("convert", package, "--to", "elf", "-o", str(out))
    assert result.returncode == 0, result.stderr
    data = out.read_bytes()
    assert data[:4] == b"\x7fELF"

    info = parse_elf(data)
    assert info.type_name == "ET_SCE_EXEC_ASLR"
    loads = info.load_segments()
    assert loads
    assert any(
        data[p.p_offset : p.p_offset + p.p_filesz] == CODE for p in loads
    ), "code payload must survive the SELF -> ELF conversion"


def test_convert_default_output_name(package):
    result = run("convert", package, "--to", "elf")
    assert result.returncode == 0, result.stderr
    produced = ROOT / "eboot.elf"
    assert produced.exists()
    produced.unlink()


def test_convert_standalone_self(tmp_path):
    blob = tmp_path / "module.sprx"
    blob.write_bytes(b.build_self(b.build_elf([CODE])))
    out = tmp_path / "module.elf"
    result = run("convert", str(blob), "--to", "elf", "-o", str(out))
    assert result.returncode == 0, result.stderr
    assert out.read_bytes()[:4] == b"\x7fELF"


def test_convert_unknown_target_is_rejected(tmp_path):
    blob = tmp_path / "x.bin"
    blob.write_bytes(b.build_elf([CODE]))
    result = run("convert", str(blob), "--to", "wav")
    assert result.returncode == 4
    assert "not implemented" in result.stderr


def test_elf_command_describes_and_converts(tmp_path):
    blob = tmp_path / "right.sprx"
    blob.write_bytes(b.build_self(b.build_elf([CODE, RODATA])))
    result = run("elf", str(blob), "--json")
    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["container"] == "SELF"
    assert payload["self"]["entry_count"] == 3

    out = tmp_path / "right.elf"
    result = run("elf", str(blob), "-o", str(out))
    assert result.returncode == 0, result.stderr
    assert out.read_bytes()[:4] == b"\x7fELF"


def test_encrypted_package_requires_passcode(tmp_path):
    tree = {"eboot.bin": b.build_self(b.build_elf([CODE]))}
    path = tmp_path / "enc.pkg"
    path.write_bytes(b.build_pkg(b.build_pfs(tree, encrypted=True)))

    result = run("list", str(path))
    assert result.returncode == 3
    assert "passcode" in result.stderr

    from pkgmaster.crypto import compute_keys_sha3

    result = run("list", str(path), "--ekpfs", compute_keys_sha3(b.CONTENT_ID, b.PASSCODE, 1).hex())
    assert result.returncode == 0, result.stderr
    assert "eboot.bin" in result.stdout


def test_encrypted_package_wrong_passcode_is_reported(tmp_path):
    tree = {"eboot.bin": b.build_self(b.build_elf([CODE]))}
    path = tmp_path / "enc.pkg"
    path.write_bytes(b.build_pkg(b.build_pfs(tree, encrypted=True)))

    out = tmp_path / "bad.elf"
    result = run("convert", str(path), "--to", "elf", "-o", str(out), "--passcode", "f" * 32)
    assert result.returncode == 3, result.stderr
    assert "Traceback" not in result.stderr
    assert not out.exists()


def test_encrypted_self_reports_unsupported(tmp_path):
    blob = tmp_path / "locked.bin"
    blob.write_bytes(b.build_self(b.build_elf([CODE, RODATA]), encrypt_first=True))
    result = run("convert", str(blob), "--to", "elf")
    assert result.returncode == 4
    assert "encrypted" in result.stderr


def test_ps5_package_conversion(tmp_path):
    tree = {"eboot.bin": b.build_self(b.build_elf([CODE, RODATA]))}
    path = tmp_path / "ps5.pkg"
    path.write_bytes(b.build_ps5(tree))
    out = tmp_path / "eboot.elf"

    result = run("info", str(path))
    assert result.returncode == 0, result.stderr
    assert "PS5" in result.stdout

    result = run("convert", str(path), "--to", "elf", "-o", str(out))
    assert result.returncode == 0, result.stderr
    info = parse_elf(out.read_bytes())
    assert any(
        out.read_bytes()[p.p_offset : p.p_offset + p.p_filesz] == CODE
        for p in info.load_segments()
    )
