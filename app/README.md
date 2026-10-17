# PKG Master (desktop app)

A small Windows app, written in Rust, for PS4/PS5 `.pkg` files you dumped
legally yourself:

- **Open file…** (or drag a `.pkg` onto the window or onto the `.exe`) shows
  the package's format, content ID and the files inside it.
- **Convert to ELF…** rebuilds the selected executable (by default
  `eboot.bin`) from its SELF wrapper into a standard ELF64 file.
- **Extract all files…** copies everything inside the package into a folder.
- **Open folder…** opens a decrypted game folder you dumped yourself (the one
  holding `eboot.bin`, `sce_module/` and `sce_sys/`).
- **Export for AnyPS5…** lays a package or game folder out for
  [AnyPS5](https://github.com/boykopovar/AnyPS5), the PS5-to-PC relinker:
  `source/eboot.elf`, `source/sce_module/` (every module converted to ELF,
  same file names), `app0/` (all original files) and a `HOW-TO-RUN.txt` with
  the next steps. A module that can't be converted (for example, one with
  encrypted segments) is listed and left out; no broken file is written.
  AnyPS5 itself isn't included.
- **Passcode** is only needed for encrypted fake-signed or debug packages.

It also opens standalone SELF/ELF files (`eboot.bin`, `.sprx`, `.prx`).

## Limits

- Retail packages from the PlayStation Store can't be opened. They need
  console keys, and this tool doesn't include any.
- An `eboot.bin` whose segments are still encrypted (normal for retail)
  is refused instead of written out as a broken ELF.
- Files compressed with PFSC are supported.

## Build

Needs Rust (stable, MSVC toolchain on Windows).

```bash
cd app
cargo build --release
```

This produces `target/release/pkgmaster-gui.exe` (the app) and
`target/release/pkgmaster-cli.exe` (a minimal command-line front end:
`info`, `list`, `convert FILE OUT`, `extract FILE DIR`,
`export FILE|FOLDER DIR`, with
`--passcode P` and `--path P`).

## Checking it against the Python version

The Rust core is a port of the Python `pkgmaster` package in the repository
root. `tests/parity.py` builds synthetic packages (plain, encrypted, PS5,
PFSC-compressed) and checks that both versions produce byte-identical ELF
and extracted files, and the same exit codes for bad input. It also runs
`export` on packages with an `sce_module/` folder, and on a folder extracted
from one, and checks the layout against Python's own conversions.
`cargo test` covers the folder export and the offset/vaddr alignment that
AnyPS5 needs:

```bash
cargo build && cargo test
python app/tests/parity.py
```

Run it from the repository root with the Python package installed
(`pip install -e .`).
