"""Tests for tools/policy/check_tls_stack.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_tls_stack", ROOT / "tools/policy/check_tls_stack.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules["check_tls_stack"] = checker
SPEC.loader.exec_module(checker)


class TlsStackTests(unittest.TestCase):
    def test_the_repository_has_no_openssl(self) -> None:
        self.assertEqual(checker.check(ROOT), [])

    def test_openssl_and_its_dependents_are_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.lock").write_text(
                "version = 4\n\n"
                '[[package]]\nname = "app"\nversion = "0.1.0"\n'
                'dependencies = [\n "reqwest",\n]\n\n'
                '[[package]]\nname = "reqwest"\nversion = "0.12.0"\n'
                'dependencies = [\n "native-tls",\n "rustls",\n]\n\n'
                '[[package]]\nname = "native-tls"\nversion = "0.2.14"\n'
                'dependencies = [\n "openssl 0.10.73",\n]\n\n'
                '[[package]]\nname = "openssl"\nversion = "0.10.73"\n\n'
                '[[package]]\nname = "rustls"\nversion = "0.23.31"\n',
                encoding="utf-8",
            )
            self.assertEqual(
                checker.check(root),
                [
                    "Cargo.lock: contains native-tls 0.2.14",
                    "Cargo.lock: contains openssl 0.10.73",
                    "Cargo.lock: native-tls 0.2.14 depends on openssl",
                    "Cargo.lock: reqwest 0.12.0 depends on native-tls",
                ],
            )


if __name__ == "__main__":
    unittest.main()
