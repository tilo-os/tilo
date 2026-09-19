from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


class RegistryServerImageWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = (ROOT / ".forgejo/workflows/registry-server-image.yml").read_text()

    def test_pull_requests_build_and_smoke_test_without_registry_write(self) -> None:
        self.assertIn('pull_request:\n    branches: ["main"]', self.source)
        self.assertIn("push: false", self.source)
        self.assertIn("container/test-image.sh", self.source)
        login = self.source.split("      - name: Log in for main publication\n", 1)[1].split(
            "      - name:", 1
        )[0]
        self.assertIn("github.ref == 'refs/heads/main'", login)
        self.assertIn("github.event_name == 'push'", login)

    def test_pr_code_cannot_read_checkout_registry_credentials(self) -> None:
        checkouts = self.source.split("      - uses: actions/checkout@", 1)[1]
        applicability_checkout, checkouts = checkouts.split("\n      - name:", 1)
        build_checkout = checkouts.split(
            "      - name: Checkout the exact image source\n", 1
        )[1].split("\n\n      - name:", 1)[0]
        for checkout in (applicability_checkout, build_checkout):
            self.assertIn("persist-credentials: false", checkout)
            self.assertNotIn("REGISTRY_TOKEN", checkout)
        self.assertIn("fetch-depth: 0", applicability_checkout)

        before_login = self.source.split(
            "      - name: Log in for main publication\n", 1
        )[0].split("\n  build-test-publish:\n", 1)[1]
        self.assertIn("container/test-image.sh", before_login)
        self.assertNotIn("REGISTRY_TOKEN", before_login)

    def test_main_pushes_the_already_tested_sha_tag_and_retains_digest(self) -> None:
        build, publish = self.source.split(
            "      - name: Publish the exact image that passed the smoke test\n", 1
        )
        self.assertIn(":sha-${{ env.SOURCE_REVISION }}", build)
        self.assertNotIn("docker/build-push-action", publish)
        self.assertIn('docker push "$image"', publish)
        self.assertIn("IMAGE_DIGEST: ${{ steps.publish.outputs.digest }}", publish)
        self.assertIn("actions/upload-artifact@", publish)
        self.assertNotRegex(self.source, r"(?m)^\s+tags:.*(?:latest|main)\s*$")

    def test_runtime_is_distroless_and_nonroot(self) -> None:
        containerfile = (
            ROOT / "crates/packaging/registry-server/container/Containerfile"
        ).read_text()
        self.assertRegex(containerfile, r"FROM gcr\.io/distroless/cc-debian12:nonroot@sha256:[0-9a-f]{64}")
        self.assertIn("USER 65532:65532", containerfile)

    def test_smoke_state_is_visible_to_the_runner_docker_daemon(self) -> None:
        smoke = (
            ROOT / "crates/packaging/registry-server/container/test-image.sh"
        ).read_text()
        self.assertIn('docker volume create "$volume"', smoke)
        self.assertIn(
            '--mount "type=volume,source=$volume,target=/var/lib/tilo-registry"',
            smoke,
        )
        self.assertNotIn("type=bind", smoke)
        self.assertIn('docker volume rm -f "$volume"', smoke)


if __name__ == "__main__":
    unittest.main()
