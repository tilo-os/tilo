"""Byte fixtures exercise the signature exception without running codesign."""

from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "dist/macos"))

import struct
import unittest

from collection_signing_macho import MachOError, compare_signed_macho, inspect_macho


def segment(name, offset, length, vmaddr, sections=()):
    size = 72 + 80 * len(sections)
    command = struct.pack("<II16sQQQQiiII", 0x19, size, name, vmaddr,
                          (length + 16383) // 16384 * 16384, offset, length,
                          5, 5, len(sections), 0)
    return command + b"".join(sections)


def fixture(signature_size=0, extra_data=b"hello world", signature_byte=b"S"):
    # __TEXT has a real section, sufficient zero header padding, and nonzero code.
    text = bytearray(16384)
    text[1024:1040] = bytes(range(16))
    section = struct.pack("<16s16sQQIIIIIIII", b"__text", b"__TEXT", 0x100000400,
                          16, 1024, 2, 0, 0, 0x80000400, 0, 0, 0)
    commands = [segment(b"__TEXT", 0, len(text), 0x100000000, [section])]
    sigoff = (len(text) + len(extra_data) + 15) // 16 * 16
    payload = extra_data
    if signature_size:
        payload += bytes(sigoff - len(text) - len(payload)) + signature_byte * signature_size
    commands.append(segment(b"__LINKEDIT", len(text), len(payload), 0x100004000))
    commands.append(struct.pack("<6I", 2, 24, len(text), 0, len(text), len(extra_data)))
    if signature_size:
        commands.append(struct.pack("<4I", 0x1D, 16, sigoff, signature_size))
    raw = b"".join(commands)
    text[:32] = struct.pack("<8I", 0xFEEDFACF, 0x100000C, 0, 2,
                            len(commands), len(raw), 0x200085, 0)
    text[32:32 + len(raw)] = raw
    return bytes(text) + payload


def patch(data, offset, fmt, *values):
    changed = bytearray(data)
    struct.pack_into(fmt, changed, offset, *values)
    return bytes(changed)


