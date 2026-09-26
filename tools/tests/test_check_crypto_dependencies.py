#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "policy/check_crypto_dependencies.py"
SPEC = importlib.util.spec_from_file_location("check_crypto_dependencies", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class CryptoDependencyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, relative: str, contents: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    def workspace(self, members: list[str], dependencies: str = "") -> None:
        rows = "\n".join(f'    "{member}",' for member in members)
        self.write(
            "Cargo.toml",
            f"[workspace]\nmembers = [\n{rows}\n]\n\n[workspace.dependencies]\n{dependencies}\n",
        )

    def crate(self, relative: str, name: str, body: str) -> None:
        self.write(
            f"{relative}/Cargo.toml",
            f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n{body}\n',
        )

    def test_crypto_primitives_are_confined_to_the_seal_crate_in_every_manifest_form(self) -> None:
        with self.subTest(phase='seal crate may use primitives'):
            self.doCleanups()
            self.setUp()
            self.workspace(["crates/system/tilo-seal"])
            self.crate(
                "crates/system/tilo-seal",
                "tilo-seal",
                '[dependencies]\nchacha20poly1305 = "0.11"\nhpke-rs = "0.7"\nargon2 = "0.6"\n',
            )
            self.assertEqual(checker.violations(self.root), [])
        with self.subTest(phase='direct dependency is rejected'):
            self.doCleanups()
            self.setUp()
            self.workspace(["apps/vault"])
            self.crate("apps/vault", "vault", '[dependencies]\nx25519-dalek = "3"\n')
            errors = checker.violations(self.root)
            self.assertEqual(len(errors), 1)
            self.assertIn("x25519-dalek", errors[0])
        with self.subTest(phase='workspace inheritance and dev and target tables are checked'):
            self.doCleanups()
            self.setUp()
            self.workspace(
                ["apps/vault"],
                'chacha20poly1305 = "0.11"\ncipher = { version = "0.6", package = "argon2" }\n',
            )
            self.crate(
                "apps/vault",
                "vault",
                "[dev-dependencies]\nchacha20poly1305.workspace = true\n\n"
                "[target.'cfg(unix)'.dependencies]\ncipher = { workspace = true }\n",
            )
            errors = checker.violations(self.root)
            self.assertEqual(len(errors), 2, errors)
            self.assertTrue(any("argon2" in error for error in errors))
            self.assertTrue(any("dev-dependencies" in error for error in errors))
        with self.subTest(phase='rename does not hide a primitive'):
            self.doCleanups()
            self.setUp()
            self.workspace(["apps/vault"])
            self.crate(
                "apps/vault",
                "vault",
                '[dependencies]\ninnocent = { version = "2", package = "x25519-dalek" }\n',
            )
            self.assertEqual(len(checker.violations(self.root)), 1)
        with self.subTest(phase='protocol libraries are not primitives'):
            self.doCleanups()
            self.setUp()
            self.workspace(["apps/tube"])
            self.crate("apps/tube", "tube", '[dependencies]\nsnow = "0.9"\nopenmls = "0.9"\n')
            self.assertEqual(checker.violations(self.root), [])


if __name__ == "__main__":
    unittest.main()
