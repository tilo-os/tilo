#!/usr/bin/env python3
from pathlib import Path
import sys
import unittest
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_readiness  # noqa: E402


class ReleaseReadinessTests(unittest.TestCase):
    def test_extracts_source_from_immutable_channel(self) -> None:
        sha = "a" * 40
        self.assertEqual(
            release_readiness.channel_source(
                f"https://code.example/api/packages/qos/generic/qos-apps/0.0.0-{sha}/apps.tar.gz"
            ),
            sha,
        )

    def test_rejects_moving_or_unversioned_channel(self) -> None:
        with self.assertRaisesRegex(ValueError, "immutable"):
            release_readiness.channel_source("https://code.example/qos-apps/latest/apps.tar.gz")

    def test_os_boundary_does_not_include_app_only_change(self) -> None:
        self.assertTrue(release_readiness.os_changed(["os/Containerfile"]))
        self.assertFalse(release_readiness.os_changed(["apps/notes/src/main.rs"]))

    @mock.patch.object(release_readiness, "apps_changed")
    @mock.patch.object(release_readiness.desktop_release, "desktop_changed")
    @mock.patch.object(release_readiness, "changed_paths")
    def test_waits_for_each_stale_component_channel(self, changed, desktop, apps) -> None:
        changed.side_effect = [
            ["apps/notes/src/main.rs"],
            ["shell/src/main.rs"],
        ]
        apps.return_value = True
        desktop.return_value = True
        result = release_readiness.readiness(
            f"https://code.example/0.0.0-{'a' * 40}/apps.tar.gz",
            f"https://code.example/0.0.0-{'b' * 40}/desktop.tar.gz",
        )
        self.assertFalse(result["ready"])
        self.assertTrue(result["pending_apps"])
        self.assertTrue(result["pending_desktop"])

    @mock.patch.object(release_readiness, "apps_changed", return_value=False)
    @mock.patch.object(release_readiness.desktop_release, "desktop_changed", return_value=False)
    @mock.patch.object(release_readiness, "changed_paths", return_value=[])
    def test_ready_when_both_channels_cover_head(self, _changed, _desktop, _apps) -> None:
        result = release_readiness.readiness(
            f"https://code.example/0.0.0-{'a' * 40}/apps.tar.gz",
            f"https://code.example/0.0.0-{'b' * 40}/desktop.tar.gz",
        )
        self.assertTrue(result["ready"])


if __name__ == "__main__":
    unittest.main()
