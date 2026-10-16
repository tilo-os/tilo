"""Tests for tools/policy/check_no_telemetry.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_no_telemetry", ROOT / "tools/policy/check_no_telemetry.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules["check_no_telemetry"] = checker
SPEC.loader.exec_module(checker)


class NoTelemetryTests(unittest.TestCase):
    def test_the_repository_has_no_telemetry_client(self) -> None:
        self.assertEqual(checker.check(ROOT), [])

    def test_a_telemetry_client_and_its_dependents_are_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.lock").write_text(
                "version = 4\n\n"
                '[[package]]\nname = "app"\nversion = "0.1.0"\n'
                'dependencies = [\n "sentry 0.34.0",\n "tracing",\n]\n\n'
                '[[package]]\nname = "sentry"\nversion = "0.34.0"\n'
                'dependencies = [\n "sentry-core",\n]\n\n'
                '[[package]]\nname = "sentry-core"\nversion = "0.34.0"\n\n'
                '[[package]]\nname = "tracing"\nversion = "0.1.41"\n',
                encoding="utf-8",
            )
            self.assertEqual(
                checker.check(root),
                [
                    "Cargo.lock contains sentry 0.34.0",
                    "Cargo.lock contains sentry-core 0.34.0",
                    "app 0.1.0 depends on sentry",
                    "sentry 0.34.0 depends on sentry-core",
                ],
            )


if __name__ == "__main__":
    unittest.main()
