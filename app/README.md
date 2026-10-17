# PKG Master (desktop app)

A small Windows app, written in Rust, for PS4/PS5 `.pkg` files you dumped
legally yourself:

- **Open file…** (or drag a `.pkg` onto the window or onto the `.exe`) shows
  the package's format, content ID and the files inside it.
- **Convert to ELF…** rebuilds the selected executable (by default
  `eboot.bin`) from its SELF wrapper into a standard ELF64 file.
- **Extract all files…** copies everything inside the package into a folder.
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
`info`, `list`, `convert FILE OUT`, `extract FILE DIR`, with
`--passcode P` and `--path P`).

## Checking it against the Python version

The Rust core is a port of the Python `pkgmaster` package in the repository
root. `tests/parity.py` builds synthetic packages (plain, encrypted, PS5,
PFSC-compressed) and checks that both versions produce byte-identical ELF
and extracted files, and the same exit codes for bad input:

```bash
cargo build
python app/tests/parity.py
```

Run it from the repository root with the Python package installed
(`pip install -e .`).
