"""Tests for tools/policy/check_crypto_generation.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_crypto_generation", ROOT / "tools/policy/check_crypto_generation.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules["check_crypto_generation"] = checker
SPEC.loader.exec_module(checker)

REGISTRY = 'source = "registry+https://github.com/rust-lang/crates.io-index"'


def package(name: str, version: str, dependencies: list[str], *, local: bool) -> str:
    rows = "".join(f' "{dependency}",\n' for dependency in dependencies)
    source = "" if local else f"{REGISTRY}\n"
    deps = f"dependencies = [\n{rows}]\n" if dependencies else ""
    return f'[[package]]\nname = "{name}"\nversion = "{version}"\n{source}{deps}\n'


class CryptoGenerationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def lock(self, *packages: str) -> None:
        body = "".join(packages) + package("sha2", "0.10.9", [], local=False)
        body += package("sha2", "0.11.0", [], local=False)
        body += package("rand_core", "0.10.0", [], local=False)
        (self.root / "Cargo.lock").write_text("version = 4\n\n" + body, encoding="utf-8")

    def test_the_repository_is_on_the_current_generation(self) -> None:
        self.assertEqual(checker.check(ROOT), ([], []))

    def test_old_generation_direct_edge_of_an_own_crate_fails(self) -> None:
        self.lock(
            package("tilo-crypto", "0.1.0", ["sha2 0.10.9"], local=True),
            package("app", "0.1.0", ["sha2 0.10.9", "rand_core"], local=True),
        )
        violations, stale = checker.check(self.root)
        self.assertEqual(
            [(v.package, v.dependency, v.version) for v in violations],
            [("app", "sha2", "0.10.9")],
        )
        self.assertEqual(stale, [])

    def test_registry_vendored_and_hakari_packages_are_exempt(self) -> None:
        vendored = self.root / "vendor/upstream/Cargo.toml"
        vendored.parent.mkdir(parents=True)
        vendored.write_text(
            textwrap.dedent(
                """
                [package]
                name = "upstream"
                version = "1.0.0"
                """
            ),
            encoding="utf-8",
        )
        self.lock(
            package("tilo-crypto", "0.1.0", ["sha2 0.10.9"], local=True),
            package("upstream", "1.0.0", ["sha2 0.10.9"], local=True),
            package("workspace-hack", "0.1.0", ["sha2 0.10.9"], local=True),
            package("openmls", "0.6.0", ["sha2 0.10.9"], local=False),
        )
        self.assertEqual(checker.check(self.root), ([], []))

    def test_a_sanctioned_edge_that_is_gone_fails(self) -> None:
        self.lock(package("tilo-crypto", "0.1.0", ["sha2 0.11.0"], local=True))
        violations, stale = checker.check(self.root)
        self.assertEqual(violations, [])
        self.assertEqual(stale, [("tilo-crypto", "sha2", "0.10.")])


if __name__ == "__main__":
    unittest.main()
