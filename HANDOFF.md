# HANDOFF — pkgmaster (PS4/PS5 PKG → ELF converter)

Written for whoever continues this work. Read this before touching the code.

## 1. Where things stand

`pkgmaster` is a working Python tool that opens PS4 `.pkg` and PS5 `.pkg`
(finalized-image) files, reads the PFS filesystem inside them, and converts
`eboot.bin` / other modules from SELF containers into standard ELF64 files.

**Important: nothing is committed or pushed.** All work is uncommitted
working-tree changes in the branch `freebuff/3db855ed1d40f92a1efad83c`:

```
 M README.md          # rewritten from a one-line joke into real docs
?? .gitignore
?? HANDOFF.md         # this file
?? pkg2elf.py         # standalone single-file PKG -> ELF script (no package needed)
?? pkgmaster/         # the package (8 modules, ~2.9k lines)
?? pyproject.toml
?? tests/             # 41 tests + synth fixtures
```

`pkg2elf.py` is a single-file mirror of the PKG -> PFS -> SELF -> ELF path for
people who just want to run a script: `python pkg2elf.py game.pkg -o eboot.elf`
(`--info`, `--list`, `--passcode`, `--ekpfs`, `--path`). It is verified to
produce **byte-identical** output to the library for PS4, encrypted-PS4, PS5 and
raw-SELF inputs. Treat `pkgmaster/` as the source of truth and keep the two in
step if you change the pipeline.

