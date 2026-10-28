"""Tests for tools/policy/check_vendor_provenance.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_vendor_provenance", ROOT / "tools/policy/check_vendor_provenance.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules["check_vendor_provenance"] = checker
SPEC.loader.exec_module(checker)


def vendored(root: Path, name: str, version: str = "1.2.3") -> Path:
    """A vendored crate that meets the rule."""
    directory = root / "vendor" / name
    directory.mkdir(parents=True)
    (directory / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "{version}"\n', encoding="utf-8"
    )
    (directory / "LICENSE-MIT").write_text("MIT licence text\n", encoding="utf-8")
    (directory / "PROVENANCE.md").write_text(
        f"# {name} provenance\n\n- Package: `{name}` {version}\n", encoding="utf-8"
    )
    return directory


class VendorProvenanceTests(unittest.TestCase):
    def test_the_repository_meets_the_rule(self) -> None:
        self.assertEqual(checker.check(ROOT), [])

    def test_the_repository_has_vendored_directories_to_check(self) -> None:
        self.assertTrue(any(p.is_dir() for p in (ROOT / "vendor").iterdir()))

    def test_a_conforming_tree_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            vendored(root, "alpha")
            beta = vendored(root, "beta", "0.9.0")
            (beta / "LICENSE-MIT").rename(beta / "COPYING")
            (root / "vendor" / "PATCHES.md").write_text("# Patches\n", encoding="utf-8")
            self.assertEqual(checker.check(root), [])

    def test_missing_licence_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (vendored(root, "alpha") / "LICENSE-MIT").unlink()
            vendored(root, "beta")
            self.assertEqual(
                checker.check(root),
                [
                    "vendor/alpha: no licence file (LICENSE*, LICENCE* or COPYING*); "
                    "copy the upstream text byte for byte"
                ],
            )

    def test_a_licence_directory_is_not_a_licence_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = vendored(root, "alpha")
            (directory / "LICENSE-MIT").unlink()
            (directory / "LICENSES").mkdir()
            self.assertEqual(len(checker.check(root)), 1)

    def test_missing_provenance_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (vendored(root, "alpha") / "PROVENANCE.md").unlink()
            self.assertEqual(
                checker.check(root),
                ["vendor/alpha: no PROVENANCE.md recording upstream version and source"],
            )

    def test_provenance_naming_the_wrong_version_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = vendored(root, "alpha", "0.1.0")
            for stale in ("0.1.1", "10.1.0", "0.1.0.1", "0.1.0-alpha"):
                (directory / "PROVENANCE.md").write_text(
                    f"- Package: `alpha` {stale}\n", encoding="utf-8"
                )
                self.assertEqual(
                    checker.check(root),
                    ["vendor/alpha/PROVENANCE.md: does not name version 0.1.0 from its Cargo.toml"],
                    stale,
                )

    def test_version_at_the_end_of_a_sentence_is_named(self) -> None:
        self.assertTrue(checker.names_version("carried from alpha 0.1.0.", "0.1.0"))
        self.assertTrue(checker.names_version("`alpha-0.1.0.crate`", "0.1.0"))

    def test_nested_workspace_is_checked_at_its_top_level(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = root / "vendor" / "suite"
            (directory / "member").mkdir(parents=True)
            (directory / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["member"]\n\n[workspace.package]\nversion = "2.0.0"\n',
                encoding="utf-8",
            )
            (directory / "member" / "Cargo.toml").write_text(
                '[package]\nname = "member"\nversion.workspace = true\n', encoding="utf-8"
            )
            (directory / "LICENSE").write_text("licence\n", encoding="utf-8")
            (directory / "PROVENANCE.md").write_text("suite 2.0.0\n", encoding="utf-8")
            self.assertEqual(checker.check(root), [])

    def test_directory_without_manifest_still_needs_both_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "vendor" / "assets").mkdir(parents=True)
            self.assertEqual(
                checker.check(root),
                [
                    "vendor/assets: no licence file (LICENSE*, LICENCE* or COPYING*); "
                    "copy the upstream text byte for byte",
                    "vendor/assets: no PROVENANCE.md recording upstream version and source",
                ],
            )

    def test_empty_vendor_fails_rather_than_passing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertEqual(len(checker.check(root)), 1)
            (root / "vendor").mkdir()
            (root / "vendor" / "PATCHES.md").write_text("# Patches\n", encoding="utf-8")
            self.assertEqual(len(checker.check(root)), 1)


if __name__ == "__main__":
    unittest.main()
