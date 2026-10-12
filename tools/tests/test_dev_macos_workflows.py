"""Contract of the workflows on the persistent development Mac."""

from __future__ import annotations

import pathlib
import re
import tempfile
import unittest

from tools.ci.macos_apps import macos_apps

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
MACOS = ("dev-runtime-macos.yml", "dev-apps-macos.yml")


def triggers(source: str) -> str:
    match = re.search(r"^on:\n((?:  .*\n|\s*\n)*)", source, re.M)
    assert match
    return match.group(1)


class DevMacosWorkflowTests(unittest.TestCase):
    def test_jobs_run_on_the_development_label_without_release_credentials(self) -> None:
        for name in MACOS:
            source = (WORKFLOWS / name).read_text()
            with self.subTest(name):
                labels = re.findall(r"^    runs-on: (.+)$", source, re.M)
                self.assertTrue(labels)
                self.assertEqual(set(labels), {"macos-aarch64"})
                self.assertNotIn("id-token", source)
                self.assertNotIn("secrets.", source)

    def test_runtime_check_follows_pull_requests(self) -> None:
        on = triggers((WORKFLOWS / "dev-runtime-macos.yml").read_text())
        self.assertIn("  pull_request:\n", on)
        self.assertIn("  push:\n", on)

    def test_whole_app_build_is_scheduled_not_per_change(self) -> None:
        on = triggers((WORKFLOWS / "dev-apps-macos.yml").read_text())
        self.assertIn("  schedule:\n", on)
        self.assertIn("  workflow_dispatch:\n", on)
        self.assertNotIn("pull_request", on)
        self.assertNotIn("push", on)


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
