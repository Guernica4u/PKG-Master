"""``pkgmaster`` command line interface."""

from __future__ import annotations

import argparse
import json
import os
import sys

from .binary import jsonable
from .convert import ContentRef, convert_to_elf, load_package_source
from .self import describe as describe_binary
from .errors import (
    EncryptedContent,
    KeyRequired,
    NotAPackage,
    PkgMasterError,
    UnsupportedFeature,
)
from .pkg import Pkg, open_package
from .pfs import PfsImage

EXIT_OK = 0
EXIT_ERROR = 2
EXIT_KEY = 3
EXIT_UNSUPPORTED = 4


# --------------------------------------------------------------------------
# helpers


def _key_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--passcode",
        default=None,
        help="32-character FPKG passcode (or set PKGMASTER_PASSCODE)",
    )
    parser.add_argument(
        "--ekpfs",
        default=None,
        help="32-byte EKPFS key as 64 hex characters (overrides --passcode)",
    )
    parser.add_argument(
        "--new-crypt",
        action="store_true",
        default=None,
        help="force the alternate (PS5-era) key schedule",
    )


def _passcode(args) -> str | None:
    return args.passcode or os.environ.get("PKGMASTER_PASSCODE")


def _emit(payload: dict | list, as_json: bool, render) -> None:
    if as_json:
        json.dump(jsonable(payload), sys.stdout, indent=2)
        sys.stdout.write("\n")
    else:
        render()


def _fmt_size(value: int) -> str:
    if value >= 1 << 30:
        return f"{value / (1 << 30):.2f} GiB"
    if value >= 1 << 20:
        return f"{value / (1 << 20):.2f} MiB"
    if value >= 1 << 10:
        return f"{value / (1 << 10):.1f} KiB"
    return f"{value} B"


def _open_pfs(pkg: Pkg, args) -> PfsImage:
    return pkg.open_pfs(
        passcode=_passcode(args), ekpfs=args.ekpfs, new_crypt=args.new_crypt
    )


# --------------------------------------------------------------------------
# commands


def cmd_info(args) -> int:
    pkg = open_package(args.file)
    try:
        info = pkg.describe()
        pfs_error = None
        try:
            pfs = _open_pfs(pkg, args)
            info["pfs"] = pfs.header.to_dict()
            info["pfs"]["root_entries"] = len(pfs.listdir(pfs.root))
        except EncryptedContent as exc:
            pfs_error = str(exc)
            info["pfs"] = {"error": pfs_error}

        def render() -> None:
            header = info.get("header")
            print(f"file            {args.file}")
            print(f"format          {info['kind']}")
            print(f"content id      {info['content_id']}")
            if header:
                print(f"drm             {header['drm_name']}")
                print(f"content type    {header['content_type_name']}")
                print(
                    f"pfs image       0x{int(header['pfs_image_offset'], 16):X} "
                    f"({_fmt_size(header['pfs_image_size'])})"
                )
            if info.get("fih"):
                fih = info["fih"]
                print(f"variant         {fih['variant']}")
                print(
                    f"outer pfs       {fih['pfs_offset']} "
                    f"({_fmt_size(fih['pfs_size'])})"
                )
                print(f"embedded cnt    {fih['cnt_offset']} ({_fmt_size(fih['cnt_size'])})")
            print(f"entries         {info['entry_count']}")
            pfs_info = info.get("pfs", {})
            if "error" in pfs_info:
                print(f"pfs             unavailable: {pfs_info['error']}")
            else:
                flags = []
                if pfs_info["signed"]:
                    flags.append("signed")
                if pfs_info["64bit_inodes"]:
                    flags.append("64-bit inodes")
                if pfs_info["encrypted"]:
                    flags.append("encrypted")
                print(
                    f"pfs             v{pfs_info['version']}, "
                    f"block 0x{pfs_info['block_size']:X}, "
                    f"{pfs_info['inode_count']} inodes"
                    + (f" [{', '.join(flags)}]" if flags else "")
                )
                print(f"pfs root        {pfs_info.get('root_entries', 0)} entries")

        _emit(info, args.json, render)
        return EXIT_OK
    finally:
        pkg.close()


