"""The extension's build refuses a Proton whose libraries the system does not carry."""

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "os/dev/proton_libraries.py"
SPEC = importlib.util.spec_from_file_location("proton_libraries", SCRIPT)
LIBRARIES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIBRARIES)

# The 20 header bytes the check reads: ELF, 64-bit, little-endian, EM_X86_64.
X86_64 = b"\x7fELF\x02\x01\x01" + b"\x00" * 9 + b"\x02\x00" + (62).to_bytes(2, "little")
I386 = b"\x7fELF\x01\x01\x01" + b"\x00" * 9 + b"\x02\x00" + (3).to_bytes(2, "little")


class ProtonLibrariesTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        self.proton = self.root / "tree/usr/lib/tilo/windows-runners/proton/UMU-Proton-10.0-4"
        self.tree = self.root / "tree"
        self.base = self.root / "base"
        self.write(self.proton / "files/bin/wine", X86_64)
        self.write(self.proton / "files/lib/wine/x86_64-unix/winex11.so", X86_64)
        self.write(self.proton / "files/lib/wine/i386-unix/winex11.so", I386)
        self.write(self.proton / "files/lib/x86_64-linux-gnu/libgstreamer-1.0.so.0", X86_64)
        self.write(self.proton / "files/lib/x86_64-linux-gnu/notes.txt", b"not a library")
        for name in LIBRARIES.LOADED_BY_NAME:
            self.write(self.base / "usr/lib64" / name, X86_64)
        self.write(self.base / LIBRARIES.INTERPRETER, b"#!")
        self.listed = {}

    def write(self, path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def listing(self, command):
        return self.listed.get(Path(command[-1]).name, "\tlibc.so.6 => /lib64/libc.so.6\n")

    def missing(self):
        return LIBRARIES.missing(self.proton, self.tree, self.base, listing=self.listing)

    def test_a_system_with_every_library_passes(self):
        self.assertEqual(self.missing(), [])
        checked = [path.name for path in LIBRARIES.proton_elves(self.proton)]
        self.assertEqual(checked, ["wine", "winex11.so", "libgstreamer-1.0.so.0"])

    def test_a_library_loaded_by_name_that_nothing_carries_is_named(self):
        (self.base / "usr/lib64/libvulkan.so.1").unlink()
        self.assertEqual(self.missing(), ["libvulkan.so.1"])
        # The extension may carry it instead of the base image.
        self.write(self.tree / "usr/lib64/libvulkan.so.1", X86_64)
        self.assertEqual(self.missing(), [])

    def test_a_needed_library_the_loader_cannot_find_is_named(self):
        self.listed["winex11.so"] = "\tlibXi.so.6 => not found\n\tlibc.so.6 => /lib64/libc.so.6\n"
        self.assertEqual(self.missing(), ["libXi.so.6"])

    def test_protons_script_has_the_system_python(self):
        (self.base / LIBRARIES.INTERPRETER).unlink()
        self.assertEqual(self.missing(), ["/usr/bin/python3"])


if __name__ == "__main__":
    unittest.main()
