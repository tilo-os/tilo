from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ACTION_PATH = ".forgejo/actions/restore-cargo-downloads"
PREPARE_ACTION_PATH = ".forgejo/actions/prepare-rust-build"
DIRECT_CONSUMERS = {
    "release-apps.yml": 3,  # The scoped Signal build is also a Cargo consumer.
}
PREPARED_CONSUMERS: dict[str, int] = {}


class CargoDownloadCacheActionTests(unittest.TestCase):
    def test_cargo_download_cache_is_one_shared_fail_open_action(self) -> None:
        with self.subTest(phase='action owns the complete fail open cache contract'):
            action = (ROOT / ACTION_PATH / "action.yml").read_text(encoding="utf-8")
            self.assertIn(
                "uses: actions/cache/restore@5a3ec84eff668545956fd18022155c47e93e2684",
                action,
            )
            self.assertIn("continue-on-error: true", action)
            # The cached directories follow the job's CARGO_HOME, so a save
            # step reuses the restore step's paths and the cache version.
            self.assertIn("home=${CARGO_HOME:-$HOME/.cargo}", action)
            for directory in ("registry/index", "registry/cache", "git/db"):
                self.assertEqual(action.count("%s/" + directory), 1)
            self.assertIn("path: ${{ steps.home.outputs.paths }}", action)
            self.assertIn("value: ${{ steps.home.outputs.paths }}", action)
            self.assertIn("default: Cargo.lock", action)
            self.assertIn("key: cargo-deps-${{ hashFiles(inputs.lockfile) }}", action)
            self.assertIn("value: ${{ steps.restore.outputs.cache-primary-key }}", action)
            self.assertNotIn('hashFiles', action.split('outputs:', 1)[1].split('runs:', 1)[0])
            self.assertIn("value: ${{ steps.restore.outputs.cache-hit }}", action)
        with self.subTest(phase='rust build action restores downloads; the runner provides the compiler cache'):
            action = (ROOT / PREPARE_ACTION_PATH / "action.yml").read_text(encoding="utf-8")
            self.assertEqual(action.count(f"uses: ./{ACTION_PATH}"), 1)
            self.assertIn("value: ${{ steps.cargo-downloads.outputs.cache-key }}", action)
            self.assertNotIn("sccache", action)
            self.assertNotIn("compiler_cache_env", action)
            self.assertIn(
                "value: ${{ steps.cargo-downloads.outputs.cache-hit }}",
                action,
            )
        with self.subTest(phase='every cargo download consumer uses a shared action'):
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
            self.assertEqual(remaining_keys, 0)
            for path in workflows.glob("*.y*ml"):
                with self.subTest(workflow=path.name):
                    source = path.read_text(encoding="utf-8")
                    self.assertNotIn("/cargo-home", source)
                    for save in source.split("uses: actions/cache/save@")[1:]:
                        save = save.split("\n      - ", 1)[0]
                        if "cargo-cache" not in save and "downloads.outputs" not in save:
                            continue
                        self.assertRegex(save,
                                         r"path: \$\{\{ steps\.[a-z-]+\.outputs\.(cargo-cache-)?paths \}\}")
        with self.subTest(phase='path filtered consumers track the shared action'):
            workflows = ROOT / ".forgejo" / "workflows"
            expected = {
                "release-apps.yml": (ACTION_PATH,),
            }
            for name, actions in expected.items():
                source = (workflows / name).read_text(encoding="utf-8")
                for action in actions:
                    with self.subTest(workflow=name, action=action):
                        self.assertIn(f'"{action}/**"', source)


if __name__ == "__main__":
    unittest.main()