def cmd_entries(args) -> int:
    pkg = open_package(args.file)
    try:
        container = pkg.container
        rows = []
        for entry in container.entries:
            rows.append(
                {
                    "id": f"0x{entry.id:04X}",
                    "name": container.name_of(entry) or "-",
                    "size": entry.data_size,
                    "offset": f"0x{entry.data_offset:X}",
                    "encrypted": entry.encrypted,
                }
            )
        payload = {"kind": pkg.kind, "entries": rows}

        def render() -> None:
            print(f"{'id':>6}  {'size':>10}  {'offset':>10}  {'enc':>3}  name")
            for row in rows:
                print(
                    f"{row['id']:>6}  {_fmt_size(row['size']):>10}  "
                    f"{row['offset']:>10}  {'yes' if row['encrypted'] else 'no':>3}  "
                    f"{row['name']}"
                )

        _emit(payload, args.json, render)
        return EXIT_OK
    finally:
        pkg.close()


def cmd_list(args) -> int:
    pkg = open_package(args.file)
    try:
        pfs = _open_pfs(pkg, args)
        rows = []
        for path, inode in pfs.walk():
            if inode.is_dir and not args.dirs:
                continue
            if not inode.is_file and not args.dirs:
                continue
            if args.pattern and args.pattern.lower() not in path.lower():
                continue
            rows.append(
                {
                    "path": path,
                    "type": inode.label,
                    "size": inode.size,
                    "compressed": inode.is_compressed,
                }
            )
        payload = {"files": rows, "count": len(rows)}

        def render() -> None:
            for row in rows:
                marker = " (compressed)" if row["compressed"] else ""
                if row["type"] == "dir":
                    print(f"{'':>10}  {row['path']}/{marker}")
                else:
                    print(f"{_fmt_size(row['size']):>10}  {row['path']}{marker}")

        _emit(payload, args.json, render)
        return EXIT_OK
    finally:
        pkg.close()


def cmd_tree(args) -> int:
    pkg = open_package(args.file)
    try:
        pfs = _open_pfs(pkg, args)
        lines: list[str] = []

        def walk_dir(inode, prefix: str, depth: int) -> None:
            if depth > 32:
                return
            entries = [
                e for e in pfs.iter_dir(inode) if not (e.is_dir and e.name in (".", ".."))
            ]
            entries.sort(key=lambda e: (not e.is_dir, e.name))
            for i, entry in enumerate(entries):
                last = i == len(entries) - 1
                child = pfs.inode(entry.inode_number)
                branch = "`-- " if last else "|-- "
                if child.is_dir:
                    lines.append(f"{prefix}{branch}{entry.name}/")
                    walk_dir(child, prefix + ("    " if last else "|   "), depth + 1)
                else:
                    lines.append(
                        f"{prefix}{branch}{entry.name} ({_fmt_size(child.size)})"
                    )

        lines.append("uroot/")
        walk_dir(pfs.root, "", 0)

        def render() -> None:
            print("\n".join(lines))

        _emit({"tree": lines}, args.json, render)
        return EXIT_OK
    finally:
        pkg.close()


def _safe_relative(path: str) -> str:
    parts = [part for part in path.replace("\\", "/").split("/") if part]
    if not parts:
        raise PkgMasterError(f"refusing to write empty path from {path!r}")
    for part in parts:
        if part in (".", ".."):
            raise PkgMasterError(f"refusing to write suspicious path {path!r}")
    if os.path.isabs(path) or ":" in parts[0]:
        raise PkgMasterError(f"refusing to write absolute path {path!r}")
    return os.path.join(*parts)


