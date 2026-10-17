# PKG Master

A small Windows app for PS4/PS5 game files **you dumped legally yourself**.
It opens fake-signed and debug `.pkg` files and decrypted game folders, turns
the game's executable into a standard ELF, unpacks packages, and can run a
decrypted PS5 game through [AnyPS5](https://github.com/boykopovar/AnyPS5).

**[⬇ Download the latest release](https://github.com/Guernica4u/PKG-Master/releases/latest)**:
unzip it and run `pkgmaster-gui.exe`. Nothing to install.

> Windows may say "Windows protected your PC" because the app isn't
> code-signed. Click **More info → Run anyway**.

## What it does

| Button | What happens |
| --- | --- |
| **Open file…** | Open a `.pkg`, or a single `eboot.bin` / `.prx` / `.sprx` / `.elf`. You can also drag a file onto the window or onto the `.exe`. |
| **Open folder…** | Open a decrypted game folder (the one holding `eboot.bin` or `eboot.elf`, `sce_module/` and `sce_sys/`). Other folders are listed so you can click through them: a folder with a single `.pkg` opens it, and a folder with a single subfolder steps into it. |
| **Run shadPKG (PS4)…** | Runs your own `shadpkg.exe` on a PS4 `.pkg` (see [shadPKG](#shadpkg)). |
| **Back / Refresh** | Go up one folder, or reload what's open. |
| **History** | Everything the app has written, with **Revert** and **Show in Explorer**. See [History](#history). |
| **Convert to ELF…** | Turns the selected file (by default `eboot.bin`) from Sony's SELF format into a standard ELF64, which is the file type AnyPS5 needs. Saves into a new `<name>-elf` folder next to the game files by default. |
| **Extract all files…** | Unpacks everything inside a `.pkg` into a normal folder. |
| **Run in AnyPS5…** | For a decrypted PS5 game folder: converts the executables, runs your AnyPS5 relinker, and sets up a ready-to-run folder inside your AnyPS5 folder. **Launch** then starts it. |
| **Passcode** | Only shown for encrypted fake/debug packages. Usually 32 zeros. |

**Right-click** any file in a list for **Convert to ELF**, **Show in
Explorer**, **Copy path** and **Delete** (which moves it to the Recycle Bin).
Files inside a `.pkg` are read-only, so those only get Convert and Copy path.

### History

Every Convert, Extract, shadPKG and AnyPS5 run is recorded. If something was
already at the save location, the app backs it up first. **Revert** deletes
what the app wrote and puts the old version back. A failed operation is rolled
back automatically, so it never leaves half-written files. The history is kept
between sessions in `%APPDATA%\PKG Master\`; **Clear history** deletes the
backups to free the space.

### AnyPS5

[AnyPS5](https://github.com/boykopovar/AnyPS5) turns **PS5** games (title IDs
starting with `PPSA`) into Windows programs. It isn't included, so download
its release yourself:

1. Unzip AnyPS5's `prx-windows-v….zip` (the folder with `libs/`) and put its
   `relinker….exe` in the same folder.
2. In PKG Master, open your decrypted PS5 game folder and click
   **AnyPS5 folder…** once to point at that folder. Any `.exe` with
   "relinker" in its name is found automatically.
3. Click **Run in AnyPS5…**, then **Launch**.

PS4 games (`CUSA…`) aren't supported by AnyPS5; the app warns you before
starting. AnyPS5 is early software and only a few games work so far.

### shadPKG

The **Run shadPKG (PS4)…** button runs a `shadpkg.exe` that you provide. Put
it next to `pkgmaster-gui.exe` (or in a `shad\` folder beside it), or the app
asks where it is. shadPKG isn't part of PKG Master and isn't included in the
download.

## What it won't do

- **No Sony keys are included, and retail packages can't be decrypted.**
  Packages from the PlayStation Store or a disc are encrypted with console
  keys. The legal route is to dump the game on your own console, which writes
  out the files already decrypted; PKG Master works from there.
- Encrypted or broken input fails with a clear message. The app never writes
  a plausible-looking but wrong file.

Use it only on games you own and are allowed to dump where you live.

## Building from source

The app is Rust (in [`app/`](app/)); the original command-line tool is
Python (in [`pkgmaster/`](pkgmaster/)). Both read the same formats and produce
byte-identical output, which [`app/tests/parity.py`](app/tests/parity.py)
checks.

```bash
# Desktop app (needs Rust, MSVC toolchain on Windows)
cd app
cargo build --release          # target/release/pkgmaster-gui.exe and pkgmaster-cli.exe
cargo test

# Python tool
python -m venv .venv
.venv/Scripts/pip install -e ".[dev]"
.venv/Scripts/python -m pytest -q
python app/tests/parity.py     # Rust vs Python, byte for byte
```

### Command line

`pkgmaster-cli.exe` (in the release zip) covers the basics:

```bash
pkgmaster-cli info    game.pkg [--passcode P]
pkgmaster-cli list    game.pkg
pkgmaster-cli convert game.pkg eboot.elf [--path sce_module/libc.prx]
pkgmaster-cli extract game.pkg out/
```

The Python tool (`pip install -e .`, then `pkgmaster --help`) has more:
`info`, `entries`, `list`, `tree`, `extract`, `convert` and `elf`, with
`--json` output. Exit codes: `0` ok, `2` bad file, `3` passcode needed or
wrong, `4` not supported yet.

### Formats supported

| | |
| --- | --- |
| PS4 `\x7FCNT` and PS5 `\x7FFIH` packages | yes |
| PFS filesystem, all three inode layouts | yes |
| Passcode-encrypted PFS (AES-XTS) | yes |
| PFSC-compressed files | yes |
| SELF → ELF (plaintext segments) | yes |
| Retail decryption | no, by design |

Format details are documented in [`HANDOFF.md`](HANDOFF.md).

## Credits

Format details were cross-checked against
[LibOrbisPkg](https://github.com/maxton/LibOrbisPkg),
[orbis-pfs](https://github.com/videobitva/orbis),
[MkPFS](https://github.com/PSBrew/MkPFS),
[LibProsperoPKG](https://github.com/SvenGDK/LibProsperoPKG) and
[ps4_unfself](https://github.com/SocraticBliss/ps4_unfself). No third-party
code was copied. AnyPS5 is by
[boykopovar and contributors](https://github.com/boykopovar/AnyPS5).
