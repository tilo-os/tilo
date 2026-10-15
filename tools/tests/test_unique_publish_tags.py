"""Every published manifest keeps a tag no later push can take.

Forgejo's container registry treats a tag as a package version. Pushing a tag
again replaces it, and the manifest it named becomes unreachable by digest once
no tag is left, which breaks every digest pin on it. Run 29829 re-pushed
`rpms:native-<sha>-aarch64` and orphaned the child run 29823 had just signed.
So each publishing push carries a tag unique to its run and attempt, next to
any tag a rerun or a later run may move.
"""
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
UNIQUE = ("${{ github.run_id }}", "${{ github.run_attempt }}")


def steps(path: Path) -> list[str]:
    return re.split(r"\n(?=      - (?:name|uses): )", path.read_text())


def pushing_build_steps(path: Path) -> list[str]:
    found = []
    for step in steps(path):
        if "docker/build-push-action@" not in step:
            continue
        push = re.search(r"\n          push: (.+)", step)
        if push and push.group(1).strip() not in ("false",):
            found.append(step)
    return found


def tags(step: str) -> str:
    block = re.search(r"\n          tags: (\|\n(?:            .+\n)+|.+)", step)
    return block.group(1) if block else ""


class BuildPushTests(unittest.TestCase):
    def test_every_pushing_build_carries_a_run_and_attempt_unique_tag(self):
        checked = 0
        for path in sorted(WORKFLOWS.glob("*.y*ml")):
            for step in pushing_build_steps(path):
                name = re.match(r"      - (?:name: )?(.+)", step).group(1)
                with self.subTest(workflow=path.name, step=name):
                    value = tags(step)
                    unique = [line for line in value.splitlines()
                              if all(part in line for part in UNIQUE)]
                    self.assertTrue(unique, f"no run- and attempt-unique tag in: {value}")
                    checked += 1
        self.assertGreaterEqual(checked, 5)


class ShellPushTests(unittest.TestCase):
    def read(self, name: str) -> str:
        return (WORKFLOWS / name).read_text()

    def test_kernel_retry_pushes_the_unique_tag(self):
        text = self.read("build-kernel.yml")
        self.assertIn("UNIQUE_TAG: registry.tilo.so/tilo/tilo-kernel:native-${{ github.sha }}-"
                      "${{ github.run_id }}-${{ github.run_attempt }}-${{ matrix.arch }}", text)
        self.assertIn('done <<< "$UNIQUE_TAG', text)

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
