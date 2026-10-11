"""tools/apps/compare_build_outputs.py: two builds are identical or the report says where not."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import tarfile
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("compare_build_outputs.py")
spec = importlib.util.spec_from_file_location("compare_build_outputs", SCRIPT)
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


def tapp(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w", format=tarfile.GNU_FORMAT) as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size, info.mode = len(data), 0o644
            archive.addfile(info, io.BytesIO(data))


class CompareBuildOutputs(unittest.TestCase):
    def setUp(self) -> None:
        self.work = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.first, self.second = self.work / "first", self.work / "second"
        for root in (self.first, self.second):
            (root / "tapp").mkdir(parents=True)
            (root / "runtime/lib").mkdir(parents=True)
            (root / "runtime/lib/libruntime_dylib.so").write_bytes(b"\x7fELF runtime")
            (root / "runtime/lib/libstd.so").symlink_to("libruntime_dylib.so")
            tapp(root / "tapp/calculator.tapp", {"tapp.toml": b"format = 1", "signed-root": b"root",
                                                 "blobs/aa": b"binary"})
            (root / "work").mkdir()
            (root / "work/publisher.json").write_text(f"secret of {root.name}")

    def run_main(self, *extra: str) -> tuple[int, str]:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(io.StringIO()):
            status = compare.main([str(self.first), str(self.second), "--ignore", "work", *extra])
        return status, stderr.getvalue()

    def test_identical_builds_pass(self) -> None:
        self.assertEqual(self.run_main(), (0, ""))

    def test_ignored_names_are_not_compared_but_everything_else_is(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(compare.main([str(self.first), str(self.second)]), 1)
        self.assertIn("work/publisher.json: file", stderr.getvalue())

    def test_a_differing_tapp_names_the_differing_members(self) -> None:
        tapp(self.second / "tapp/calculator.tapp", {"tapp.toml": b"format = 1", "signed-root": b"root2",
                                                    "blobs/bb": b"binary2"})
        status, report = self.run_main()
        self.assertEqual(status, 1)
        self.assertIn("tapp/calculator.tapp: file", report)
        self.assertIn("member signed-root:", report)
        self.assertIn("member blobs/aa:", report)
        self.assertIn("member blobs/bb: absent |", report)
        self.assertNotIn("member tapp.toml", report)

    def test_a_differing_binary_names_the_first_differing_byte(self) -> None:
        (self.second / "runtime/lib/libruntime_dylib.so").write_bytes(b"\x7fELF runtimf")
        status, report = self.run_main()
        self.assertEqual(status, 1)
        self.assertIn("first differing byte at offset 11", report)

    def test_mode_links_and_missing_paths_count(self) -> None:
        (self.second / "runtime/lib/libruntime_dylib.so").chmod(0o755)
        (self.second / "runtime/lib/libstd.so").unlink()
        (self.second / "runtime/lib/libstd.so").symlink_to("elsewhere")
        (self.second / "runtime/extra").write_text("new")
        status, report = self.run_main()
        self.assertEqual(status, 1)
        self.assertIn("runtime/lib/libruntime_dylib.so: file 644", report)
        self.assertIn("runtime/lib/libstd.so: link 0 libruntime_dylib.so | link 0 elsewhere", report)
        self.assertIn("runtime/extra: only in the second build", report)

    def test_an_empty_first_build_fails_rather_than_passing_vacuously(self) -> None:
        empty = self.work / "empty"
        empty.mkdir()
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(compare.main([str(empty), str(empty)]), 1)
        self.assertIn("holds nothing to compare", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
