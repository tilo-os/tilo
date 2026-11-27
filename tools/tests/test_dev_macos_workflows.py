"""Which apps a Mac's collection selects, and why it leaves others out (dist/macos/collection.py).

A Mac runs an app when the app says it runs there: a native app's `package.platforms`, or a
tilolang package's committed catalogue entry (`fleet.json`), which also says whether a person is
offered it. A package is carried as the tapp its author installs through the Store; a native app
as an `.app` bundle. Nothing is selected apart from what reaches it.
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
import sys

sys.path.insert(0, str(ROOT / "dist" / "macos"))
import collection as c


def write(path: Path, text: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


class SelectionTests(unittest.TestCase):
    def root(self, temp: str) -> Path:
        """A checkout with the tilolang runtime whose platforms every package runs where."""
        root = Path(temp)
        write(root / "crates/lang/tilolang-process/tilo-runtime.toml",
              '[package]\nprovides = "tilolang/0.1.0+tilolang"\nplatforms = ["linux", "macos", "web"]\n')
        write(root / "crates/lang/tilolang/std/app.tl")
        write(root / "crates/lang/tilolang/std/app.ids")
        return root

    def package(self, root: Path, name: str, title: str, *, app: str = "",
                platforms: tuple[str, ...] = ("linux", "macos", "web"), listed: bool = True) -> None:
        folder = root / "apps/tl" / name
        app_id = f"so.tilo.{name}"
        write(folder / "app.tl", f'app {app_id} "{title}" (\n)\n')
        write(folder / "app.ids")
        write(folder / "fleet.json", json.dumps(
            {"id": app_id, "name": title, "platforms": list(platforms), "listed": listed}) + "\n")
        if app:
            write(folder / "app.toml", app)

    def native(self, root: Path, name: str, title: str, *, platforms: str = "") -> None:
        folder = root / "apps" / name
        write(folder / "Cargo.toml", f'[package]\nname = "{name}"\n')
        write(folder / "tilo-app.toml",
              f'[package]\nid = "so.tilo.{name}"\nname = "{title}"\nversion = "1.0.0"\nruntime = "r"\n{platforms}'
              f'[entrypoint]\nbin = "{name}"\n')

    def plan(self, root: Path, selection: str = "") -> dict:
        with patch.object(c.subprocess, "run", return_value=SimpleNamespace(stdout="a" * 40)):
            return c.make_plan(root, selection, "arm64", "development")

    def test_a_package_that_runs_on_macos_is_carried_as_its_tapp(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = self.root(temp)
            self.package(root, "notes", "Notes")
            self.native(root, "tools", "Tools")
            plan = self.plan(root)
            self.assertEqual([a["name"] for a in plan["apps"]], ["tools", "tl-notes"])
            notes = plan["apps"][1]
            self.assertEqual(notes["kind"], "tilolang")
            self.assertEqual(notes["tapp"], "Notes-0.1.0.tapp")
            self.assertTrue(notes["manifest_sha256"])
            self.assertEqual(plan["apps"][0]["bundle"], "Tools.app")
            self.assertEqual(plan["excluded"], [])

    def test_where_an_app_does_not_run_is_named_with_its_reason(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = self.root(temp)
            self.package(root, "server", "Server", platforms=("linux",))
            self.package(root, "service", "Service", listed=False)
            self.package(root, "bridge", "Bridge", app=(
                "[package]\nid = \"so.tilo.bridge\"\noptional-runtimes = [\"bun/1.4.5+bun\"]\n"
                "[entrypoint]\ntype = \"cli\"\nruntime = \"bun/1.4.5+bun\"\nbin = \"bin/bun\"\nscript = \"main.js\"\n"))
            self.package(root, "notes", "Notes")
            self.native(root, "connector", "Connector", platforms='platforms = ["linux"]\nplatform-reason = "Runs on a host."\n')
            plan = self.plan(root)
            self.assertEqual([a["name"] for a in plan["apps"]], ["tl-notes"])
            reasons = {entry["name"]: entry["reason"] for entry in plan["excluded"]}
            self.assertIn("linux", reasons["tl-server"])
            self.assertIn("service", reasons["tl-service"])
            self.assertIn("bun", reasons["tl-bridge"])
            self.assertEqual(reasons["connector"], "Runs on a host.")
            with self.assertRaisesRegex(ValueError, "bun"):
                self.plan(root, "tl-bridge")

    def test_each_app_is_carried_once_under_one_name(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = self.root(temp)
            self.package(root, "one", "Same")
            self.package(root, "two", "same")
            with self.assertRaisesRegex(ValueError, "collision"):
                self.plan(root)


if __name__ == "__main__":
    unittest.main()
