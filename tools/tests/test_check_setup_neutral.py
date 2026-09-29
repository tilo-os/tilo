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
