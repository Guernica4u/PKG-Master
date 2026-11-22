# PKG Master

A toolbox for game files you dumped yourself. Open PS4/PS5 packages, turn
`eboot.bin` into a normal ELF, and run games in emulators.

> **Only here for PS5 games?** Use [PS5toPC](https://github.com/Guernica4u/PS5toPC)
> instead. It's this app trimmed down to the PS5 stuff, plus KytyPS5.
> PKG Master offers to switch you over when it starts.

**[Download](https://github.com/Guernica4u/PKG-Master/releases/latest)** and
run it. Nothing to install.

- **Windows:** unzip, run `pkgmaster-gui.exe`. It isn't code-signed, so
  Windows may warn you: click **More info → Run anyway**.
- **Linux:** `chmod +x` the AppImage and run it, or unpack the tar.gz.
- **Just the converter:** `python eboot2elf.py eboot.bin`. One file, no installs.

## What each file does

Drop it on the window:

| File | What you get |
| --- | --- |
| `eboot.bin` | Convert to ELF |
| PS5 game folder | Run in AnyPS5, then Launch |
| PS4 `.pkg` | Run shadPKG to decrypt and extract it |
| Fake / debug `.pkg` | Type its passcode (often 32 zeros), then convert or extract |
| PS3 folder or `.iso` | Run in RPCS3 |

None of the emulators come with the app. When you need one, PKG Master
downloads the official release for you: [AnyPS5](https://github.com/boykopovar/AnyPS5),
[RPCS3](https://rpcs3.net). For [shadPKG](https://github.com/seregonwar/ShadPKG/releases),
grab `shadPKG.exe` yourself and drop it in the `shadpkg` folder. RPCS3 also
needs the free [PS3 system software](https://www.playstation.com/en-us/support/hardware/ps3/system-software/).

## Good to know

- **Library:** pick your games folder on first launch (or in Settings) and your games show up on the home page. Open one and its menu music and background play, like on the console.

- **Uninstall** from the **☰** menu, not by deleting the folder, or your
  settings and downloads get left behind.
- **Windows says it's a virus?** It's a false alarm because the app is new and
  unsigned. In Windows Security, open **Protection history** and pick **Allow**.
- **Command line:** `pkgmaster-cli convert eboot.bin eboot.elf`, or drop a file on `pkgmaster-cli.exe`.

## Build

```bash
cd app
cargo build --release
cargo test
```

## License

[MIT](LICENSE). No Sony keys included. AnyPS5, RPCS3 and shadPKG are separate
projects with their own licenses.
