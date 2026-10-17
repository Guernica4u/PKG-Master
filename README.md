# pkgmaster

Inspect PS4/PS5 `.pkg` packages and convert the files inside them to standard
formats. The first supported target is **ELF** — extraction of `eboot.bin`
(and other modules such as `right.sprx`) out of the package and out of its
SELF container, producing a normal ELF64 image that `readelf`, `objdump`,
Ghidra, IDA and friends can open.

```
pkgmaster convert game.pkg --to elf -o eboot.elf
```

## Status

| Capability | State |
| --- | --- |
| PS4 `\x7FCNT` header + entry table | works |
| PS5 `\x7FFIH` finalized image + embedded container | works |
| Entry names, `param.sfo`, icons, metadata entries | works (encrypted entries need a passcode) |
| PFS filesystem: unsigned `0xA8`, signed-32 `0x2C8`, signed-64 `0x310` inodes | works |
| Encrypted PFS (AES-XTS, passcode/EKPFS derived keys) | works |
| PS5 outer PFS → inner `pfs_image.dat` | works |
| SELF → standard ELF | works (unencrypted/uncompressed segments) |
| Extract package files to a folder | works |
| Compressed file data (PFSC) | not implemented — reported, never silently wrong |
| Retail package decryption | **not possible and not attempted** (see below) |

## Install

```bash
python -m venv .venv
.venv/bin/pip install -e ".[dev]"      # or: .venv/bin/pip install cryptography
```

Requires Python 3.10+ and `cryptography`. The only entry point is the
`pkgmaster` command (or `python -m pkgmaster`).

## Try it without a real package

Real packages are large and often encrypted, so the tool ships a generator for
structurally valid synthetic ones:

```bash
python -m pkgmaster.synth demo.pkg                    # PS4 fake package
python -m pkgmaster.synth demo.pkg --encrypted        # encrypted PFS
python -m pkgmaster.synth demo.pkg --ps5              # PS5 FIH image

pkgmaster info demo.pkg
pkgmaster convert demo.pkg --to elf -o eboot.elf
```

These are synthetic fixtures, not real games: they exercise the format parsing,
not any copyrighted content.

## Commands

```bash
pkgmaster info    game.pkg [--json]                    # header, content id, PFS summary
pkgmaster entries game.pkg [--json]                    # metadata entries in the container
pkgmaster list    game.pkg [--dirs] [--pattern linux]  # files inside the PFS
pkgmaster tree    game.pkg                             # filesystem tree
pkgmaster extract game.pkg -o out/ [--path sce_sys]    # write files to disk
pkgmaster convert game.pkg --to elf [-o eboot.elf]     # convert content (default: eboot.bin)
pkgmaster elf     eboot.bin [-o eboot.elf]             # inspect/convert a standalone module
```

Key options, accepted by every command that reads content:

| Option | Meaning |
| --- | --- |
| `--passcode P` | 32-character FPKG passcode (or set `PKGMASTER_PASSCODE`) |
| `--ekpfs HEX` | explicit 32-byte EKPFS key as 64 hex characters |
| `--new-crypt` | force the alternate PS5-era key schedule |

Example session on a synthetic PS4 package:

```
$ pkgmaster info demo.pkg
file            demo.pkg
format          PS4
content id      EP0000-CUSA00000_00-0000000000000000
content type    GD (game data)
pfs image       0x2000 (576.0 KiB)
entries         2
pfs             v1, block 0x10000, 7 inodes
pfs root        5 entries

$ pkgmaster convert demo.pkg --to elf -o eboot.elf
source          eboot.bin
output          eboot.elf (4.1 KiB)
elf             ET_SCE_EXEC_ASLR / EM_X86_64 / FreeBSD
entry point     0x1010
segments        3 copied, 0 zero-filled

$ file eboot.elf
eboot.elf: ELF 64-bit LSB x86-64, version 1 (FreeBSD)
```

## How it works

