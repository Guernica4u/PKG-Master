# PKG Master

A minimal app for PS4/PS5 game files **you dumped legally yourself**.
It supports decrypting ps4 games through ShadPKG and setups AnyPS5 for you, turns
the game's executable (eboot.bin) into a standard .ELF file, unpacks packages, and can run a
decrypted PS5 game through [AnyPS5](https://github.com/boykopovar/AnyPS5).

**[⬇ Download the latest release](https://github.com/Guernica4u/PKG-Master/releases/latest)**:
unzip it and run `pkgmaster-gui.exe`. Nothing to install.

> Windows may say "Windows protected your PC" because the app isn't
> code-signed. Click **More info → Run anyway**.

## What it does

| Button | What happens |
| --- | --- |
| **Open file…** | Open a `.pkg`, or a single `eboot.bin` / `.prx` / `.sprx` / `.elf`. You can also drag a file onto the window or onto the `.exe`. |
| **Open folder…** | Open a decrypted game folder: a PS4/PS5 dump (the one holding `eboot.bin` or `eboot.elf`, `sce_module/` and `sce_sys/`) or a PS3 dump (with `PS3_GAME/` or `PARAM.SFO` and `USRDIR/`). Other folders are listed so you can click through them: a folder with a single `.pkg` opens it, and a folder with a single subfolder steps into it. |
| **⬅ / ⟳** | Back to what you had open before (like a browser), or reload what's open. |
| **Run shadPKG (PS4)…** | Shown for an open PS4 `.pkg`: runs your own `shadpkg.exe` on it (see [shadPKG](#shadpkg)). |
| **📚 Library** | Your saved games as icon tiles. Press **☆** next to a game's title to save it; games you run in AnyPS5 or RPCS3 are added for you. Click a tile to open the game (with **Start games when you open them from the Library** on in ☰ Settings, it also starts: PS5 games with an AnyPS5 run folder run `app.exe`, PS3 games start in RPCS3); right-click for **Open game files**, **Show in Explorer** and **Remove from Library**. See [Library](#library). |
| **History** | Everything the app has written, with **Revert** and **Show in Explorer**. See [History](#history). |
| **Convert to ELF…** | Turns the selected file (by default `eboot.bin`) from Sony's SELF format into a standard ELF64, which is the file type AnyPS5 needs. Saves into a new `<name>-elf` folder next to the game files by default. |
| **Extract all files…** | Shown for an unlocked `.pkg`: unpacks everything inside it into a normal folder. |
| **Run in AnyPS5…** | For a decrypted PS5 game folder: converts the executables, runs your AnyPS5 relinker, and sets up a ready-to-run folder inside your AnyPS5 folder. **Launch** then starts the open game's own run folder, and **Copy log** copies everything it printed. If the game closes, the log folds away; if it crashes, the crash shows in red, with a warning when it happens within 15 seconds (the game may not be supported by AnyPS5). Hidden for PS4 and PS3 games, which AnyPS5 can't run. |
| **Run in RPCS3** | For a dumped PS3 game folder: starts it in your own [RPCS3](#rpcs3). |
| **☰** | **Settings…** (start games from the Library, hide the log, and where AnyPS5, RPCS3 and shadPKG are), **Open data folder** and **About PKG Master** (version and Check for updates). |
| **Passcode** | Only shown for encrypted fake/debug packages. Usually 32 zeros. |

**Right-click** any file in a list for **Convert to ELF**, **Show in
Explorer**, **Copy path** and **Delete** (which moves it to the Recycle Bin).
Files inside a `.pkg` are read-only, so those only get Convert and Copy path.

### Library

Saved games live in `%APPDATA%\PKG Master\library.json`, with a small copy of
each icon in `library-icons\`. The log under the buttons folds away when you
open a different game, and it can't be dragged over the Run / Launch buttons.

### Updates

When a new version is out, PKG Master tells you when it starts. **Update now**
downloads it from this repo's releases, checks it against GitHub's SHA-256,
replaces the program and restarts it. Your settings, history, `shadpkg` and
`AnyPS5` folders are left alone. **Skip this version** stops it asking about
that release. If the app's folder can't be written to, it points you to the
download page instead.

### History

Every Convert, Extract, shadPKG and AnyPS5 run is recorded. If something was
already at the save location, the app backs it up first. **Revert** deletes
what the app wrote and puts the old version back. A failed operation is rolled
back automatically, so it never leaves half-written files. The history is kept
between sessions in `%APPDATA%\PKG Master\`; **Clear history** deletes the
backups to free the space.

### AnyPS5

[AnyPS5](https://github.com/boykopovar/AnyPS5) turns **PS5** games (title IDs
starting with `PPSA`) into Windows programs. It isn't included. The first time
you press an AnyPS5 button, PKG Master explains how to get it, and its
**Download for me** button fetches the official release from AnyPS5's GitHub
page, checks it against GitHub's SHA-256, unpacks it into an `AnyPS5` folder
next to `pkgmaster-gui.exe` (or `%LOCALAPPDATA%\PKG Master\AnyPS5` if that
folder can't be written to) and sets it up. **Cancel** stops the download.

To set it up by hand instead:

1. Unzip AnyPS5's `prx-windows-v….zip` (the folder with `libs/`) and put its
   `relinker….exe` in the same folder.
2. In PKG Master, click **AnyPS5 folder…** (or **Run in AnyPS5…**), then
   **I have it, pick the folder…** and choose that folder. Any `.exe` with
   "relinker" in its name is found automatically.
3. Click **Run in AnyPS5…**, then **Launch**.

PS4 games (`CUSA…`) aren't supported by AnyPS5, so the AnyPS5 buttons are
hidden for them. AnyPS5 is early software and only a few games work so far.

When AnyPS5 puts out a newer release than the one you have, an **Update AnyPS5**
button appears under the AnyPS5 row. It downloads the new release into a new
folder next to your current one (your old run folders stay where they are) and
switches to it.

### RPCS3

PS3 discs are encrypted with a key for each disc, so PKG Master works from
games you dumped on your own PS3 (custom firmware copies them off the disc
already decrypted). Open the dumped folder and press **Run in RPCS3**: PKG
Master starts your [RPCS3](https://rpcs3.net) with `--no-gui` on the game's
`USRDIR/EBOOT.BIN`. RPCS3 isn't included. The first time, PKG Master offers
**Download for me**, which gets RPCS3's official Windows build from its GitHub
releases (checked against GitHub's SHA-256) into an `rpcs3` folder next to
`pkgmaster-gui.exe`, or you can pick a `rpcs3.exe` you already have.

RPCS3 also needs the PS3 system software, which PlayStation gives away on its
[PS3 system software page](https://www.playstation.com/en-us/support/hardware/ps3/system-software/).
PKG Master explains the steps when it's missing: download `PS3UPDAT.PUP`, then
in RPCS3 choose **File > Install Firmware**. Convert to ELF isn't available for
PS3 games; RPCS3 runs them as they are.

### shadPKG

The **Run shadPKG (PS4)…** button runs [shadPKG](https://github.com/seregonwar/ShadPKG),
which isn't part of PKG Master and isn't included in the download.
Download `shadPKG.exe` from its
[official releases](https://github.com/seregonwar/ShadPKG/releases) and drop
it in the empty **`shadpkg`** folder next to `pkgmaster-gui.exe`. If it's
missing, the button tells you this and links to the download.

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

## License

PKG Master is under the [MIT license](LICENSE). shadPKG and AnyPS5 are
separate projects with their own licenses; neither is included.

## Credits

Format details were cross-checked against
[LibOrbisPkg](https://github.com/maxton/LibOrbisPkg),
[orbis-pfs](https://github.com/videobitva/orbis),
[MkPFS](https://github.com/PSBrew/MkPFS),
[LibProsperoPKG](https://github.com/SvenGDK/LibProsperoPKG) and
[ps4_unfself](https://github.com/SocraticBliss/ps4_unfself). No third-party
code was copied. The app icon is egui's logo, from
[egui/eframe](https://github.com/emilk/egui) (MIT/Apache-2.0). AnyPS5 is by
[boykopovar and contributors](https://github.com/boykopovar/AnyPS5).
