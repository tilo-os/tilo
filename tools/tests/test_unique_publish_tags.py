"""Every published manifest keeps a tag no later push can take.

Forgejo's container registry treats a tag as a package version. Pushing a tag
again replaces it, and the manifest it named becomes unreachable by digest once
no tag is left, which breaks every digest pin on it. Run 29829 re-pushed
`rpms:native-<sha>-aarch64` and orphaned the child run 29823 had just signed.
So each publishing push carries a tag unique to its run and attempt, next to
any tag a rerun or a later run may move.
"""
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"


class ShellPushTests(unittest.TestCase):
    def read(self, name: str) -> str:
        return (WORKFLOWS / name).read_text()

    def test_os_candidates_and_integration_children_are_unique(self):
        text = self.read("build-image.yml")
        self.assertIn('$TILO_OS_REPOSITORY:candidate-${TILO_IMAGE_SOURCE}-${GITHUB_RUN_ID}-${{ github.run_attempt }}-${{ matrix.arch }}"', text)
        self.assertIn('tilo:index-candidate-${TILO_IMAGE_SOURCE}-${GITHUB_RUN_ID}-${{ github.run_attempt }}"', text)
        self.assertIn('$repository:index-candidate-${TILO_IMAGE_SOURCE}-${GITHUB_RUN_ID}-${{ github.run_attempt }}"', text)
        self.assertIn('"docker://registry.tilo.so/tilo/tilo-ci:$DEV_SERIES-$ARCH-$GITHUB_RUN_ID-${{ github.run_attempt }}"', text)

    def test_base_mirror_dev_disk_and_wine_pushes_are_unique(self):
        mirror = (ROOT / "os/dev/ensure-base-mirror.sh").read_text()
        self.assertIn('fedora-bootc:${BASE_SOURCE##*:}-recover-', mirror)
        self.assertIn('skopeo copy --all --preserve-digests', mirror)
        dev_disk = self.read("dev-disk.yml")
        self.assertIn("build-candidate-${SOURCE_REVISION:0:12}-$BASE_TRUST-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT-$ARCH", dev_disk)
        publish = (ROOT / "os/dev/publish-dev-disk.sh").read_text()
        self.assertIn('"docker://$REPOSITORY:$DEV_TAG-$ARCH-${image_digest:7:16}"', publish)
        self.assertIn('"docker://$REPOSITORY:$DEV_TAG-${index_digest:7:16}"', publish)
        wine = (ROOT / "os/dev/run-wine-ci.sh").read_text()
        self.assertEqual(wine.count("${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT:?}-${target}"), 2)

    def test_native_index_promotion_always_pushes_a_fresh_candidate(self):
        text = (ROOT / "os/dev/promote-native-index.py").read_text()
        self.assertIn("tilo-native-index-", text)
        self.assertIn("uuid", text)


if __name__ == "__main__":
    unittest.main()
