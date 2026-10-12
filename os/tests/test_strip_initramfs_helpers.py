"""The initramfs gets Tilo's helpers without symbols, and nothing else changed.

os/scripts/strip-initramfs-helpers.py runs binutils' strip from another tree
and refuses any copy whose loaded sections differ from the original's. These
tests build small ELF files by hand, so they need no toolchain.
"""
import importlib.util
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'os/scripts/strip-initramfs-helpers.py'
spec = importlib.util.spec_from_file_location('strip_initramfs_helpers', SCRIPT)
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)

PROGBITS, SYMTAB, STRTAB = 1, 2, 3
ALLOC_EXEC = 0x6


def elf(text=b'\x90' * 64, symbols=True, entry=0x1000):
    """An ELF64 executable with one loaded .text, and a symbol table when `symbols`."""
    sections = [('.text', PROGBITS, ALLOC_EXEC, 0x1000, text)]
    if symbols:
        sections += [('.symtab', SYMTAB, 0, 0, b'\0' * 48), ('.strtab', STRTAB, 0, 0, b'\0main\0')]
    names = b'\0' + b''.join(name.encode() + b'\0' for name, *_ in sections) + b'.shstrtab\0'
    sections.append(('.shstrtab', STRTAB, 0, 0, names))
    body = bytearray(64)
    placed = []
    for name, kind, flags, address, contents in sections:
        placed.append((names.index(name.encode() + b'\0'), kind, flags, address, len(body), len(contents)))
        body += contents
    shoff = len(body)
    body += b'\0' * 64
    for name, kind, flags, address, offset, size in placed:
        body += struct.pack('<IIQQQQIIQQ', name, kind, flags, address, offset, size, 0, 0, 1, 0)
    header = b'\x7fELF\x02\x01\x01' + b'\0' * 9
    header += struct.pack('<HHIQQQIHHHHHH', 2, 62, 1, entry, 0, shoff, 0, 64, 56, 0, 64,
                          len(placed) + 1, len(placed))
    body[:64] = header
    return bytes(body)


class StripCheck(unittest.TestCase):
    def test_a_copy_without_symbols_and_the_same_code_is_accepted(self):
        helpers.check(elf(), elf(symbols=False))

    def test_a_copy_that_kept_its_symbols_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'still carries a symbol table'):
            helpers.check(elf(), elf())

    def test_a_copy_with_other_code_or_entry_is_refused(self):
        with self.assertRaisesRegex(ValueError, r'\.text differs'):
            helpers.check(elf(), elf(text=b'\xcc' * 64, symbols=False))
        with self.assertRaisesRegex(ValueError, 'entry point'):
            helpers.check(elf(), elf(symbols=False, entry=0x2000))

    def test_an_original_without_symbols_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'nothing to strip'):
            helpers.check(elf(symbols=False), elf(symbols=False))


class StripRun(unittest.TestCase):
    def run_script(self, stripped):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / 'tools'
            (tools / 'usr/bin').mkdir(parents=True)
            (tools / 'usr/lib64').mkdir(parents=True)
            loader = tools / 'usr/lib64/ld-linux-x86-64.so.2'
            # The loader is handed `--library-path DIRS strip ARGS...`.
            loader.write_text('#!/bin/sh\nshift 2\nexec "$@"\n')
            strip = tools / 'usr/bin/strip'
            strip.write_text('#!/bin/sh\n'
                             'while [ "$1" != -o ]; do shift; done\n'
                             'cp "$STRIPPED" "$2"\n')
            for tool in (loader, strip):
                tool.chmod(0o755)
            binary = root / 'tilo-rollback'
            binary.write_bytes(elf())
            binary.chmod(0o755)
            fixture = root / 'fixture'
            fixture.write_bytes(stripped)
            out = root / 'out'
            result = subprocess.run([sys.executable, str(SCRIPT), '--tools', str(tools), '--out', str(out),
                                     str(binary)], env=dict(os.environ, STRIPPED=str(fixture)),
                                    capture_output=True, text=True)
            copy = out / 'tilo-rollback'
            return result, copy.read_bytes() if copy.exists() else None, copy.exists() and os.access(copy, os.X_OK)

    def test_the_checked_copy_is_written_executable(self):
        result, copy, executable = self.run_script(elf(symbols=False))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(copy, elf(symbols=False))
        self.assertTrue(executable)

    def test_a_bad_copy_fails_the_build(self):
        result, _, _ = self.run_script(elf(text=b'\xcc' * 64, symbols=False))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('differs from the original', result.stderr)

    def test_the_module_scripts_and_the_image_build_use_the_copies(self):
        build = (REPO / 'os/scripts/build-initramfs.sh').read_text()
        self.assertIn('strip-initramfs-helpers.py', build)
        self.assertIn('without its symbol table', build)
        for module, helper in (('91tilo-rollback', 'tilo-rollback'),
                               ('92tilo-sibling-unlock', 'tilo-sibling-unlock')):
            setup = (REPO / 'os/usr/lib/dracut/modules.d' / module / 'module-setup.sh').read_text()
            self.assertIn(f'tilo_initramfs_helper {helper}', setup)
        containerfile = (REPO / 'os/Containerfile').read_text()
        self.assertIn('TILO_STRIP_TOOLS=/run/tilo-strip-tools bash /run/tilo-build-initramfs.sh', containerfile)


if __name__ == '__main__':
    unittest.main()
