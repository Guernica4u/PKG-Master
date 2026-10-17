from __future__ import annotations

import pytest

from pkgmaster.elf import is_elf, is_self, parse_elf
from pkgmaster.errors import CorruptImage, UnsupportedFeature
from pkgmaster.self import describe, parse_self, to_elf

from pkgmaster import synth as b

CODE = b"\x90\x90\x90\xcc" * 32
RODATA = b"rodata-section" * 40


def test_plain_elf_passes_through_unchanged():
    elf = b.build_elf([CODE, RODATA])
    result = to_elf(elf)
    assert result.data == elf
    assert result.info.type_name == "ET_SCE_EXEC_ASLR"


def test_self_is_detected_and_rebuilt():
    elf = b.build_elf([CODE, RODATA])
    blob = b.build_self(elf)
    assert is_self(blob) and not is_elf(blob)

    header, info, elf_offset = parse_self(blob)
    assert header.variant == "PS4"
    assert header.entry_count == 3
    assert is_elf(blob[elf_offset:])

    result = to_elf(blob)
    assert is_elf(result.data)
    assert result.copied_segments == 3
    assert result.zero_filled_segments == 0
    assert result.warnings == []

    rebuilt = parse_elf(result.data)
    original = parse_elf(elf)
    assert rebuilt.e_entry == original.e_entry
    assert rebuilt.e_type == original.e_type
    assert rebuilt.e_phnum == original.e_phnum

    for phdr in rebuilt.load_segments():
        src = next(p for p in original.phdrs if p.index == phdr.index)
        assert result.data[phdr.p_offset : phdr.p_offset + phdr.p_filesz] == elf[
            src.p_offset : src.p_offset + src.p_filesz
        ]


def test_ps5_self_variant():
    blob = b.build_self(b.build_elf([CODE]), variant="PS5")
    header, _, _ = parse_self(blob)
    assert header.variant == "PS5"
    assert to_elf(blob).copied_segments == 2


def test_encrypted_segment_is_refused():
    blob = b.build_self(b.build_elf([CODE, RODATA]), encrypt_first=True)
    with pytest.raises(UnsupportedFeature) as excinfo:
        to_elf(blob)
    assert "encrypted" in str(excinfo.value)


def test_compressed_segment_is_refused():
    blob = b.build_self(b.build_elf([CODE, RODATA]), compress_first=True)
    with pytest.raises(UnsupportedFeature):
        to_elf(blob)


def test_describe_covers_self_and_elf():
    elf = b.build_elf([CODE])
    assert describe(elf)["container"] == "ELF"
    described = describe(b.build_self(elf))
    assert described["container"] == "SELF"
    assert described["self"]["variant"] == "PS4"
    assert described["elf"]["phnum"] == 2


def test_describe_rejects_other_data():
    with pytest.raises(CorruptImage):
        describe(b"not an elf" * 8)
