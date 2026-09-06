#!/usr/bin/env python3
from pathlib import Path
import sys
import tempfile
import tomllib
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_release  # noqa: E402


CATALOG = {name: {} for name in ("files", "notes", "settings")}
ROOT = Path(__file__).resolve().parents[2]


class AppReleaseTests(unittest.TestCase):
    def test_main_push_defaults_to_available_native_lane(self) -> None:
        self.assertEqual(
            app_release.architecture_matrix("refs/heads/main", "push"),
            {"include": [{"arch": "x86_64", "runner": "runner"}]},
        )

    def test_main_architecture_switch_and_tags_cover_both(self) -> None:
        both = {
            "include": [
                {"arch": "aarch64", "runner": "runner-arm64"},
                {"arch": "x86_64", "runner": "runner"},
            ]
        }
        self.assertEqual(
            app_release.architecture_matrix(
                "refs/heads/main", "push", main_architectures="x86_64,aarch64"
            ),
            both,
        )
        self.assertEqual(
            app_release.architecture_matrix(
                "refs/tags/apps-v1.0.0", "push", requested="x86_64"
            ),
            both,
        )

    def test_invalid_release_architecture_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "architectures must contain only"):
            app_release.architecture_matrix(
                "refs/heads/main", "push", main_architectures="riscv64"
            )

    def test_wildcard_collection_tracks_catalog(self) -> None:
        cfg = {"collections": {"default": {"include": ["*"]}}}
        self.assertEqual(
            app_release.resolve_collection("default", CATALOG, cfg),
            ["files", "notes", "settings"],
        )

    def test_collection_extension_and_exclusion(self) -> None:
        cfg = {
            "collections": {
                "default": {"include": ["*"]},
                "small": {"extends": ["default"], "exclude": ["settings"]},
            }
        }
        self.assertEqual(
            app_release.resolve_collection("small", CATALOG, cfg),
            ["files", "notes"],
        )

    def test_single_app_component_tag(self) -> None:
        self.assertEqual(
            app_release.app_from_tag("app-notes-v1.2.3", CATALOG), "notes"
        )
        self.assertIsNone(app_release.app_from_tag("apps-v1.2.3", CATALOG))

    def test_direct_app_change_does_not_select_fleet(self) -> None:
        self.assertEqual(
            app_release.apps_from_changes(
                ["apps/notes/src/main.rs"], CATALOG
            ),
            ["notes"],
        )

    def test_lockfile_change_selects_the_fleet(self) -> None:
        self.assertEqual(
            app_release.apps_from_changes(["Cargo.lock"], CATALOG),
            sorted(CATALOG),
        )

    def test_global_change_excludes_os_integrated_apps(self) -> None:
        catalog = {**CATALOG, "browser": {}}
        self.assertEqual(
            app_release.apps_from_changes(["Cargo.lock"], catalog),
            sorted(CATALOG),
        )

    def test_shared_runtime_change_selects_reverse_dependent_apps(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = {
                "packages": [
                    {
                        "name": "ui",
                        "manifest_path": str(root / "ui/Cargo.toml"),
                        "dependencies": [],
                    },
                    {
                        "name": "notes-bin",
                        "manifest_path": str(root / "apps/notes/Cargo.toml"),
                        "dependencies": [{"path": str(root / "ui")}],
                    },
                    {
                        "name": "settings-bin",
                        "manifest_path": str(root / "apps/settings/Cargo.toml"),
                        "dependencies": [],
                    },
                ]
            }
            catalog = {
                "notes": {"package": "notes-bin"},
                "settings": {"package": "settings-bin"},
            }
            self.assertEqual(
                app_release.apps_from_changes(
                    ["ui/src/lib.rs"], catalog, metadata, root
                ),
                ["notes"],
            )

    def test_os_integrated_browser_change_does_not_use_generic_builder(self) -> None:
        catalog = {**CATALOG, "browser": {}}
        self.assertEqual(
            app_release.apps_from_changes(["apps/browser/src/main.rs"], catalog), []
        )

    def test_release_graph_keeps_image_codecs_explicit(self) -> None:
        """One app must not enable unused image codecs in every thin qpkg."""
        expected = {
            ROOT / "apps/slides/Cargo.toml": ["jpeg", "png", "gif", "webp"],
            ROOT / "crates/doc/qdoc-pdf/Cargo.toml": ["jpeg", "png"],
        }
        for manifest, formats in expected.items():
            dependency = tomllib.loads(manifest.read_text())["dependencies"]["image"]
            self.assertFalse(dependency["default-features"], manifest)
            self.assertEqual(dependency["features"], formats, manifest)


if __name__ == "__main__":
    unittest.main()
