from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ACTION_PATH = ".forgejo/actions/install-cosign"
CONSUMERS = {
    "build-builder-image.yml": 2,
    "build-image.yml": 1,
    "build-kernel.yml": 2,
    "snapshot-kernel-buildroot.yml": 3,
    "snapshot-rpms.yml": 1,
    "snapshot-wpe-buildroot.yml": 3,
    "tilo-relay.yaml": 2,
}


class CosignInstallActionTests(unittest.TestCase):
    def test_action_owns_restore_verified_install_and_save(self) -> None:
        action = (ROOT / ACTION_PATH / "action.yml").read_text(encoding="utf-8")

        self.assertEqual(action.count("actions/cache/restore@5a3ec84e"), 1)
        self.assertEqual(action.count("actions/cache/save@5a3ec84e"), 1)
        self.assertEqual(action.count("continue-on-error: true"), 2)
        self.assertEqual(action.count("./os/dev/install-cached-cosign.sh"), 1)
        self.assertEqual(action.count("path: os/external-assets/cosign-cache"), 2)
        self.assertEqual(action.count("key: cosign-v2.5.2-${{ inputs.cache-architecture }}-"), 2)
        self.assertIn("if: steps.restore.outputs.cache-hit != 'true'", action)

    def test_workflows_do_not_reimplement_the_cosign_cache_protocol(self) -> None:
        workflows = ROOT / ".forgejo" / "workflows"
        for name, expected_uses in CONSUMERS.items():
            with self.subTest(workflow=name):
                source = (workflows / name).read_text(encoding="utf-8")
                self.assertEqual(source.count(f"uses: ./{ACTION_PATH}"), expected_uses)

        combined = "\n".join(
            path.read_text(encoding="utf-8") for path in workflows.glob("*.y*ml")
        )
        self.assertNotIn("path: os/external-assets/cosign-cache", combined)

    def test_path_filtered_consumers_track_the_shared_action(self) -> None:
        workflows = ROOT / ".forgejo" / "workflows"
        for name in (
            "build-builder-image.yml",
            "build-kernel.yml",
            "snapshot-kernel-buildroot.yml",
            "snapshot-rpms.yml",
            "snapshot-wpe-buildroot.yml",
            "tilo-relay.yaml",
        ):
            with self.subTest(workflow=name):
                source = (workflows / name).read_text(encoding="utf-8")
                self.assertIn(f'"{ACTION_PATH}/**"', source)


if __name__ == "__main__":
    unittest.main()
