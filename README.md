# PKG Master

Turn a PS4/PS5 `eboot.bin` into a normal `eboot.elf`. Then run the game in
an emulator. For games **you dumped yourself**.

**[⬇ Download](https://github.com/Guernica4u/PKG-Master/releases/latest)**. Nothing to install.

- **Windows:** unzip `PKG-Master-v…-windows-x64.zip`, run `pkgmaster-gui.exe`.
  App isn't code-signed, so Windows may warn. Click **More info → Run anyway**.
  If Defender blocks it, see [Windows says it's a virus](#windows-says-its-a-virus).
- **Linux:** `chmod +x PKG-Master-v…-x86_64.AppImage` and run it. Or unpack
  `PKG-Master-v…-linux-x64.tar.gz` and run `./pkgmaster-gui`.
- **Python only:** `eboot2elf.py`. One file, no installs:
  `python eboot2elf.py eboot.bin` writes `eboot.elf` next to it.

## Drop a file, get this

| Drop | You get |
| --- | --- |
| `eboot.bin` | **Convert to ELF…** |
| PS5 game folder | **Run in AnyPS5…**, then **Launch** |
| PS4 `.pkg` | **Run shadPKG (PS4)…** to decrypt and extract it |
| Fake / debug `.pkg` | Passcode box (often 32 zeros), then convert or extract |
| PS3 folder or `.iso` | **Run in RPCS3** |

Right-click a file for **Convert to ELF** (eboot.bin only), **Show in
Explorer**, **Copy path** and **Delete** (to the Recycle Bin).

## Buttons

- **⬅ / ⟳ / History**: back, reload, and everything the app wrote, with **Revert**.
- **📚 Library**: press **☆** next to a title to save the game. Click a tile to open it.
- **☰**: Settings, data folder, About (updates), Uninstall.

## Emulators

None are included. PKG Master downloads the official release when you ask
(**Download for me**), checked against GitHub's SHA-256.

- **[AnyPS5](https://github.com/boykopovar/AnyPS5)** runs PS5 games (`PPSA…`).
  Early software: few games work. A crash within 15 seconds usually means the
  game isn't supported yet.
- **[RPCS3](https://rpcs3.net)** runs PS3 games. It needs the free
  [PS3 system software](https://www.playstation.com/en-us/support/hardware/ps3/system-software/):
  download `PS3UPDAT.PUP`, then in RPCS3 choose **File > Install Firmware**.
- **[shadPKG](https://github.com/seregonwar/ShadPKG)** handles PS4 packages.
  Download `shadPKG.exe` from its [releases](https://github.com/seregonwar/ShadPKG/releases)
  and drop it in the `shadpkg` folder next to `pkgmaster-gui.exe`.

## Uninstall

Use **☰ → Uninstall PKG Master…**. Deleting the folder by hand leaves
settings, History backups and downloads behind.

It removes settings, History, the Library, update leftovers, half-finished
downloads and shortcuts. The AnyPS5 and RPCS3 folders hold your run folders and
saves, so they stay unless you tick them. On Windows the program can't delete
itself while running: its folder opens at the end, so delete `pkgmaster-gui.exe`
there.

Data lives in `%APPDATA%\PKG Master` (Windows) or `~/.config/pkg-master` (Linux).

## Windows says it's a virus

False alarm. The app is unsigned and new, so SmartScreen and Defender don't
know it yet. Every release is built from this repo's source.

- Blue "Windows protected your PC" box: **More info → Run anyway**.
- Defender quarantined it: Windows Security → **Protection history** →
  pick the item → **Allow**. Or add the folder under **Exclusions**.

## Command line

`pkgmaster-cli` ships with the app. Drop an `eboot.bin` on it to convert it.

```bash
pkgmaster-cli convert eboot.bin eboot.elf
pkgmaster-cli info    game.pkg [--passcode P]
pkgmaster-cli list    game.pkg
pkgmaster-cli extract game.pkg out/
```

Exit codes: `0` ok, `2` bad file, `3` passcode needed or wrong, `4` not supported.

## Build

```bash
cd app
cargo build --release
cargo test
```

## License

[MIT](LICENSE). No Sony keys are included. AnyPS5, RPCS3 and shadPKG are
separate projects with their own licenses.

Formats cross-checked against [LibOrbisPkg](https://github.com/maxton/LibOrbisPkg),
[orbis-pfs](https://github.com/videobitva/orbis), [MkPFS](https://github.com/PSBrew/MkPFS),
[LibProsperoPKG](https://github.com/SvenGDK/LibProsperoPKG) and
[ps4_unfself](https://github.com/SocraticBliss/ps4_unfself). No third-party code
copied. Icon from [egui](https://github.com/emilk/egui) (MIT/Apache-2.0).
