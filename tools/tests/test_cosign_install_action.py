from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ACTION_PATH = ".forgejo/actions/install-cosign"
CONSUMERS = {
    "build-ci-builder.yml": 2,
    "build-image.yml": 2,
    "build-kernel.yml": 3,
    "build-buildroots.yml": 3,
    "build-rpms.yml": 1,
    "release-relay.yml": 2,
}


class CosignInstallActionTests(unittest.TestCase):
    def test_cosign_cache_protocol_lives_only_in_the_shared_action(self):
        with self.subTest(phase='action owns restore verified install and save'):
            action = (ROOT / ACTION_PATH / "action.yml").read_text(encoding="utf-8")

            self.assertEqual(action.count("actions/cache/restore@5a3ec84e"), 1)
            self.assertEqual(action.count("actions/cache/save@5a3ec84e"), 1)
            self.assertEqual(action.count("continue-on-error: true"), 2)
            self.assertEqual(action.count("./os/dev/install-cached-cosign.sh"), 1)
            self.assertEqual(action.count("path: os/external-assets/cosign-cache"), 2)
            self.assertEqual(action.count("key: cosign-v2.5.2-${{ inputs.cache-architecture }}-"), 2)
            self.assertIn("if: steps.restore.outputs.cache-hit != 'true'", action)
        with self.subTest(phase='workflows do not reimplement the cosign cache protocol'):
            workflows = ROOT / ".forgejo" / "workflows"
            for name, expected_uses in CONSUMERS.items():
                with self.subTest(workflow=name):
                    source = (workflows / name).read_text(encoding="utf-8")
                    self.assertEqual(source.count(f"uses: ./{ACTION_PATH}"), expected_uses)

            combined = "\n".join(
                path.read_text(encoding="utf-8") for path in workflows.glob("*.y*ml")
            )
            self.assertNotIn("path: os/external-assets/cosign-cache", combined)
        with self.subTest(phase='path filtered consumers track the shared action'):
            workflows = ROOT / ".forgejo" / "workflows"
            for name in (
                "build-ci-builder.yml",
                "build-kernel.yml",
                "build-buildroots.yml",
                "build-rpms.yml",
                "release-relay.yml",
            ):
                with self.subTest(workflow=name):
                    source = (workflows / name).read_text(encoding="utf-8")
                    self.assertIn(f'"{ACTION_PATH}/**"', source)


if __name__ == "__main__":
    unittest.main()
