from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


class RegistryServerImageWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = (ROOT / ".forgejo/workflows/release-registry-server.yml").read_text()

    def test_registry_server_image_is_smoke_tested_before_credentials_and_pushed_by_tested_sha(self) -> None:
        with self.subTest(phase='pull requests build and smoke test without registry write'):
            self.doCleanups()
            self.setUp()
            self.assertIn('pull_request:\n    branches: ["main"]', self.source)
            self.assertIn("push: false", self.source)
            self.assertIn("container/test-image.sh", self.source)
            login = self.source.split("      - name: Log in for main publication\n", 1)[1].split(
                "      - name:", 1
            )[0]
            self.assertIn("github.ref == 'refs/heads/main'", login)
            self.assertIn("github.event_name == 'push'", login)
        with self.subTest(phase='pr code cannot read checkout registry credentials'):
            self.doCleanups()
            self.setUp()
            build_checkout = self.source.split(
                "      - name: Checkout the exact image source\n", 1
            )[1].split("\n\n      - name:", 1)[0]
            self.assertIn("persist-credentials: false", build_checkout)
            self.assertNotIn("REGISTRY_TOKEN", build_checkout)

            before_login = self.source.split(
                "      - name: Log in for main publication\n", 1
            )[0].split("\n  build-test-publish:\n", 1)[1]
            self.assertIn("container/test-image.sh", before_login)
            self.assertNotIn("REGISTRY_TOKEN", before_login)
        with self.subTest(phase='main pushes the already tested sha tag and retains digest'):
            self.doCleanups()
            self.setUp()
            build, publish = self.source.split(
                "      - name: Publish the exact image that passed the smoke test\n", 1
            )
            self.assertIn(":sha-${{ env.SOURCE_REVISION }}", build)
            self.assertNotIn("docker/build-push-action", publish)
            self.assertIn('docker push "$image"', publish)
            self.assertIn("IMAGE_DIGEST: ${{ steps.publish.outputs.digest }}", publish)
            self.assertIn("actions/upload-artifact@", publish)
            self.assertNotRegex(self.source, r"(?m)^\s+tags:.*(?:latest|main)\s*$")
        with self.subTest(phase='runtime is distroless and nonroot'):
            self.doCleanups()
            self.setUp()
            containerfile = (
                ROOT / "crates/packaging/registry-server/container/Containerfile"
            ).read_text()
            self.assertRegex(containerfile, r"FROM gcr\.io/distroless/cc-debian12:nonroot@sha256:[0-9a-f]{64}")
            self.assertIn("USER 65532:65532", containerfile)
        with self.subTest(phase='smoke state is visible to the runner docker daemon'):
            self.doCleanups()
            self.setUp()
            smoke = (
                ROOT / "crates/packaging/registry-server/container/test-image.sh"
            ).read_text()
            self.assertIn('docker volume create "$volume"', smoke)
            self.assertRegex(smoke, r"docker\.io/library/busybox@sha256:[0-9a-f]{64}")
            self.assertIn("--cap-add CHOWN", smoke)
            self.assertIn("chmod 0750 /state && chown 65532:65532 /state", smoke)
            self.assertIn(
                '--mount "type=volume,source=$volume,target=/var/lib/tilo-registry,volume-nocopy"',
                smoke,
            )
            self.assertIn("--data-dir /var/lib/tilo-registry", smoke)
            self.assertIn('--network "container:$container"', smoke)
            self.assertNotIn("--publish 127.0.0.1", smoke)
            self.assertNotIn("type=bind", smoke)
            self.assertIn('docker volume rm -f "$volume"', smoke)
        with self.subTest(phase='smoke requires the delegated offline root cli'):
            self.doCleanups()
            self.setUp()
            smoke = (
                ROOT / "crates/packaging/registry-server/container/test-image.sh"
            ).read_text()
            for command in (
                "authorize-channel",
                "collect-first-party",
                "publish-staged-first-party",
                "sign-delegated-snapshot",
            ):
                self.assertIn(command, smoke)
            self.assertIn("removed unsafe promote-first-party command is still present", smoke)
            self.assertIn('"$image" promote-first-party --help', smoke)


if __name__ == "__main__":
    unittest.main()
