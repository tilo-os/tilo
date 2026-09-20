"""Contract tests for the GitHub-built Tilo builder image."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/build-builder-image.yml").read_text()
CHECK = (ROOT / ".github/workflows/check.yml").read_text()


class BuilderWorkflow(unittest.TestCase):
    def test_builds_natively_on_standard_github_images(self):
        self.assertIn("runner: ubuntu-24.04", WORKFLOW)
        self.assertIn("runner: ubuntu-24.04-arm", WORKFLOW)
        self.assertIn("platform: linux/amd64", WORKFLOW)
        self.assertIn("platform: linux/arm64", WORKFLOW)
        self.assertIn("runs-on: ${{ matrix.runner }}", WORKFLOW)

    def test_builds_from_source_and_publishes_only_to_ghcr(self):
        self.assertIn("file: os/Containerfile.builder", WORKFLOW)
        self.assertIn("uses: docker/build-push-action@", WORKFLOW)
        self.assertIn("registry: ghcr.io", WORKFLOW)
        self.assertIn("password: ${{ github.token }}", WORKFLOW)
        self.assertIn("ghcr.io/tilo-os/tilo-builder:sha-${{ github.sha }}", WORKFLOW)
        self.assertIn("ghcr.io/tilo-os/tilo-builder:latest", WORKFLOW)
        self.assertNotIn("code.podesta.ai", WORKFLOW)
        self.assertNotIn("REGISTRY_TOKEN", WORKFLOW)

    def test_uses_github_cache_and_github_attestations(self):
        self.assertIn("cache-from: type=gha,scope=tilo-builder-${{ matrix.arch }}", WORKFLOW)
        self.assertIn("cache-to: type=gha,scope=tilo-builder-${{ matrix.arch }},mode=max", WORKFLOW)
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
