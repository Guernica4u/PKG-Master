"""Finding content inside a package and converting it.

``CONVERTERS`` is the extension point: each entry maps a target name to a
callable that turns raw file bytes into the requested output. Adding PNG, AT9,
SFO, or folder export later means adding one entry here plus a CLI choice.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, Iterator

from .elf import is_elf, is_self
from .errors import ConversionError, PkgMasterError, UnsupportedFeature
from .pkg import Pkg
from .pfs import PfsImage
from .self import SelfConversion, to_elf

#: Preferred in-package targets for the ``elf`` conversion, best first.
ELF_CANDIDATES = [
    "eboot.bin",
    "sce_sys/about/right.sprx",
    "sce_sys/eboot.bin",
]

#: Suffixes that are worth sniffing when no well-known name is present.
ELF_SUFFIXES = (".elf", ".prx", ".sprx", ".bin", ".self")

CHUNK_SIZE = 1 << 20


@dataclass
class ContentRef:
    """A file selected inside a package, lazy about reading its bytes."""

    path: str
    size: int
    reader: object  # any read_at provider
    container: str = ""
    kind: str = "file"

    def read_all(self, progress: Callable[[int, int], None] | None = None) -> bytes:
        out = bytearray()
        while len(out) < self.size:
            take = min(CHUNK_SIZE, self.size - len(out))
            chunk = self.reader.read_at(len(out), take)
            out += chunk
            if progress is not None:
                progress(len(out), self.size)
        return bytes(out)

    def read_prefix(self, length: int) -> bytes:
        return self.reader.read_at(0, min(length, self.size))

    def to_dict(self) -> dict:
        return {
            "path": self.path,
            "size": self.size,
            "container": self.container,
            "kind": self.kind,
        }


@dataclass
class EltConversionResult:
    source: ContentRef
    output: str
    conversion: SelfConversion

    def to_dict(self) -> dict:
        return {
            "source": self.source.to_dict(),
            "output": self.output,
            "container": "SELF" if self.conversion.info else "ELF",
            "warnings": list(self.conversion.warnings),
            "bytes_written": len(self.conversion.data),
            "elf": self.conversion.info.to_dict(),
        }


def sniff(path: str, reader) -> str:
    """Classify a file by its leading bytes: ``elf``, ``self``, or ``other``."""
    try:
        head = reader.read_at(0, 8)
    except PkgMasterError:
        return "other"
    if is_elf(head):
        return "elf"
    if is_self(head):
        return "self"
    return "other"


def iter_elf_candidates(pfs: PfsImage) -> Iterator[ContentRef]:
    """Yield plausible ELF/SELF files, best candidates first."""
    for name in ELF_CANDIDATES:
        handle = pfs.find(name)
        if handle is not None and sniff(handle.path, handle) != "other":
            yield ContentRef(handle.path, handle.size, handle)

    candidates = []
    for path, inode in pfs.all_files():
        if path in ELF_CANDIDATES:
            continue
        lower = path.lower()
        if not lower.endswith(ELF_SUFFIXES):
            continue
        candidates.append((path, inode))
    candidates.sort(key=lambda item: (item[0].count("/"), item[0]))
    for path, inode in candidates:
        handle = pfs.find(path)
        if handle is None:
            continue
        if sniff(path, handle) == "other":
            continue
        yield ContentRef(path, handle.size, handle)


def resolve_elf_source(pfs: PfsImage, path: str | None) -> ContentRef:
    if path:
        handle = pfs.find(path)
        if handle is None:
            if pfs.resolve(path) is not None:
                raise ConversionError(f"{path} is a directory, not a file")
            raise ConversionError(f"{path} was not found in the package filesystem")
        return ContentRef(handle.path, handle.size, handle)

    for candidate in iter_elf_candidates(pfs):
        return candidate
    raise ConversionError(
        "no ELF or SELF file was found in the package; pass --path to pick one"
    )


def load_package_source(
    path: str,
    *,
    file_path: str | None = None,
    passcode: str | None = None,
    ekpfs: bytes | str | None = None,
    new_crypt: bool | None = None,
) -> tuple[Pkg | None, ContentRef, PfsImage | None]:
    """Resolve a CLI source into a content reference.

    A package input is opened and searched; a standalone file is wrapped as-is,
    which makes ``pkgmaster convert some.bin --to elf`` work without a package.
    """
    with open(path, "rb") as probe:
        magic = probe.read(4)

    if magic in (b"\x7fCNT", b"\x7fFIH"):
        pkg = Pkg.open(path)
        pfs = pkg.open_pfs(passcode=passcode, ekpfs=ekpfs, new_crypt=new_crypt)
        ref = resolve_elf_source(pfs, file_path)
        ref.container = pkg.kind
        return pkg, ref, pfs

    handle = open(path, "rb")
    from .binary import Reader

    reader = Reader(handle, name=path)
    kind = sniff(path, reader)
    if kind == "other":
        handle.close()
        raise ConversionError(
            f"{path} is neither a package, nor an ELF/SELF file"
        )
    return None, ContentRef(path, reader.size, reader, container="file", kind=kind), None


def convert_to_elf(
    ref: ContentRef,
    *,
    strip_sce: bool = False,
    progress: Callable[[int, int], None] | None = None,
) -> SelfConversion:
    """Convert a content reference to a standard ELF."""
    data = ref.read_all(progress)
    if not (is_elf(data) or is_self(data)):
        raise ConversionError(f"{ref.path} is not an ELF or SELF file")
    result = to_elf(data)
    if strip_sce:
        result = _strip_sce_metadata(result)
    return result


def _strip_sce_metadata(result: SelfConversion) -> SelfConversion:
    """Zero Sony-specific program headers, leaving a plain loadable ELF.

    SCE program headers such as ``PT_SCE_COMMENT`` and ``PT_SCE_VERSION`` carry
    metadata that generic tooling cannot interpret. Zeroing them (rather than
    deleting them, which would shift the program header table) keeps the file
    loadable by ordinary ELF readers.
    """
    info = result.info
    if not info.e_phnum:
        return result
    data = bytearray(result.data)
    entry_size = info.e_phentsize or 56
    changed = 0
    for phdr in info.phdrs:
        if 0x60000000 <= phdr.p_type <= 0x6FFFFFFF:
            base = info.e_phoff + phdr.index * entry_size
            data[base : base + entry_size] = b"\x00" * entry_size
            changed += 1
    if not changed:
        return result
    from .elf import parse_elf

    warnings = list(result.warnings)
    warnings.append(f"zeroed {changed} SCE program header(s) (--strip-sce)")
    return SelfConversion(
        data=bytes(data),
        info=parse_elf(bytes(data)),
        warnings=warnings,
        copied_segments=result.copied_segments,
        zero_filled_segments=result.zero_filled_segments,
    )
