"""Check the Rust core produces byte-identical output to the Python pkgmaster.

Builds synthetic packages with ``pkgmaster.synth``, then converts and extracts
each one with both implementations and compares the results. Run from the
repository root after ``cargo build`` in ``app/``:

    python app/tests/parity.py [path/to/pkgmaster-cli]

Compressed (PFSC) cases run only when the importable ``pkgmaster.synth`` has
``Compressed``. Exits non-zero on any mismatch.
"""

from __future__ import annotations

import filecmp
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from pkgmaster import synth as s
from pkgmaster.cli import main as py_main

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "debug" / ("pkgmaster-cli.exe" if os.name == "nt" else "pkgmaster-cli")


def cases() -> dict[str, bytes]:
    out = {
        "ps4": s.build_pkg(s.build_pfs(s.DEFAULT_TREE)),
        "ps4-encrypted": s.build_pkg(
            s.build_pfs(
                s.DEFAULT_TREE,
                encrypted=True,
                ekpfs=s.compute_keys_sha3(s.CONTENT_ID, s.PASSCODE, 1),
            )
        ),
        "ps5": s.build_ps5(s.DEFAULT_TREE),
    }
    if hasattr(s, "Compressed"):
        tree = s.compressed_tree()
        raw_deflate = dict(s.DEFAULT_TREE)
        raw_deflate["eboot.bin"] = s.Compressed(tree["eboot.bin"].data, zlib_header=False)
        out.update(
            {
                "ps4-pfsc": s.build_pkg(s.build_pfs(tree)),
                "ps4-pfsc-raw-deflate": s.build_pkg(s.build_pfs(raw_deflate)),
                "ps4-pfsc-encrypted": s.build_pkg(
                    s.build_pfs(
                        tree,
                        encrypted=True,
                        ekpfs=s.compute_keys_sha3(s.CONTENT_ID, s.PASSCODE, 1),
                    )
                ),
                "ps5-pfsc": s.build_ps5(tree),
                "ps5-pfsc-inner": s.build_ps5(s.DEFAULT_TREE, compress_inner=True),
            }
        )
    return out


def run_rust(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([str(EXE), *args], capture_output=True, text=True)


def run_py(*args: str) -> int:
    from contextlib import redirect_stderr, redirect_stdout
    import io

    with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
        return py_main(list(args))


def same_tree(a: Path, b: Path) -> bool:
    cmp = filecmp.dircmp(a, b)
    if cmp.left_only or cmp.right_only or cmp.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(a, b, cmp.common_files, shallow=False)
    if mismatch or errors:
        return False
    return all(same_tree(a / d, b / d) for d in cmp.common_dirs)


def main() -> int:
    global EXE
    if len(sys.argv) > 1:
        EXE = Path(sys.argv[1])
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for name, data in cases().items():
            pkg = tmp / f"{name}.pkg"
            pkg.write_bytes(data)
            key = ["--passcode", s.PASSCODE] if "encrypted" in name else []

            py_elf, rs_elf = tmp / f"{name}.py.elf", tmp / f"{name}.rs.elf"
            py_code = run_py("convert", str(pkg), "--to", "elf", "-o", str(py_elf), "--quiet", *key)
            rs = run_rust("convert", str(pkg), str(rs_elf), *key)
            ok = py_code == 0 and rs.returncode == 0 and py_elf.read_bytes() == rs_elf.read_bytes()

            py_dir, rs_dir = tmp / f"{name}.py", tmp / f"{name}.rs"
            py_code = run_py("extract", str(pkg), "-o", str(py_dir), "--quiet", *key)
            rs_x = run_rust("extract", str(pkg), str(rs_dir), *key)
            ok_x = py_code == 0 and rs_x.returncode == 0 and same_tree(py_dir, rs_dir)

            print(f"{'ok  ' if ok and ok_x else 'FAIL'}  {name:24} convert={'ok' if ok else 'MISMATCH'} extract={'ok' if ok_x else 'MISMATCH'}")
            if not (ok and ok_x):
                failures += 1
                print("      rust:", rs.stderr.strip(), rs_x.stderr.strip())

        # Error handling must match too: no key, wrong key, junk input.
        enc = tmp / "ps4-encrypted.pkg"
        junk = tmp / "junk.bin"
        junk.write_bytes(b"not a package at all")
        for label, args, want in [
            ("no passcode", [str(enc), str(tmp / "x.elf")], 3),
            ("wrong passcode", [str(enc), str(tmp / "x.elf"), "--passcode", "f" * 32], 3),
            ("junk input", [str(junk), str(tmp / "x.elf")], 2),
        ]:
            r = run_rust("convert", *args)
            good = r.returncode == want and not (tmp / "x.elf").exists()
            print(f"{'ok  ' if good else 'FAIL'}  {label:24} exit={r.returncode} (want {want})")
            failures += 0 if good else 1
    print("all cases match" if not failures else f"{failures} case(s) failed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
