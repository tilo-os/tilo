"""Include artifact handoff regressions in the existing Cargo-free policy gate."""

from pathlib import Path
import unittest


def load_tests(loader, tests, pattern):
    source = Path(__file__).resolve().parents[2] / "scripts/vm-acceptance"
    return unittest.TestLoader().discover(str(source), pattern="test_layer_artifacts.py")
