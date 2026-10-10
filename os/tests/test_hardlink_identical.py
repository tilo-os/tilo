#!/usr/bin/env python3
"""os/scripts/hardlink-identical.py merges only files no reader can tell apart."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/scripts/hardlink-identical.py"
spec = importlib.util.spec_from_file_location("hardlink_identical", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def write(path: Path, data: bytes, mode: int = 0o644) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(mode)
    return path


def inode(path: Path) -> int:
    return path.lstat().st_ino


class HardlinkIdenticalTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_identical_files_share_the_first_names_inode(self):
        first = write(self.root / "a/LC_COLLATE", b"collation" * 1000)
        second = write(self.root / "b/LC_COLLATE", b"collation" * 1000)
        third = write(self.root / "c/d/LC_COLLATE", b"collation" * 1000)
        original = inode(first)
        self.assertEqual(module.merge([str(self.root)]), 2 * 9000)
        self.assertEqual({inode(first), inode(second), inode(third)}, {original})
        self.assertEqual(third.read_bytes(), b"collation" * 1000)

    def test_files_a_reader_could_tell_apart_stay_separate(self):
        plain = write(self.root / "plain", b"same bytes")
        executable = write(self.root / "executable", b"same bytes", 0o755)
        different = write(self.root / "different", b"other byte")
        empty_one = write(self.root / "empty-one", b"")
        empty_two = write(self.root / "empty-two", b"")
        link = self.root / "symlink"
        link.symlink_to("plain")
        self.assertEqual(module.merge([str(self.root)]), 0)
        self.assertEqual(len({inode(p) for p in (plain, executable, different, empty_one, empty_two)}), 5)
        self.assertTrue(link.is_symlink())
        self.assertEqual(executable.stat().st_mode & 0o777, 0o755)

    def test_names_that_already_shared_an_inode_move_together(self):
        # The font catalog's three names are one inode before this runs
        #; merging a fourth identical file must not split them.
        catalog = write(self.root / "runtime/lib/fonts/Noto.ttf", b"font" * 500)
        os.link(catalog, self.root / "share-fonts-Noto.ttf")
        stray = write(self.root / "aaa/Noto.ttf", b"font" * 500)
        module.merge([str(self.root)])
        names = [catalog, self.root / "share-fonts-Noto.ttf", stray]
        self.assertEqual(len({inode(p) for p in names}), 1)
        self.assertEqual(catalog.lstat().st_nlink, 3)

    def test_merging_is_deterministic_and_idempotent(self):
        for name in ("z", "m", "a"):
            write(self.root / name / "file", b"payload" * 100)
        module.merge([str(self.root)])
        kept = inode(self.root / "a/file")
        self.assertEqual(inode(self.root / "z/file"), kept)
        self.assertEqual(module.merge([str(self.root)]), 0)
        self.assertEqual([p.name for p in self.root.rglob("*") if p.name.endswith("hardlink-identical")], [])

    def test_the_command_line_reports_and_refuses_a_missing_root(self):
        write(self.root / "x", b"dup")
        write(self.root / "y", b"dup")
        result = subprocess.run([sys.executable, str(SCRIPT), str(self.root)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("3 bytes", result.stdout)
        missing = subprocess.run([sys.executable, str(SCRIPT), str(self.root / "missing")],
                                 capture_output=True, text=True)
        self.assertEqual(missing.returncode, 1)
        self.assertIn("is not a directory", missing.stderr)


if __name__ == "__main__":
    unittest.main()
