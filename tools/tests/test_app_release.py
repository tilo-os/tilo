#!/usr/bin/env python3
from pathlib import Path
import sys
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_release  # noqa: E402


CATALOG = {name: {} for name in ("files", "notes", "settings")}


class AppReleaseTests(unittest.TestCase):
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
                ["apps/notes/src/main.rs", "Cargo.lock"], CATALOG
            ),
            ["notes"],
        )

    def test_shared_runtime_change_is_left_to_affected_package_ci(self) -> None:
        self.assertEqual(app_release.apps_from_changes(["ui/src/lib.rs"], CATALOG), [])

    def test_os_integrated_browser_change_does_not_use_generic_builder(self) -> None:
        catalog = {**CATALOG, "browser": {}}
        self.assertEqual(
            app_release.apps_from_changes(["apps/browser/src/main.rs"], catalog), []
        )


if __name__ == "__main__":
    unittest.main()
