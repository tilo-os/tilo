"""Contract of the checks on t's Mac (t.toml)."""

from __future__ import annotations

import pathlib
import tempfile
import tomllib
import unittest

from tools.ci.macos_apps import macos_apps

ROOT = pathlib.Path(__file__).resolve().parents[2]
CHECKS = {check["name"]: check for check in tomllib.loads((ROOT / "t.toml").read_text())["check"]}
MACOS = ("runtime-macos", "sdk-macos", "apps-macos")


class DevMacosCheckTests(unittest.TestCase):
    def test_checks_run_on_the_mac_without_release_credentials(self) -> None:
        for name in MACOS:
            check = CHECKS[name]
            with self.subTest(name):
                self.assertIn("os=macos", check["requires"])
                self.assertNotIn("secrets", check)
                self.assertNotIn("id_token", check)
                self.assertFalse(check.get("default", True))

    def test_app_checks_prepare_the_fonts_their_tests_render_with(self) -> None:
        for name in ("sdk-macos", "apps-macos"):
            with self.subTest(name):
                self.assertIn("tools/font-catalog.py root", CHECKS[name]["run"])
                self.assertIn("TILO_FONT_ASSETS_OFFLINE=1", CHECKS[name]["run"])

    def test_runtime_check_follows_main(self) -> None:
        self.assertEqual(CHECKS["runtime-macos"]["on"], ["main"])

    def test_whole_app_build_is_scheduled_not_per_change(self) -> None:
        for name in ("sdk-macos", "apps-macos"):
            with self.subTest(name):
                on = CHECKS[name]["on"]
                self.assertEqual(len(on), 1)
                self.assertTrue(on[0].startswith("schedule:"))


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
