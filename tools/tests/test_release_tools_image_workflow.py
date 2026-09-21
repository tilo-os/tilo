from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseToolsImageWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (
            ROOT / ".forgejo/workflows/build-release-tools-image.yml"
        ).read_text()
        self.release_workflow = (
            ROOT / ".forgejo/workflows/release-apps.yml"
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
        for package in ("gawk", "grep"):
            self.assertRegex(install, rf"\b{package}\b")
        self.assertIn('for compiler in cargo rustc cc gcc clang', self.containerfile)

    def test_both_native_platforms_are_built_and_tested(self) -> None:
        self.assertIn("platform: linux/amd64", self.workflow)
        self.assertIn("platform: linux/arm64", self.workflow)
        self.assertIn("runner: runner-arm64", self.workflow)
        self.assertIn("os/tests/release_tools_image.sh", self.workflow)
        self.assertIn("docker buildx imagetools create", self.workflow)
        self.assertIn("verify-public:", self.workflow)
        self.assertIn("Pull and test the public image without credentials", self.workflow)
        smoke = (ROOT / "os/tests/release_tools_image.sh").read_text()
        self.assertIn("max_image_bytes=536870912", smoke)
        self.assertIn("docker image inspect --format '{{.Size}}'", smoke)

    def test_pr_code_runs_before_registry_credentials_exist(self) -> None:
        checkout, after_checkout = self.workflow.split(
            "      - uses: actions/checkout@", 1
        )[1].split("\n\n      - name:", 1)
        self.assertIn("persist-credentials: false", checkout)
        self.assertNotIn("REGISTRY_TOKEN", checkout)

        before_login = after_checkout.split(
            "\n      - name: Log in only after the candidate passed\n", 1
        )[0]
        self.assertIn("os/tests/release_tools_image.sh", before_login)
        self.assertNotIn("REGISTRY_TOKEN", before_login)

    def test_only_immutable_candidate_or_source_tags_are_published(self) -> None:
        self.assertIn('tag="sha-$SOURCE_REVISION"', self.workflow)
        self.assertIn('tag="candidate-$SOURCE_REVISION"', self.workflow)
        self.assertNotRegex(self.workflow, r"(?m)^\s*(?:tags:|tag=).*(?:latest|:main)\s*$")
        self.assertIn("release-tools-image-receipt/release-tools-image.json", self.workflow)

    def test_tested_index_is_replicated_and_verified_anonymously(self) -> None:
        publish = self.workflow.split("\n  publish-index:\n", 1)[1].split(
            "\n  verify-public:\n", 1
        )[0]
        self.assertIn(
            "PUBLIC_IMAGE_REPOSITORY: code.sandbox.example/public/tilo-release-tools",
            publish,
        )
        self.assertIn('"$IMAGE_REPOSITORY@$digest"', publish)
        self.assertIn('[[ $public_digest == "$digest" ]]', publish)
        self.assertIn('DOCKER_CONFIG="$anonymous_config"', publish)
        self.assertIn('[[ $actual_children == "$expected_children" ]]', publish)

        verify = self.workflow.split("\n  verify-public:\n", 1)[1]
        self.assertNotIn("\n      credentials:", verify)
        self.assertNotIn("REGISTRY_TOKEN", verify)
        self.assertIn("docker pull --platform '${{ matrix.platform }}'", verify)
        self.assertIn('bash os/tests/release_tools_image.sh "$image"', verify)

    def test_isolated_publisher_uses_the_verified_public_compiler_free_image(self) -> None:
        expected = (
            "code.sandbox.example/public/tilo-release-tools@"
            "sha256:a59c9b664bc4aa09da7a747b77aa72c965608d6da9fa5659a3e70e01810b764b"
        )
        publish = self.release_workflow.split("\n  publish:\n", 1)[1]
        self.assertIn(f"image: {expected}", publish)
        self.assertNotIn("image: ${{ needs.plan.outputs.builder_image }}", publish)
        container = publish.split("    container:\n", 1)[1].split(
            "    timeout-minutes:", 1
        )[0]
        self.assertNotIn("credentials:", container)
        self.assertNotIn("REGISTRY_TOKEN", container)

        build = self.release_workflow.split("\n  build:\n", 1)[1].split(
            "\n  qualify-collection:\n", 1
        )[0]
        self.assertIn("image: ${{ needs.plan.outputs.builder_image }}", build)


if __name__ == "__main__":
    unittest.main()
