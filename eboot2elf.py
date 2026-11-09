#!/usr/bin/env python3
"""eboot2elf: turn a PS4/PS5 eboot.bin (or .prx/.sprx) into a normal ELF.

    python eboot2elf.py eboot.bin              writes eboot.elf next to it
    python eboot2elf.py eboot.bin out.elf

Plain Python 3.8+, nothing to install. On Windows you can also drop a file
onto this script.
"""

import os
import struct
import sys

ELF_MAGIC = b"\x7fELF"
SELF_MAGICS = (0x1D3D154F, 0xEEF51454)  # PS4, PS5
ENCRYPTED, COMPRESSED = 0x2, 0x8


class ConvertError(Exception):
    pass


def to_elf(data):
    """Return (elf_bytes, warnings) for a SELF. An ELF comes back as is."""
    if data[:4] == ELF_MAGIC:
        return data, []
    if len(data) < 0x20 or struct.unpack_from("<I", data)[0] not in SELF_MAGICS:
        raise ConvertError("not an eboot.bin: no SELF or ELF header")

    count = struct.unpack_from("<H", data, 0x18)[0]
    table_end = 0x20 + count * 0x20
    if count > 4096 or table_end > len(data):
        raise ConvertError("SELF segment table is damaged")
    # Each segment: (props, offset, size).
    segments = [struct.unpack_from("<QQQ", data, 0x20 + i * 0x20) for i in range(count)]

    locked = [str(i) for i, (props, _, _) in enumerate(segments) if props & (ENCRYPTED | COMPRESSED)]
    if locked:
        raise ConvertError(f"segments {', '.join(locked)} are encrypted or compressed; decrypt this file first")

    elf = data.find(ELF_MAGIC, table_end, table_end + 0x1000)
    if elf < 0:
        elf = data.find(ELF_MAGIC, table_end)
    if elf < 0:
        raise ConvertError("SELF has no ELF header inside")
    if len(data) < elf + 0x40:
        raise ConvertError("ELF header is cut off")

    phoff, shoff = struct.unpack_from("<QQ", data, elf + 0x20)
    phentsize, phnum = struct.unpack_from("<HH", data, elf + 0x36)
    phentsize = phentsize or 56
    phdr_bytes = phnum * phentsize
    if elf + phoff + phdr_bytes > len(data):
        raise ConvertError("program headers are cut off")

    # Program headers: (type, offset, size in file).
    phdrs = []
    for i in range(phnum):
        base = elf + phoff + i * phentsize
        p_type = struct.unpack_from("<I", data, base)[0]
        p_offset = struct.unpack_from("<Q", data, base + 0x08)[0]
        p_filesz = struct.unpack_from("<Q", data, base + 0x20)[0]
        phdrs.append((p_type, p_offset, p_filesz))

    total = max([0x40, phoff + phdr_bytes] + [off + size for _, off, size in phdrs if size])
    if total > 1 << 32:
        raise ConvertError("program headers point past 4 GB")
    out = bytearray(total)
    out[:0x40] = data[elf:elf + 0x40]
    out[phoff:phoff + phdr_bytes] = data[elf + phoff:elf + phoff + phdr_bytes]

    # Segments and program headers line up by size, in order.
    unused = [s for s in segments if s[2]]
    warnings, copied = [], 0
    for i, (p_type, p_offset, p_filesz) in enumerate(phdrs):
        if not p_filesz:
            continue
        seg = next((s for s in unused if s[2] == p_filesz), None)
        if seg is None:
            warnings.append(f"program header {i} (0x{p_type:X}) has no matching segment; left as zeros")
            continue
        unused.remove(seg)
        _, seg_off, size = seg
        if seg_off + size > len(data):
            raise ConvertError("a segment runs past the end of the file")
        out[p_offset:p_offset + size] = data[seg_off:seg_off + size]
        copied += 1

    if not copied:
        raise ConvertError("no segment matched a program header")
    if unused:
        warnings.append(f"{len(unused)} segment(s) were not used")
    if shoff:
        # Section headers aren't in a SELF; clear them so the ELF stays valid.
        out[0x28:0x30] = bytes(8)
        out[0x3C:0x3E] = bytes(2)
    return bytes(out), warnings


def main(argv):
    if len(argv) not in (1, 2):
        print(__doc__.strip())
        return 2
    src = argv[0]
    dst = argv[1] if len(argv) == 2 else os.path.splitext(src)[0] + ".elf"
    if os.path.abspath(src) == os.path.abspath(dst):
        print("error: output would overwrite the input; give an output name")
        return 2
    try:
        with open(src, "rb") as f:
            elf, warnings = to_elf(f.read())
    except (OSError, ConvertError) as e:
        print(f"error: {e}")
        return 1
    with open(dst, "wb") as f:
        f.write(elf)
    for w in warnings:
        print(f"warning: {w}")
    print(f"wrote {dst} ({len(elf)} bytes)")
    return 0


def own_console():
    """True when Windows opened a new window just for this script (double-click or drop)."""
    if os.name != "nt":
        return False
    import ctypes
    k32 = ctypes.windll.kernel32
    ids = (ctypes.c_uint32 * 16)()
    n = k32.GetConsoleProcessList(ids, 16)
    # Only Python itself (and the py.exe launcher) on this console: no shell.
    for pid in ids[:n]:
        handle = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        name = ctypes.create_unicode_buffer(512)
        size = ctypes.c_uint32(512)
        ok = handle and k32.QueryFullProcessImageNameW(handle, 0, name, ctypes.byref(size))
        if handle:
            k32.CloseHandle(handle)
        if not ok or not os.path.basename(name.value).lower().startswith(("py", "python")):
            return False
    return 0 < n <= 16


if __name__ == "__main__":
    code = main(sys.argv[1:])
    if own_console():
        input("\nPress Enter to close.")
    sys.exit(code)
