# PKG Master

A toolbox for game files you dumped yourself. One app for PS1 to PS5, PSP,
PS Vita, GameCube, Wii and Xbox 360: open packages, turn `eboot.bin` into a
normal ELF, and play games in the right emulator.

> **Only here for PS5 or PS4 games?** Grab the `PS5toPC` download from the
> same release instead. It's the PS5/PS4 part of this app, plus KytyPS5. PKG
> Master offers to swap you over when it starts. Its source is in [PS5toPC/](PS5toPC).

**[Download](https://github.com/Guernica4u/PKG-Master/releases/latest)** and
run it. Nothing to install.

- **Windows:** unzip, run `pkgmaster-gui.exe`. It isn't code-signed, so
  Windows may warn you: click **More info → Run anyway**.
- **Linux:** `chmod +x` the AppImage and run it, or unpack the tar.gz.
- **Just the converter:** `python eboot2elf.py eboot.bin`. One file, no installs.

## What each file does

Drop it on the window, or pick your games folder and use the Library:

| File | What you get |
| --- | --- |
| `eboot.bin` | Convert to ELF |
| PS5 game folder | Run in AnyPS5, then Launch |
| PS4 game folder | Run in shadPS4 |
| PS4 `.pkg` | Run shadPKG to decrypt and extract it |
| Fake / debug `.pkg` | Type its passcode (often 32 zeros), then convert or extract |
| PS3 game folder | Run in RPCS3 |
| PS2 `.iso` / `.chd` | Run in PCSX2 |
| PS1 `.cue` / `.bin` / `.chd` | Run in DuckStation |
| PSP `.iso` / `.cso` | Run in PPSSPP |
| PS Vita `.vpk` | Run in Vita3K |
| GameCube / Wii `.iso` `.rvz` `.wbfs` | Run in Dolphin |
| Xbox 360 `.iso` / `.zar` / game folder | Run in Xenia Canary |

None of the emulators come with the app. The first time you need one, PKG
Master downloads its official release for you (or you point it at your own).
For [shadPKG](https://github.com/seregonwar/ShadPKG/releases), grab
`shadPKG.exe` yourself and drop it in the `shadpkg` folder. RPCS3 also needs
the free [PS3 system software](https://www.playstation.com/en-us/support/hardware/ps3/system-software/).

## Good to know

- **Library:** pick your games folder on first launch (or in Settings) and your
  games show up on the home page. Open a PS4/PS5 game and its menu music and
  background play, like on the console.
- **Uninstall** from the **☰** menu, not by deleting the folder, or your
  settings and downloads get left behind.
- **Windows says it's a virus?** It's a false alarm because the app is new and
  unsigned. In Windows Security, open **Protection history** and pick **Allow**.
- **Command line:** `pkgmaster-cli convert eboot.bin eboot.elf`, or drop a file on `pkgmaster-cli.exe`.
- **Was called ROM Master for a while.** Those installs update to PKG Master on their own.

## Build

```bash
cd app            # or PS5toPC/app
cargo build --release
cargo test
```

## License

[MIT](LICENSE). No Sony keys included. The emulators and shadPKG are separate
projects with their own licenses. The menu music decoder is a fixed copy of
[atrac9dec](https://github.com/claimore22/atrac9dec) (MIT) in `app/vendor`.