def cmd_extract(args) -> int:
    pkg = open_package(args.file)
    try:
        pfs = _open_pfs(pkg, args)
        out_root = args.output or "."
        targets: list[tuple[str, object]] = []
        if args.path:
            inode = pfs.resolve(args.path)
            if inode is None:
                raise PkgMasterError(f"{args.path} was not found in the package")
            if inode.is_dir:
                base = args.path.strip("/")
                for path, child in pfs.walk():
                    if path == base or path.startswith(base + "/"):
                        targets.append((path, child))
            else:
                targets.append((args.path.strip("/"), inode))
        else:
            targets = [item for item in pfs.walk() if item[1].is_file]

        written = 0
        for path, inode in targets:
            if inode.is_dir:
                os.makedirs(os.path.join(out_root, _safe_relative(path)), exist_ok=True)
                continue
            destination = os.path.join(out_root, _safe_relative(path))
            os.makedirs(os.path.dirname(destination) or ".", exist_ok=True)
            with open(destination, "wb") as handle:
                pfs.copy_file(inode, handle)
            written += 1
            if not args.quiet:
                print(f"extracted {path} ({_fmt_size(inode.size)})")
        if not args.quiet:
            print(f"{written} file(s) written to {out_root}")
        return EXIT_OK
    finally:
        pkg.close()


def _default_output_name(source_path: str) -> str:
    base = os.path.basename(source_path)
    stem, ext = os.path.splitext(base)
    if ext.lower() in (".bin", ".self", ".sprx", ".prx", ".elf"):
        return f"{stem}.elf"
    return f"{base}.elf"


def cmd_convert(args) -> int:
    pkg, ref, _pfs = load_package_source(
        args.file,
        file_path=args.path,
        passcode=_passcode(args),
        ekpfs=args.ekpfs,
        new_crypt=args.new_crypt,
    )
    try:
        def progress(done: int, total: int) -> None:
            if not args.quiet and total:
                print(f"\r  reading {done * 100 // total:3d}%", end="", file=sys.stderr)

        result = convert_to_elf(ref, strip_sce=args.strip_sce, progress=progress)
        if not args.quiet:
            print("", file=sys.stderr)

        out_path = args.output or _default_output_name(ref.path)
        if os.path.isdir(out_path):
            out_path = os.path.join(out_path, _default_output_name(ref.path))
        with open(out_path, "wb") as handle:
            handle.write(result.data)

        payload = {
            "source": ref.to_dict(),
            "output": out_path,
            "bytes": len(result.data),
            "elf": result.info.to_dict(),
            "warnings": result.warnings,
        }

        def render() -> None:
            print(f"source          {ref.path}")
            print(f"output          {out_path} ({_fmt_size(len(result.data))})")
            print(
                f"elf             {result.info.type_name} / {result.info.machine_name} / "
                f"{result.info.osabi_name}"
            )
            print(f"entry point     0x{result.info.e_entry:X}")
            print(f"segments        {result.copied_segments} copied, "
                  f"{result.zero_filled_segments} zero-filled")
            for warning in result.warnings:
                print(f"warning: {warning}")

        _emit(payload, args.json, render)
        return EXIT_OK
    finally:
        if pkg is not None:
            pkg.close()
        elif hasattr(ref.reader, "stream"):
            ref.reader.stream.close()


def cmd_elf(args) -> int:
    with open(args.file, "rb") as handle:
        data = handle.read()
    info = describe_binary(data)
    payload = dict(info)
    payload["path"] = args.file

    if args.output:
        result = convert_to_elf(
            ContentRef(args.file, len(data), _MemoryReader(data), container="file"),
            strip_sce=args.strip_sce,
        )
        with open(args.output, "wb") as handle:
            handle.write(result.data)
        payload["output"] = args.output
        payload["bytes"] = len(result.data)
        payload["warnings"] = result.warnings

    def render() -> None:
        container = payload.get("container", "unknown")
        print(f"file            {args.file}")
        print(f"container       {container}")
        elf = payload.get("elf", {})
        print(
            f"elf             {elf.get('type_name')} / {elf.get('machine')} / "
            f"{elf.get('os_abi')}"
        )
        print(f"entry point     {elf.get('entry')}")
        print(f"program headers {elf.get('phnum')}")
        for phdr in elf.get("phdrs", []):
            print(
                f"  #{phdr['index']} {phdr['type_name']:<18} "
                f"off 0x{phdr['offset']:X} filesz 0x{phdr['filesz']:X} "
                f"vaddr {phdr['vaddr']}"
            )
        if "self" in payload:
            self_info = payload["self"]
            print(f"self variant    {self_info['variant']}")
            print(f"self segments   {self_info['entry_count']}")
        if payload.get("output"):
            print(f"output          {payload['output']} ({_fmt_size(payload['bytes'])})")
        for warning in payload.get("warnings", []):
            print(f"warning: {warning}")

    _emit(payload, args.json, render)
    return EXIT_OK