class SigningProjectionTests(unittest.TestCase):
    def setUp(self):
        self.unsigned = fixture()
        self.signed = fixture(128)
        self.linkedit = 32 + 152
        self.symtab = self.linkedit + 72
        self.sigcmd = self.symtab + 24

    def rejected(self, before=None, after=None):
        with self.assertRaises(MachOError):
            compare_signed_macho(self.unsigned if before is None else before,
                                 self.signed if after is None else after)

    def test_unsigned_to_signed_with_zero_alignment_padding(self):
        result = compare_signed_macho(self.unsigned, self.signed)
        self.assertTrue(result["signature_only_change"])
        self.assertEqual(result["signature_size"], 128)
        self.assertEqual(result["preserved_code_end"], len(self.unsigned))

    def test_replace_adhoc_signature_with_larger_signature(self):
        result = compare_signed_macho(fixture(128, signature_byte=b"A"), fixture(19000))
        self.assertEqual(result["signature_size"], 19000)

    def test_replace_signature_with_smaller_signature(self):
        compare_signed_macho(fixture(19000), fixture(64))

    def test_unchanged_linkedit_virtual_size_is_accepted(self):
        old = patch(fixture(128), self.linkedit + 32, "<Q", 32768)
        new = patch(fixture(256), self.linkedit + 32, "<Q", 32768)
        compare_signed_macho(old, new)

    def test_aligned_original_requires_no_extra_padding(self):
        compare_signed_macho(fixture(extra_data=b"D" * 16), fixture(128, extra_data=b"D" * 16))

    def test_code_byte_change(self):
        self.rejected(after=patch(self.signed, 1024, "<B", 99))

    def test_nonsection_padding_change(self):
        self.rejected(after=patch(self.signed, 4096, "<B", 99))

    def test_string_table_change(self):
        self.rejected(after=patch(self.signed, 16384, "<B", 99))

    def test_signature_alignment_padding_change(self):
        self.rejected(after=patch(self.signed, len(self.unsigned), "<B", 1))

    def test_consumed_header_padding_must_be_zero(self):
        self.rejected(before=patch(self.unsigned, self.sigcmd, "<B", 1))

    def test_header_flags_change(self):
        self.rejected(after=patch(self.signed, 24, "<I", 0))

    def test_non_signature_load_command_change(self):
        self.rejected(after=patch(self.signed, self.linkedit + 56, "<I", 7))

    def test_linkedit_fileoff_change(self):
        self.rejected(after=patch(self.signed, self.linkedit + 40, "<Q", 16385))

    def test_unexpected_linkedit_virtual_size_change(self):
        self.rejected(after=patch(self.signed, self.linkedit + 32, "<Q", 65536))

    def test_signature_data_is_allowed_to_change(self):
        compare_signed_macho(fixture(128, signature_byte=b"A"), fixture(128, signature_byte=b"B"))

    def test_existing_signature_offset_may_not_move(self):
        old = fixture(128)
        new = fixture(128, extra_data=b"hello world" + bytes(16))
        self.rejected(before=old, after=new)

    def test_no_signature_after(self):
        self.rejected(after=self.unsigned)

    def test_truncated_header(self):
        self.rejected(after=self.signed[:31])

    def test_bad_magic_or_fat_or_arch_or_arm64e(self):
        for offset, value in ((0, 0xCAFEBABE), (4, 0x1000007), (8, 2)):
            with self.subTest(value=value):
                self.rejected(after=patch(self.signed, offset, "<I", value))

    def test_nonzero_reserved_header(self):
        self.rejected(after=patch(self.signed, 28, "<I", 1))

    def test_invalid_load_command_count_size_or_alignment(self):
        for offset, value in ((16, 5000), (20, 0xFFFFFFFF), (36, 0), (36, 73)):
            with self.subTest(offset=offset, value=value):
                self.rejected(after=patch(self.signed, offset, "<I", value))

    def test_signature_extent_past_eof_or_not_terminal(self):
        for size in (127, 129, 0xFFFFFFFF):
            with self.subTest(size=size):
                self.rejected(after=patch(self.signed, self.sigcmd + 12, "<I", size))

    def test_unaligned_signature(self):
        self.rejected(after=patch(self.signed, self.sigcmd + 8, "<I", 16401))

    def test_signature_outside_linkedit(self):
        self.rejected(after=patch(self.signed, self.sigcmd + 8, "<II", 1024, len(self.signed) - 1024))

    def test_symtab_overlaps_signature(self):
        self.rejected(after=patch(self.signed, self.symtab + 20, "<I", 100))

    def test_section_overlaps_header(self):
        self.rejected(after=patch(self.signed, 32 + 72 + 48, "<I", 32))

    def test_section_exceeds_file_segment(self):
        self.rejected(after=patch(self.signed, 32 + 72 + 48, "<I", 16380))

    def test_segment_table_mismatch(self):
        self.rejected(after=patch(self.signed, 32 + 64, "<I", 500))

    def test_segment_overlap(self):
        data = patch(self.signed, self.linkedit + 40, "<QQ", 16368, len(self.signed) - 16368)
        self.rejected(after=data)

    def test_virtual_segment_overlap(self):
        self.rejected(after=patch(self.signed, self.linkedit + 24, "<Q", 0x100000100))

    def test_duplicate_signature_command(self):
        data = bytearray(self.signed)
        data[self.sigcmd + 16:self.sigcmd + 32] = data[self.sigcmd:self.sigcmd + 16]
        struct.pack_into("<II", data, 16, 5, self.sigcmd)
        self.rejected(after=bytes(data))

    def test_duplicate_linkedit(self):
        nameoff = 32 + 8
        data = bytearray(self.signed)
        data[nameoff:nameoff + 16] = b"__LINKEDIT\0\0\0\0\0\0"
        self.rejected(after=bytes(data))

    def test_unknown_load_command(self):
        self.rejected(after=patch(self.signed, self.symtab, "<I", 0xDEADBEEF))

    def test_parse_returns_no_signature_for_unsigned(self):
        self.assertIsNone(inspect_macho(self.unsigned)["signature"])


if __name__ == "__main__":
    unittest.main()