`HEAD` is still `2493ff9`, the repo's original commit. The user's GitHub repo
and local clone are untouched. To transfer: commit + push this branch (needs
the user's go-ahead), or copy the files.

## 2. Verify it before trusting it

```bash
python -m venv .venv
.venv/bin/pip install -e ".[dev]"      # needs cryptography; pytest for tests
.venv/bin/python -m pytest -q          # expect: 89 passed

# no real samples exist; generate structurally valid synthetic ones
.venv/bin/python -m pkgmaster.synth demo.pkg
.venv/bin/python -m pkgmaster.synth enc.pkg --encrypted   # AES-XTS PFS
.venv/bin/python -m pkgmaster.synth demo.pkg --ps5        # PS5 FIH image
.venv/bin/python -m pkgmaster.synth demo.pkg --compressed # PFSC eboot.bin

.venv/bin/pkgmaster info demo.pkg
.venv/bin/pkgmaster convert demo.pkg --to elf -o eboot.elf
file eboot.elf && readelf -h eboot.elf       # independent sanity check

# single-file script, works without installing the package:
.venv/bin/python pkg2elf.py demo.pkg -o eboot.elf
.venv/bin/python pkg2elf.py demo.pkg --info
```

Last verified state: 89/89 tests pass; installed console script produces ELFs
that GNU `readelf`/`file` accept (ELF64, LSB, FreeBSD OS/ABI, 56-byte phdrs,
correct entry point) for plaintext PS4, encrypted PS4, and PS5 packages.

## 3. CLI surface

```
pkgmaster info    FILE [--json]
pkgmaster entries FILE [--json]
pkgmaster list    FILE [--dirs] [--pattern P]
pkgmaster tree    FILE
pkgmaster extract FILE -o DIR [--path P]
pkgmaster convert FILE --to elf [-o OUT] [--path P] [--strip-sce]
pkgmaster elf     FILE [-o OUT]
```

Shared key options: `--passcode P` (32 chars, or `PKGMASTER_PASSCODE`),
`--ekpfs HEX` (32 bytes as 64 hex), `--new-crypt`.
Exit codes: `0` ok, `2` bad file/usage, `3` key required/wrong, `4` recognised
but unimplemented.

## 4. Code map

| File | Responsibility |
| --- | --- |
| `pkgmaster/binary.py` | `Reader` (positional, bounds-checked) and `WindowReader` (nested sub-windows) |
| `pkgmaster/crypto.py` | passcode→key derivation, PFS XTS key schedule, `XtsDecryptReader` |
| `pkgmaster/pkg.py` | PS4 CNT + PS5 FIH containers, entry table, names, entry decryption |
| `pkgmaster/pfs.py` | superblock, three inode layouts, dirents, block maps, file reads |
| `pkgmaster/elf.py` | ELF header/program-header parsing, SCE type name tables |
| `pkgmaster/self.py` | SELF parsing + ELF reconstruction |
| `pkgmaster/convert.py` | content discovery and the `CONVERTERS` registry |
| `pkgmaster/cli.py` | argparse front end |
| `pkgmaster/synth.py` | synthetic package/module generator (also the test fixtures) |

All I/O is positional streaming — a 40 GB package is never loaded into memory.

## 5. Format facts (the expensive part — do not re-derive)

### PS4 container `\x7FCNT` (big-endian header)

| Offset | Field |
| --- | --- |
| `0x00` | magic `7F 43 4E 54` |
| `0x10` | entry count (u32) |
| `0x14` / `0x16` | sc entry count (u16) / duplicate count (u16) |
| `0x18` / `0x1C` | entry table offset (u32, absolute) / main entry data size |
| `0x20` / `0x28` | body offset / body size (u64) |
| `0x40` | content id, 48-byte field, 36 ASCII chars + NUL padding |
| `0x70` / `0x74` | drm type (0 none, 0xF PS4) / content type (0x1A GD, 0x1B AC, 0x1C AL, 0x1E DP) |
| `0x404` / `0x408` / `0x410` / `0x418` | pfs image count / pfs flags (u64) / pfs image offset / size (u64) |
| `0x430` | package size |

Header struct is `0x5A0` bytes; digest at `0xFE0`, signature at `0x1000`.

Entry table records are exactly `0x20` bytes, big-endian:
`id(u32), name_offset(u32), flags1(u32), flags2(u32), data_offset(u32), data_size(u32), pad(8)`.
- `flags1 & 0x80000000` → entry payload is encrypted.
- key index = `(flags2 & 0xF000) >> 12`.
- name table is the entry with id `0x0200`; names are NUL-terminated and
  concatenated starting with an empty string, so the first real name is at
  offset `1`. `name_offset` indexes into that list.
- `pfs_flags & 0x2000000000000000` → "new crypt" key schedule.

Entry decryption (metadata only, needs just the passcode):
`seed = ComputeKeys(content_id, passcode, key_index)`,
`digest = SHA256(entry_32_raw_bytes + seed)`,
AES-128-CBC with `key = digest[16:32]`, `iv = digest[0:16]`, no padding.
Key index 3 (retail licensed content) additionally needs RSA material this tool
does not and will not ship.

Known entry ids: `0x0001` digests, `0x0010` entry keys, `0x0020` image key,
`0x0080` general digests, `0x0100` metas, `0x0200` entry names, `0x0400`
license.dat, `0x040A` imagedigs.dat, `0x1000` param.sfo, `0x1001`
playgo-chunk.dat, `0x1200` icon0.png, `0x2000` param.json.

### PS5 `\x7FFIH` finalized image (little-endian)

| Offset | Field |
| --- | --- |
| `0x00` | magic `7F 46 49 48` |
| `0x05` | `0x00` debug, `0x80` retail |
| `0x10` / `0x18` | outer PFS offset (always `0x10000`) / size (u64) |
| `0x58` | embedded `\x7FCNT` offset (u64) |
| `0xA0` | embedded `\x7FCNT` size (u64) |

Segment order is FIH / outer PFS / embedded CNT / trailing install metadata.
The application filesystem is **not** the outer PFS: it is `pfs_image.dat`
inside it, itself a PFS image (format version 2). `eboot.bin` lives at the root
of that inner image. `Pkg._open_ps5_inner()` implements this nesting.

### PFS (little-endian, 64 KiB blocks typical)

Superblock: version u64 @`0x00` (1 = PS4, 2 = PS5), magic u64 @`0x08`
(`20130315`), mode u16 @`0x1C` (bit0 signed, bit1 64-bit inodes, bit2 encrypted,
bit3 case-insensitive/always-set), block size u32 @`0x20`, nblock u64 @`0x28`,
inode count u64 @`0x30`, data block count @`0x38`, inode block count @`0x40`,
superroot inode u64 @`0x48`, 16-byte seed @`0x370`.

Inode table starts at block 1, `block_size // inode_size` inodes per block.
**The three inode layouts (this is the trap — see §6):**

| mode | size | layout |
| --- | --- | --- |
| unsigned | `0xA8` | 100-byte header, then 12 direct + 5 indirect u32 |
| signed-32 | `0x2C8` | 100-byte header, then 17 × (32-byte sig + u32) |
| signed-64 | `0x310` | 104-byte header (4-byte pad), then 17 × (32-byte sig + u64) |

The common header is 100 bytes and ends with `blocks` (u32) at `0x60`; the
signed layouts put their pointer table at `0x64` (`0x68` for signed-64).
Header fields: mode u16 @`0x00`, nlink u16 @`0x02`, flags u32 @`0x04`,
size i64 @`0x08`, size_compressed i64 @`0x10`, 4×time i64 @`0x18`,
4×nsec u32 @`0x38`, uid @`0x48`, gid @`0x4C`, 2×u64 @`0x50`, blocks u32 @`0x60`.
`flags & 0x1` = compressed (PFSC). `size != size_compressed` also means compressed.

Block maps: `blocks == 1` → single block; `direct[1] == 0xFFFFFFFF`
(or `0xFFFFFFFFFFFFFFFF` for 64-bit) marks a contiguous run starting at
`direct[0]` of length `blocks`; otherwise walk 12 direct, then indirect[0]
(single indirect), then indirect[1] (double indirect). Indirect entries are
36 bytes signed (32 sig + u32) or 40 bytes for signed-64, 4 bytes unsigned.

Dirents (variable length): `inode u32, type i32, name_length i32,
entry_size i32, name`, padded to an 8-byte boundary; `entry_size == 0`
terminates. Types: 2 file, 3 dir, 4 dot, 5 dotdot. The superroot's directory
contains `uroot`, which is the content tree.

### PFSC compressed files

A compressed inode stores `size_compressed` bytes through its normal block map;
those bytes are a PFSC stream that decodes to `size` bytes (little-endian):

| Offset | Field |
| --- | --- |
| `0x00` | magic `PFSC` |
| `0x04` / `0x08` | unknown u32 (`0`) / unknown u32 (`6`) |
| `0x0C` / `0x10` | block size u32 / block size again as u64 (`0x10000` typical; must match) |
| `0x18` | offset of the block-offset table (u64, `0x400` typical) |
| `0x20` | data start (u64) |
| `0x28` | decoded length (u64; must equal the inode `size`) |

The table holds `ceil(size / block_size) + 1` u64 offsets into the stream;
block `i` is `[t[i], t[i+1])`. Stored size `== block_size` means the block is
raw; shorter means zlib (raw deflate is also accepted). Every block decodes to
exactly `block_size` except the last, which may be short or padded to a full
block. `PfscReader` reads two table entries per block and caches one decoded
block, so memory stays bounded. Any bad offset, oversize block, inflate error,
trailing junk or wrong decoded length raises `CorruptImage`.

The field layout follows LibOrbisPkg's `PFSCHdr`; it has not been checked
against a real compressed package (none are available here).

### XTS encryption

- Sector size `0x1000`; the **first `block_size // 0x1000` sectors are plaintext**
  (the superblock block), decryption starts at that sector index.
- Tweak input = `LE64(sector_number) || 8 zero bytes`.
- Key derivation: `enc = HMAC-SHA256(base, LE32(1) + seed)`; `tweak = enc[:16]`,
  `data = enc[16:32]`; XTS key order is `data || tweak`.
- `base = EKPFS` normally, or `HMAC-SHA256(EKPFS, seed)` when "new crypt" is set.
- Passcode → EKPFS: `SHA256( SHA256(BE32(1)) || SHA256(content_id padded to 48) || passcode )`.
  A SHA3-256 variant exists for some PS5 debug images; both are tried.

### SELF container

Header: magic u32 (`4F153D1D` PS4, `5414F5EE` PS5), version u8 @`0x04`,
mode @`0x05`, endian @`0x06`, attributes @`0x07`, content_type @`0x08`,
key_type @`0x09`, header_size u16 @`0x0C`, meta_size u16 @`0x0E`,
file_size u64 @`0x10`, entry_count u16 @`0x18`, flags u16 @`0x1A`.
Segment entries start at `0x20`, `0x20` bytes each:
`props u64, file_offset u64, file_size u64, memory_size u64`.
Segment props bits: bit1 encrypted, bit2 signed, bit3 compressed.
The embedded ELF header follows the segment table (scan forward for `\x7fELF`).

Rebuild algorithm: copy the embedded ELF header and phdr table verbatim, then
for each program header with `p_filesz > 0` find an unused segment whose
`file_size == p_filesz`, copy that payload to `p_offset`, and clear the section
header table (`e_shoff = 0`, `e_shnum = 0`). Refuse if any used segment is
encrypted or compressed — do not emit a best-effort file.

## 6. Traps already hit (do not re-introduce)

1. **LibOrbisPkg's signed-32 inode size is inconsistent with its own
   deserializer** (`SizeOf = 0x2C8` but `ReadFromStream` consumes `0x30C`).
   Trust the 100-byte-header layout in §5, cross-checked against the Rust
   `orbis-pfs` reader and MkPFS. Getting this wrong silently misaligns every
   inode after the first.
2. **Wrong key must be detected, not tolerated.** A wrong XTS key still yields
   a valid *plaintext* superblock (block 0 is unencrypted), so magic checks
   alone prove nothing. `PfsImage._validate_inodes()` requires a majority of
   inodes to have a valid dir/file mode bit, and root resolution must find
   `uroot`. Keep that guard.
3. **Sub-sector XTS reads corrupt data.** Always align reads to sector
   boundaries, decrypt whole sectors, then slice.
4. `FileHandle.read_at` clamps at EOF (file semantics); `Reader`/`WindowReader`
   stay strict (structural bounds). Don't merge these behaviours.
5. Superroot inode is not always 0 — read it from `0x48`.
6. `extract` sanitises output paths (`_safe_relative`) to stop a malicious image
   writing outside the destination. Keep it.

## 7. Not implemented (and honest about it)

- **PS5 outer-PFS compression** and the trailing SI install segment (ignored).
- **Retail packages**: undecryptable without console key material. Deliberately
  unsupported; no keys are bundled anywhere in this codebase.
- `license.dat` decryption for key index 3 (needs RSA private keys).
- Delta-patch-specific entries and PS5 SELF beyond fake-self modules.

## 8. Suggested next steps

1. Test PFSC against a real compressed fake-PKG; the reader is built from the
   documented layout and synthetic fixtures only.
2. `param.sfo` → JSON (unencrypted, present in every package, immediately makes
   `info` much more useful).
3. More converter targets via the `CONVERTERS` registry in `convert.py`:
   PNG/DDS passthrough, AT9 → WAV, whole-PFS folder or ISO export.
4. Fuzz the parsers with truncated/corrupted inputs — the error paths are
   written but not adversarially tested.
5. A standalone desktop build (PyInstaller) if the audience isn't developers.

## 9. Legal / scope boundary

Keep the tool free of keys. Fake and debug packages open with a passcode alone;
retail packages are out of scope and should stay that way. Malformed or
encrypted input should always fail loudly rather than produce a plausible but
wrong output file.
