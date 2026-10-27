#!/usr/bin/env python3
"""tools/ci/shstk-mark.py marks a shared library for shadow stacks and changes nothing else."""

from __future__ import annotations

import ctypes
import platform
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "tools/ci/shstk-mark.py"


def readelf(*args: str) -> str:
    return subprocess.run(["readelf", "-W", *args], check=True, capture_output=True, text=True).stdout


def section(path: Path, name: str) -> str:
    return readelf("-x", name, str(path))


@unittest.skipUnless(platform.machine() == "x86_64" and shutil.which("cc") and shutil.which("readelf"),
                     "needs an x86-64 C compiler and readelf")
class ShstkMarkTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="shstk-mark-"))
        self.addCleanup(shutil.rmtree, self.dir)
        source = self.dir / "lib.c"
        source.write_text("__attribute__((section(\".rustc\"))) const char meta[] = \"metadata\";\n"
                          "int answer(int x) { return x + 41; }\n")
        self.lib = self.dir / "libprobe.so"
        # Like the Rust toolchain's libstd: no GNU property note at all (no ISA note from the
        # assembler, none from the C runtime's start files).
        subprocess.run(["cc", "-shared", "-fPIC", "-O2", "-fcf-protection=none", "-Wa,-mx86-used-note=no",
                        "-nostartfiles", "-Wl,-z,now", "-Wl,-z,relro", "-o", str(self.lib), str(source)],
                       check=True)
        self.original = self.dir / "original.so"
        shutil.copy2(self.lib, self.original)

    def mark(self) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, "-I", str(SCRIPT), str(self.lib)],
                              capture_output=True, text=True)

    def test_marks_and_keeps_everything_else(self):
        self.assertNotIn("SHSTK", readelf("-n", str(self.original)))
        result = self.mark()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("x86 feature: SHSTK", readelf("-n", str(self.lib)))
        self.assertIn("GNU_PROPERTY", readelf("-l", str(self.lib)))
        self.assertEqual(readelf("--dyn-syms", str(self.lib)), readelf("--dyn-syms", str(self.original)))
        self.assertEqual(section(self.lib, ".rustc"), section(self.original, ".rustc"))
        self.assertEqual(section(self.lib, ".text"), section(self.original, ".text"))
        self.assertEqual(ctypes.CDLL(str(self.lib)).answer(1), 42)

    def test_marking_again_is_a_no_op(self):
        self.assertEqual(self.mark().returncode, 0)
        once = self.lib.read_bytes()
        result = self.mark()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already", result.stdout)
        self.assertEqual(self.lib.read_bytes(), once)

    def test_refuses_a_note_without_shstk(self):
        source = self.dir / "ibt.c"
        source.write_text("int f(void) { return 1; }\n")
        subprocess.run(["cc", "-shared", "-fPIC", "-fcf-protection=branch", "-Wl,-z,ibt",
                        "-o", str(self.lib), str(source)], check=True)
        before = self.lib.read_bytes()
        result = self.mark()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.lib.read_bytes(), before)

    def test_refuses_what_is_not_a_shared_library(self):
        self.lib.write_bytes(b"not an ELF file")
        self.assertNotEqual(self.mark().returncode, 0)


if __name__ == "__main__":
    unittest.main()
