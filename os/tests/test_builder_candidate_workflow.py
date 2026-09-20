"""Contract tests for the GitHub-built Tilo builder image."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/build-builder-image.yml").read_text()
CHECK = (ROOT / ".github/workflows/check.yml").read_text()


class BuilderWorkflow(unittest.TestCase):
    def test_builds_natively_on_blacksmith_runners(self):
        self.assertIn("arch: x86_64\n            platform: linux/amd64\n            runner: blacksmith-4vcpu-ubuntu-2404\n", WORKFLOW)
        self.assertIn("arch: aarch64\n            platform: linux/arm64\n            runner: blacksmith-4vcpu-ubuntu-2404-arm\n", WORKFLOW)
        self.assertIn("runs-on: ${{ matrix.runner }}", WORKFLOW)

    def test_builds_from_source_and_publishes_only_to_ghcr(self):
        self.assertIn("file: os/Containerfile.builder", WORKFLOW)
        self.assertRegex(WORKFLOW, r"uses: useblacksmith/build-push-action@[0-9a-f]{40}\b")
        self.assertIn("registry: ghcr.io", WORKFLOW)
        self.assertIn("password: ${{ github.token }}", WORKFLOW)
        self.assertIn("ghcr.io/tilo-os/tilo-builder:sha-${{ github.sha }}", WORKFLOW)
        self.assertIn("ghcr.io/tilo-os/tilo-builder:latest", WORKFLOW)
        self.assertNotIn("code.podesta.ai", WORKFLOW)
        self.assertNotIn("REGISTRY_TOKEN", WORKFLOW)

    def test_uses_blacksmith_layer_cache_and_github_attestations(self):
        self.assertRegex(WORKFLOW, r"uses: useblacksmith/setup-docker-builder@[0-9a-f]{40}\b")
        self.assertLess(WORKFLOW.index("uses: useblacksmith/setup-docker-builder@"),
                        WORKFLOW.index("uses: useblacksmith/build-push-action@"))
        self.assertNotIn("cache-from:", WORKFLOW)
        self.assertNotIn("cache-to:", WORKFLOW)
        self.assertIn("uses: actions/attest-build-provenance@", WORKFLOW)
        self.assertIn("attestations: write", WORKFLOW)
        self.assertIn("id-token: write", WORKFLOW)

    def test_manifest_requires_both_native_digests(self):
        self.assertIn("needs: build", WORKFLOW)
        self.assertIn("for arch in x86_64 aarch64", WORKFLOW)
        self.assertIn('[[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]', WORKFLOW)
        self.assertIn('docker buildx imagetools create "${tags[@]}" "${refs[@]}"', WORKFLOW)

    def test_only_the_useful_builder_image_remains_custom(self):
        self.assertFalse((ROOT / "os/Containerfile.ci-fast").exists())
        self.assertFalse((ROOT / "os/Containerfile.ci-control").exists())
        self.assertNotIn("ci-fast", CHECK)
        self.assertNotIn("ci-control", CHECK)
        self.assertIn("image: ${{ needs.builder.outputs.image }}", CHECK)


if __name__ == "__main__":
    unittest.main()
