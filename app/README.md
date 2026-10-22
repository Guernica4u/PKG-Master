# PKG Master desktop app

The Rust source of the PKG Master window app (`pkgmaster-gui.exe`) and its
small command-line companion (`pkgmaster-cli.exe`). See the
[main README](../README.md) for what it does and how to use it.

```bash
cargo build --release   # target/release/pkgmaster-gui.exe, pkgmaster-cli.exe
cargo test
python ../app/tests/parity.py  # needs the Python package installed; Rust vs Python, byte for byte
```

| File | Responsibility |
| --- | --- |
| `src/main.rs` | the window (egui) |
| `src/convert.rs` | loading packages, folders and files; Convert, Extract |
| `src/anyps5.rs` | AnyPS5 settings, relinker input preparation and runs |
| `src/download.rs` | GitHub release lookups, downloads (SHA-256 checked, cancellable) and unzipping |
| `src/update.rs` | update check and Update now |
| `src/library.rs` | the Library (saved games and their icons) |
| `src/rpcs3.rs` | RPCS3 Download for me and the PS3 system software check |
| `src/history.rs` | the History record, backups and Revert |
| `src/pkg.rs`, `src/pfs.rs`, `src/crypto.rs` | PS4/PS5 containers, the PFS filesystem, XTS decryption |
| `src/elf.rs` | ELF parsing and SELF → ELF rebuilding |
| `src/bin/cli.rs` | `pkgmaster-cli` |
| `assets/icon.png`, `assets/icon.ico`, `build.rs` | the app icon (window and .exe) |
