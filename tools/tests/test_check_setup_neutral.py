#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "policy/check_setup_neutral.py"
SPEC = importlib.util.spec_from_file_location("check_setup_neutral", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = checker
SPEC.loader.exec_module(checker)

WRAPPER = "r" + "cargo"


class SetupNeutralTests(unittest.TestCase):
    def test_the_wrapper_is_rejected_in_any_spelling(self) -> None:
        for line in (
            f"{WRAPPER} test -p fs",
            f"export {WRAPPER.upper()}_ENV_X=1",
            f"~/.config/{WRAPPER}/config.toml",
            f"CARGO={WRAPPER}",
        ):
            self.assertTrue(checker.line_violations(line), line)

    def test_words_that_merely_contain_it_pass(self) -> None:
        self.assertEqual(checker.line_violations("supercargoes"), [])
        self.assertEqual(checker.line_violations("cargo test --locked"), [])

    def test_only_placeholder_homes_pass(self) -> None:
        self.assertEqual(checker.line_violations("/var/home/you/src"), [])
        self.assertEqual(checker.line_violations("/var/home/<user>"), [])
        self.assertTrue(checker.line_violations("/var/" + "home/jdoe/tilo"))

    def test_macos_and_windows_homes_need_placeholders_too(self) -> None:
        for line in (
            "/Users/you/src/tilo",
            r"C:\Users\you\.ssh\vm_ed25519",
            "C:/Users/Alice/My notes.txt",
            "/c/Users/you/src/tilo",
            "GET /api/v1/admin/Users/jdoe",
        ):
            self.assertEqual(checker.line_violations(line), [], line)
        for line in (
            "/" + "Users/jdoe/dev/tilo",
            "C:" + r"\Users\jdoe\msys64",
            "C:/" + "Users/jdoe/msys64/clangarm64",
            "/c/" + "Users/jdoe/keys/vm_ed25519",
        ):
            self.assertTrue(checker.line_violations(line), line)

    def test_private_scratch_directories_are_rejected(self) -> None:
        self.assertTrue(checker.line_violations("see `.super" + "powers/sdd/task-4.md`"))
        self.assertEqual(checker.line_violations("docs/superpowers/plans/x.md"), [])
        self.assertEqual(checker.line_violations("/.super" + "powers/", ignore_file=True), [])

    def test_tracked_text_files_are_scanned(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "ok.md").write_text("cargo build\n", encoding="utf-8")
            (root / "bad.sh").write_text(f"#!/bin/sh\n{WRAPPER} build\n", encoding="utf-8")
            (root / "blob.bin").write_bytes(f"\0{WRAPPER}".encode())
            (root / "untracked.md").write_text(f"{WRAPPER}\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", str(root), "add", "ok.md", "bad.sh", "blob.bin"], check=True
            )
            found = checker.check(root)
            self.assertEqual([(v.path, v.line) for v in found], [("bad.sh", 2)])


if __name__ == "__main__":
    unittest.main()
