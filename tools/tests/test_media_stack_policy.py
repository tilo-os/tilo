"""Tests for tools/policy/check_media_stack.py."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_media_stack", ROOT / "tools/policy/check_media_stack.py"
)
assert SPEC and SPEC.loader
check_media_stack = importlib.util.module_from_spec(SPEC)
sys.modules["check_media_stack"] = check_media_stack
SPEC.loader.exec_module(check_media_stack)


def write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(text), encoding="utf-8")


class MediaStackPolicyTests(unittest.TestCase):
    def test_codec_crates_are_reported_in_every_dependency_form(self) -> None:
        with self.subTest(phase='the repository carries no in tree codec crate'):
            self.assertEqual(check_media_stack.check(ROOT), [])
        with self.subTest(phase='a direct dependency is reported'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                write(root, "Cargo.toml", '[workspace]\nmembers = ["a"]\n')
                write(
                    root,
                    "a/Cargo.toml",
                    """
                [package]
                name = "a"
                [dependencies]
                openh264 = { version = "0.9", features = ["source"] }
                """,
                )
                found = check_media_stack.check(root)
                self.assertEqual([(v.path, v.name) for v in found], [("a/Cargo.toml", "openh264")])
        with self.subTest(phase='renamed optional and target dependencies are reported'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                write(
                    root,
                    "Cargo.toml",
                    """
                [workspace.dependencies]
                av = { package = "ffmpeg-next", version = "8" }
                [target.'cfg(unix)'.dependencies]
                ffmpeg-sys-next = "8"
                """,
                )
                names = sorted(v.name for v in check_media_stack.check(root))
                self.assertEqual(names, ["ffmpeg-next", "ffmpeg-sys-next"])
        with self.subTest(phase='a transitive package in the lock is reported'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                write(root, "Cargo.toml", "[workspace]\n")
                write(
                    root,
                    "Cargo.lock",
                    """
                version = 4
                [[package]]
                name = "openh264-sys2"
                version = "0.9.6"
                """,
                )
                found = check_media_stack.check(root)
                self.assertEqual([(v.path, v.name) for v in found], [("Cargo.lock", "openh264-sys2")])


if __name__ == "__main__":
    unittest.main()
