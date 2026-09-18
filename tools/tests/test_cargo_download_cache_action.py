from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ACTION_PATH = ".forgejo/actions/restore-cargo-downloads"
PREPARE_ACTION_PATH = ".forgejo/actions/prepare-rust-build"
DIRECT_CONSUMERS = {
    "build-builder-image.yml": 2,
    "canonical-verifier-bundle.yml": 1,
    "release-apps.yml": 1,
}
PREPARED_CONSUMERS = {
    "check.yml": 1,
    "legacy-app-e2e.yml": 1,
    "nightly.yml": 1,
    "refresh-visual-baselines.yml": 1,
    "weekly-quality.yml": 1,
}


class CargoDownloadCacheActionTests(unittest.TestCase):
    def test_action_owns_the_complete_fail_open_cache_contract(self) -> None:
        action = (ROOT / ACTION_PATH / "action.yml").read_text(encoding="utf-8")
        self.assertIn(
            "uses: actions/cache/restore@5a3ec84eff668545956fd18022155c47e93e2684",
            action,
        )
        self.assertIn("continue-on-error: true", action)
        self.assertEqual(action.count("/cargo-home/registry/index"), 1)
        self.assertEqual(action.count("/cargo-home/registry/cache"), 1)
        self.assertEqual(action.count("/cargo-home/git/db"), 1)
        self.assertIn("key: cargo-deps-${{ hashFiles('Cargo.lock') }}", action)
        self.assertIn("value: ${{ steps.restore.outputs.cache-hit }}", action)

    def test_rust_build_action_composes_download_and_compiler_caches(self) -> None:
        action = (ROOT / PREPARE_ACTION_PATH / "action.yml").read_text(encoding="utf-8")
        self.assertEqual(action.count(f"uses: ./{ACTION_PATH}"), 1)
        self.assertEqual(action.count("bash tools/ci/sccache.sh prepare"), 1)
        self.assertIn(
            "value: ${{ steps.cargo-downloads.outputs.cache-hit }}",
            action,
        )

    def test_every_cargo_download_consumer_uses_a_shared_action(self) -> None:
        workflows = ROOT / ".forgejo" / "workflows"
        for name, expected_uses in DIRECT_CONSUMERS.items():
            with self.subTest(workflow=name):
                source = (workflows / name).read_text(encoding="utf-8")
                self.assertEqual(
                    source.count(f"uses: ./{ACTION_PATH}"),
                    expected_uses,
                )
        for name, expected_uses in PREPARED_CONSUMERS.items():
            with self.subTest(workflow=name):
                source = (workflows / name).read_text(encoding="utf-8")
                self.assertEqual(
                    source.count(f"uses: ./{PREPARE_ACTION_PATH}"),
                    expected_uses,
                )
                self.assertNotIn(f"uses: ./{ACTION_PATH}", source)
                self.assertNotIn("bash tools/ci/sccache.sh prepare", source)

        # Only the explicit save steps retain a cache key in workflow YAML.
        remaining_keys = sum(
            path.read_text(encoding="utf-8").count("key: cargo-deps-")
            for path in workflows.glob("*.y*ml")
        )
        self.assertEqual(remaining_keys, 2)

    def test_path_filtered_consumers_track_the_shared_action(self) -> None:
        workflows = ROOT / ".forgejo" / "workflows"
        expected = {
            "build-builder-image.yml": (ACTION_PATH,),
            "legacy-app-e2e.yml": (ACTION_PATH, PREPARE_ACTION_PATH),
            "release-apps.yml": (ACTION_PATH,),
        }
        for name, actions in expected.items():
            source = (workflows / name).read_text(encoding="utf-8")
            for action in actions:
                with self.subTest(workflow=name, action=action):
                    self.assertIn(f'"{action}/**"', source)


if __name__ == "__main__":
    unittest.main()
