"""Which apps the macOS builds select (tools/ci/macos_apps.py)."""

from __future__ import annotations

import pathlib
import tempfile
import unittest

from tools.ci.macos_apps import macos_apps
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))


class MacosAppsTests(unittest.TestCase):
    def app(self, root: pathlib.Path, directory: str, package: str, manifest: str) -> None:
        (root / "apps" / directory).mkdir(parents=True)
        (root / "apps" / directory / "Cargo.toml").write_text(f'[package]\nname = "{package}"\n')
        (root / "apps" / directory / "tilo-app.toml").write_text(manifest)

    def test_selects_apps_that_list_macos_or_no_platforms(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.app(root, "default", "default-app", "[package]\n")
            self.app(root, "mac", "mac-app", '[package]\nplatforms = ["macos"]\n')
            self.app(root, "linux", "linux-app", '[package]\nplatforms = ["linux", "web"]\n')
            (root / "apps/manifest-only").mkdir()
            (root / "apps/manifest-only/tilo-app.toml").write_text("[package]\n")
            self.assertEqual(macos_apps(root), ["default-app", "mac-app"])

    def test_repository_has_macos_apps(self) -> None:
        self.assertIn("calculator", macos_apps())


if __name__ == "__main__":
    unittest.main()