class _MemoryReader:
    __slots__ = ("data", "name", "size")

    def __init__(self, data: bytes) -> None:
        self.data = data
        self.name = "memory"
        self.size = len(data)

    def read_at(self, offset: int, length: int) -> bytes:
        return self.data[offset : offset + length]

    def __len__(self) -> int:
        return self.size


# --------------------------------------------------------------------------
# parser


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="pkgmaster",
        description="Inspect PS4/PS5 PKG files and convert their contents.",
    )
    parser.add_argument("--version", action="version", version="pkgmaster 0.1.0")
    sub = parser.add_subparsers(dest="command", required=True)

    info = sub.add_parser("info", help="show package metadata and PFS summary")
    info.add_argument("file")
    info.add_argument("--json", action="store_true")
    _key_args(info)
    info.set_defaults(func=cmd_info)

    entries = sub.add_parser("entries", help="list package metadata entries")
    entries.add_argument("file")
    entries.add_argument("--json", action="store_true")
    entries.set_defaults(func=cmd_entries)

    listing = sub.add_parser("list", help="list files inside the PFS filesystem")
    listing.add_argument("file")
    listing.add_argument("--json", action="store_true")
    listing.add_argument("--dirs", action="store_true", help="include directories")
    listing.add_argument("--pattern", default=None, help="substring filter")
    _key_args(listing)
    listing.set_defaults(func=cmd_list)

    tree = sub.add_parser("tree", help="print the filesystem tree")
    tree.add_argument("file")
    tree.add_argument("--json", action="store_true")
    _key_args(tree)
    tree.set_defaults(func=cmd_tree)

    extract = sub.add_parser("extract", help="extract files from the package")
    extract.add_argument("file")
    extract.add_argument("-o", "--output", default=".", help="output directory")
    extract.add_argument("--path", default=None, help="single file or directory inside PFS")
    extract.add_argument("--quiet", action="store_true")
    _key_args(extract)
    extract.set_defaults(func=cmd_extract)

    convert = sub.add_parser(
        "convert", help="convert content to another filetype (currently: elf)"
    )
    convert.add_argument("file", help="package, or a standalone ELF/SELF file")
    convert.add_argument("--to", default="elf", help="target format (default: elf)")
    convert.add_argument("-o", "--output", default=None, help="output path")
    convert.add_argument("--path", default=None, help="file inside the package to convert")
    convert.add_argument(
        "--strip-sce", action="store_true", help="zero Sony-specific program headers"
    )
    convert.add_argument("--quiet", action="store_true")
    convert.add_argument("--json", action="store_true")
    _key_args(convert)
    convert.set_defaults(func=cmd_convert)

    elf = sub.add_parser("elf", help="inspect and convert a standalone ELF/SELF file")
    elf.add_argument("file")
    elf.add_argument("-o", "--output", default=None, help="write a converted ELF here")
    elf.add_argument("--strip-sce", action="store_true")
    elf.add_argument("--json", action="store_true")
    elf.set_defaults(func=cmd_elf)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    if getattr(args, "command", None) == "convert" and args.to.lower() != "elf":
        print(
            f"error: target format {args.to!r} is not implemented yet "
            "(available: elf)",
            file=sys.stderr,
        )
        return EXIT_UNSUPPORTED

    try:
        return args.func(args)
    except KeyRequired as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_KEY
    except EncryptedContent as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_KEY
    except UnsupportedFeature as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_UNSUPPORTED
    except NotAPackage as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_ERROR
    except PkgMasterError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_ERROR
    except BrokenPipeError:  # pragma: no cover - piping into head
        return EXIT_OK


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
