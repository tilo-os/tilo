from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseToolsImageWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (
            ROOT / ".forgejo/workflows/build-release-tools-image.yml"
        ).read_text()
        self.containerfile = (ROOT / "os/Containerfile.release-tools").read_text()

    def test_image_installs_only_publication_runtime_tools(self) -> None:
        self.assertRegex(
            self.containerfile,
            r"FROM registry\.fedoraproject\.org/fedora-minimal:42@sha256:[0-9a-f]{64}",
        )
        install = self.containerfile.split("RUN microdnf install", 1)[1].split("&&", 1)[0]
        for compiler in ("cargo", "rustc", "gcc", "clang", "make", "cmake"):
            self.assertNotRegex(install, rf"\b{compiler}\b")
        self.assertIn('for compiler in cargo rustc cc gcc clang', self.containerfile)

    def test_both_native_platforms_are_built_and_tested(self) -> None:
        self.assertIn("platform: linux/amd64", self.workflow)
        self.assertIn("platform: linux/arm64", self.workflow)
        self.assertIn("runner: runner-arm64", self.workflow)
        self.assertIn("os/tests/release_tools_image.sh", self.workflow)
        self.assertIn("docker buildx imagetools create", self.workflow)

    def test_only_immutable_candidate_or_source_tags_are_published(self) -> None:
        self.assertIn('tag="sha-$SOURCE_REVISION"', self.workflow)
        self.assertIn('tag="candidate-$SOURCE_REVISION"', self.workflow)
        self.assertNotRegex(self.workflow, r"(?m)^\s*(?:tags:|tag=).*(?:latest|:main)\s*$")
        self.assertIn("release-tools-image-receipt/release-tools-image.json", self.workflow)


if __name__ == "__main__":
    unittest.main()
