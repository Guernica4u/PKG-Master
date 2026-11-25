# PS5toPC desktop app

The Rust source of the PS5toPC window app (`ps5topc-gui.exe`) and its
small command-line companion (`ps5topc-cli.exe`). See the
[main README](../README.md) for what it does and how to use it.

```bash
cargo build --release   # target/release/ps5topc-gui.exe, ps5topc-cli.exe
cargo test
```

| File | Responsibility |
| --- | --- |
| `src/main.rs` | the window (egui) |
| `src/convert.rs` | loading packages, folders and files; Convert, Extract |
| `src/anyps5.rs` | AnyPS5 settings, relinker input preparation and runs |
| `src/download.rs` | GitHub release lookups, downloads (SHA-256 checked, cancellable) and unzipping |
| `src/update.rs` | update check and Update now |
| `src/library.rs` | the Library (saved games and their icons) |
| `src/kyty.rs` | KytyPS5 Download for me |
| `src/history.rs` | the History record, backups and Revert |
| `src/pkg.rs`, `src/pfs.rs`, `src/crypto.rs` | PS4/PS5 containers, the PFS filesystem, XTS decryption |
| `src/elf.rs` | ELF parsing and SELF → ELF rebuilding |
| `src/bin/cli.rs` | `ps5topc-cli` |
| `assets/icon.png`, `assets/icon.ico`, `build.rs` | the app icon (window and .exe) |
