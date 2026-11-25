# PS5toPC

Play your own PS5 game dumps on PC. Turn `eboot.bin` into a normal ELF, then
run the game with **AnyPS5** or **KytyPS5**. PS4 dumps work too: decrypt them
with shadPKG and convert their `eboot.bin`.

**[Download](https://github.com/Guernica4u/PKG-Master/releases/latest)** and run
it. Nothing to install.

- **Windows:** unzip, run `ps5topc-gui.exe`. It isn't code-signed, so Windows
  may warn you: click **More info → Run anyway**.
- **Linux:** `chmod +x` the AppImage and run it, or unpack the tar.gz.
- **Just the converter:** `python eboot2elf.py eboot.bin`. One file, no installs.

## How it works

Drop a file on the window:

| File | What you get |
| --- | --- |
| `eboot.bin` (PS5 or PS4) | Convert to ELF |
| PS5 game folder | Run the game |
| PS4 `.pkg` | Run shadPKG to decrypt and extract it |
| Fake / debug `.pkg` | Type its passcode (often 32 zeros), then convert or extract |

The first time you open it, you pick what runs your games. You can change it
later in **☰ → Settings**:

- **[AnyPS5](https://github.com/boykopovar/AnyPS5)** turns the game into a
  real Windows program. It's early, so only a few games work.
- **[KytyPS5](https://github.com/KytyPS5/KytyPS5)** is a PS5 emulator. It runs
  on Windows and Linux.

Neither one comes with the app. PS5toPC downloads the official release the
first time you need it. For [shadPKG](https://github.com/seregonwar/ShadPKG/releases),
grab `shadPKG.exe` yourself and drop it in the `shadpkg` folder.

## Good to know

- **Library:** pick your games folder on first launch (or in Settings) and your games show up on the home page. Open one and its menu music and background play, like on the console.

- **Uninstall** from the **☰** menu, not by deleting the folder, or your
  settings and downloads get left behind.
- **Windows says it's a virus?** It's a false alarm because the app is new and
  unsigned. In Windows Security, open **Protection history** and pick **Allow**.
- **Command line:** `ps5topc-cli convert eboot.bin eboot.elf`, or drop a file on `ps5topc-cli.exe`.
- **Other consoles?** That's [PKG Master](https://github.com/Guernica4u/PKG-Master), in the same release.

## Build

```bash
cd app
cargo build --release
cargo test
```

## License

[MIT](LICENSE). No Sony keys included. AnyPS5, KytyPS5 and shadPKG are
separate projects with their own licenses.
