from pathlib import Path
import json
import os
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/build-image.yml"


def matrix_script() -> str:
    workflow = WORKFLOW.read_text()
    marker = "      - name: Select validation or native publication lanes\n"
    block = workflow.split(marker, 1)[1].split("\n\n  signing-preflight:", 1)[0]
    return textwrap.dedent(block.split("        run: |\n", 1)[1])


def select_matrix(ref: str, event: str, requested: str) -> tuple[bool, list[dict[str, str]]]:
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / "output"
        environment = os.environ.copy()
        environment.update(
            {
                "GITHUB_REF": ref,
                "GITHUB_EVENT_NAME": event,
                "GITHUB_OUTPUT": str(output),
                "REQUESTED_PUBLISH": requested,
            }
        )
        subprocess.run(["bash", "-c", matrix_script()], env=environment, check=True)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        return values["publish"] == "true", json.loads(values["build-matrix"])["include"]


class OsReleaseMatrixTests(unittest.TestCase):
    def test_version_tags_keep_the_dedicated_release_fleet(self):
        publish, lanes = select_matrix("refs/tags/v1.2.3", "push", "false")
        self.assertTrue(publish)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "imager-linux-x86_64"},
                {"arch": "aarch64", "runner": "imager-linux-aarch64"},
            ],
        )

    def test_protected_manual_main_publish_uses_native_large_disk_fleet(self):
        publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "true")
        self.assertTrue(publish)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "runner-disk-100g"},
                {"arch": "aarch64", "runner": "runner-disk-100g-arm64"},
            ],
        )

    def test_manual_nonpublication_remains_a_single_native_smoke_lane(self):
        publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "false")
        self.assertFalse(publish)
        self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-disk-100g"}])


if __name__ == "__main__":
    unittest.main()
