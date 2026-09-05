#!/usr/bin/env python3
from pathlib import Path
import tempfile
import unittest

import sys


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import desktop_release  # noqa: E402


class DesktopReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

        def package(name: str, rel: str, deps: list[str] | None = None) -> dict:
            directory = self.root / rel
            return {
                "name": name,
                "manifest_path": str(directory / "Cargo.toml"),
                "targets": [{"name": name}],
                "dependencies": [
                    {"name": dep, "path": str(self.root / dep)} for dep in (deps or [])
                ],
            }

        self.metadata = {
            "packages": [
                package("shared", "crates/shared"),
                package("shell", "shell", ["crates/shared"]),
                package("launcher", "crates/packaging/launcher"),
                package("qos-unlock-helper", "crates/session/qos-unlock-helper"),
                package("notes", "apps/notes"),
            ]
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_shared_dependency_advances_desktop(self) -> None:
        self.assertTrue(
            desktop_release.desktop_changed(
                ["crates/shared/src/lib.rs"], self.metadata, self.root
            )
        )

    def test_unrelated_app_does_not_advance_desktop(self) -> None:
        self.assertFalse(
            desktop_release.desktop_changed(
                ["apps/notes/src/main.rs"], self.metadata, self.root
            )
        )

    def test_direct_packaging_input_advances_desktop_without_metadata(self) -> None:
        self.assertTrue(desktop_release.desktop_changed(["dist/rpm/qos-session.spec"]))
        self.assertTrue(
            desktop_release.desktop_changed(
                [".forgejo/actions/package-desktop-lane/action.yml"]
            )
        )

    def test_global_cargo_input_advances_desktop(self) -> None:
        self.assertTrue(
            desktop_release.desktop_changed([".cargo/config.toml"], self.metadata, self.root)
        )

    def test_unowned_deleted_workspace_path_is_conservative(self) -> None:
        self.assertTrue(
            desktop_release.desktop_changed(
                ["crates/deleted/src/lib.rs"], self.metadata, self.root
            )
        )


if __name__ == "__main__":
    unittest.main()