```
                 ┌──────────────────────────── PS4 ───────────────────────────┐
  game.pkg  ───► │ \x7FCNT header → entry table → names / param.sfo / icons   │
                 │ pfs_image_offset+size ─► PFS image ─► uroot ─► eboot.bin   │
                 └───────────────────────────────────────────────────────────┘
                 ┌──────────────────────────── PS5 ───────────────────────────┐
  game.pkg  ───► │ \x7FFIH header → outer PFS → pfs_image.dat → inner PFS     │
                 │ embedded \x7FCNT → metadata entries                        │
                 └───────────────────────────────────────────────────────────┘
                                        │
                    PFS file (eboot.bin / *.sprx / *.prx)
                                        ▼
        SELF container? ── yes ─► map segment payloads onto ELF program headers
                        └ no  ──► already an ELF, pass through unchanged
                                        ▼
                                   eboot.elf
```

Module map:

| File | Responsibility |
| --- | --- |
| `pkgmaster/binary.py` | positional, bounds-checked readers; nested windows |
| `pkgmaster/crypto.py` | passcode→key derivation, PFS XTS key schedule, XTS reader |
| `pkgmaster/pkg.py` | PS4 CNT and PS5 FIH containers, entry table, entry decryption |
| `pkgmaster/pfs.py` | PFS superblock, the three inode layouts, dirents, file reads |
| `pkgmaster/elf.py` | ELF header/program header parsing, SCE type names |
| `pkgmaster/self.py` | SELF container parsing, ELF reconstruction |
| `pkgmaster/convert.py` | content discovery + the `CONVERTERS` registry |
| `pkgmaster/cli.py` | argparse front end and exit codes |
| `pkgmaster/synth.py` | synthetic package/module generator used by tests and demos |

Everything is streamed through positional reads, so a 40 GB package is never
loaded into memory — only the bytes actually requested are touched.

Exit codes: `0` success, `2` bad file/usage, `3` a key is required or wrong,
`4` recognised but unimplemented.

## Keys, encryption, and what this tool will not do

* **No keys, no content keys, and no RSA private keys are bundled.**
* Fake/debug packages can be decrypted with the passcode alone: the passcode
  plus the content id derive the entry keys and the PFS EKPFS.
* **Retail packages cannot be decrypted here, by design.** Their keys come from
  console-side material that is not derivable from the package, and providing
  it would be DRM circumvention. pkgmaster reports what it needs and stops.
* Encrypted or compressed SELF segments are refused with a clear message
  instead of emitting a corrupt ELF.

Use it on packages and dumps you are entitled to work with.

## Roadmap

The conversion layer is a registry (`CONVERTERS` in `convert.py`), so the next
targets are mostly additive:

1. `param.sfo` → JSON, and `param.json` formatting.
2. `icon0.png` / DDS image passthrough and conversion.
3. `snd0.at9` → WAV.
4. PFSC (block-compressed) file and image decompression — the main gap for
   real-world packages.
5. Folder/ISO export of a whole PFS image.
6. PS5 SELF stage-2 payload handling beyond fake-self modules.

## Verified vs not

Be precise about what has been proven:

* **Verified** with synthetic fixtures and the test suite: PS4 and PS5 container
  parsing, all three PFS inode layouts, multi-block files, encrypted PFS
  round-trips (right key works, wrong key is detected), encrypted metadata
  entries, SELF→ELF reconstruction, and the full CLI surface. Converted ELFs
  were checked with GNU `readelf` and `file`.
* **Not verified against real retail packages**, because none can be shipped
  here and none can be decrypted without console keys. Field-level offsets come
  from the documented formats and were cross-checked against two independent
  working implementations.
* **Not implemented**: PFSC-compressed file data, `license.dat` decryption for
  retail key index 3, and delta-patch specific entries.

## Tests

```bash
.venv/bin/python -m pytest -q      # 41 tests
```

## References

Format details were cross-checked against
[LibOrbisPkg](https://github.com/maxton/LibOrbisPkg) (PS4 PKG/PFS, LGPL),
[orbis-pfs](https://github.com/videobitva/orbis) (Rust PFS reader),
[MkPFS](https://github.com/PSBrew/MkPFS) (PFS image builder),
[LibProsperoPKG's PS5 format write-up](https://github.com/SvenGDK/LibProsperoPKG),
and [ps4_unfself](https://github.com/SocraticBliss/ps4_unfself) for the SELF
layout. No third-party code was copied; the implementations here were written
from the documented structures.
