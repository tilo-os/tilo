from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


def job_block(workflow: str, name: str) -> str:
    """Select one job, ending at the next peer rather than a named successor."""
    jobs = workflow.split("\njobs:\n", 1)[1]
    headers = list(re.finditer(r"(?m)^  ([A-Za-z0-9_-]+):[ \t]*$", jobs))
    matches = [index for index, header in enumerate(headers) if header[1] == name]
    if len(matches) != 1:
        raise AssertionError(f"Expected exactly one job named {name!r}")
    index = matches[0]
    end = headers[index + 1].start() if index + 1 < len(headers) else len(jobs)
    return jobs[headers[index].start():end]


class ReleaseToolsImageWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (
            ROOT / ".forgejo/workflows/build-release-tools.yml"
        ).read_text()
        self.release_workflow = (
            ROOT / ".forgejo/workflows/release-apps.yml"
        ).read_text()
        self.containerfile = (ROOT / "os/Containerfile.release-tools").read_text()

    def test_release_tools_image_is_tested_natively_and_published_immutably(self) -> None:
        with self.subTest(phase='image installs only publication runtime tools'):
            self.doCleanups()
            self.setUp()
            self.assertRegex(
                self.containerfile,
                r"FROM registry\.fedoraproject\.org/fedora-minimal:44@sha256:[0-9a-f]{64}",
            )
            install = self.containerfile.split("RUN microdnf install", 1)[1].split("&&", 1)[0]
            for compiler in ("cargo", "rustc", "gcc", "clang", "make", "cmake"):
                self.assertNotRegex(install, rf"\b{compiler}\b")
            for package in ("gawk", "grep"):
                self.assertRegex(install, rf"\b{package}\b")
            self.assertIn('for compiler in cargo rustc cc gcc clang', self.containerfile)
        with self.subTest(phase='both native platforms are built and tested'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='pr code runs before registry credentials exist'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='only immutable candidate or source tags are published'):
            self.doCleanups()
            self.setUp()
            self.assertIn('tag="sha-$SOURCE_REVISION"', self.workflow)
            self.assertIn('tag="candidate-$SOURCE_REVISION"', self.workflow)
            self.assertNotRegex(self.workflow, r"(?m)^\s*(?:tags:|tag=).*(?:latest|:main)\s*$")
            self.assertIn("release-tools-image-receipt/release-tools-image.json", self.workflow)
        with self.subTest(phase='tested index is verified anonymously'):
            self.doCleanups()
            self.setUp()
            publish = job_block(self.workflow, "publish-index")
            self.assertIn(
                "IMAGE_REPOSITORY: registry.tilo.so/tilo/tilo-release-tools",
                publish,
            )
            self.assertIn('"$IMAGE_REPOSITORY@$digest" --raw', publish)
            self.assertIn('DOCKER_CONFIG="$anonymous_config"', publish)
            self.assertIn('[[ $actual_children == "$expected_children" ]]', publish)

            verify = job_block(self.workflow, "verify-public")
            self.assertNotIn("\n      credentials:", verify)
            self.assertNotIn("REGISTRY_TOKEN", verify)
            self.assertIn("docker pull --platform '${{ matrix.platform }}'", verify)
            self.assertIn('bash os/tests/release_tools_image.sh "$image"', verify)
        with self.subTest(phase='isolated publisher uses the verified public compiler free image'):
            self.doCleanups()
            self.setUp()
            # release-apps.yml is the single owner of the reviewed digest.
            for name in ("publish", "publish-engine-runtime"):
                with self.subTest(job=name):
                    publish = job_block(self.release_workflow, name)
                    self.assertRegex(
                        publish,
                        r"image: registry\.tilo\.so/tilo/tilo-release-tools@sha256:[0-9a-f]{64}\n",
                    )
                    self.assertNotIn("image: ${{ needs.plan.outputs.builder_image }}", publish)
                    self.assertNotRegex(publish, r"(?m)^\s+credentials:")
                    container = re.search(
                        r"(?m)^    container:\n((?:      [^\n]*\n|[ \t]*\n)*)",
                        publish,
                    )
                    self.assertIsNotNone(container, f"{name} has no container mapping")
                    self.assertNotIn("credentials:", container[1])
                    self.assertNotIn("REGISTRY_TOKEN", container[1])

            build = job_block(self.release_workflow, "build")
            self.assertIn("image: ${{ needs.plan.outputs.builder_image }}", build)

    def test_job_boundaries_do_not_include_later_builders_or_hide_own_secrets(self) -> None:
        workflow = """name: release
jobs:
  publish:
    container:
      image: publication-image
    steps:
      - run: publish-with-REGISTRY_TOKEN
  build-engine-runtime:
    container:
      image: builder-image
      credentials: builder-secret
  publish-engine-runtime:
    container:
      image: publication-image
    steps:
      - run: publish-engine-with-REGISTRY_TOKEN
  another-builder:
    container:
      image: builder-image
      credentials: builder-secret
"""
        for name, command in (
            ("publish", "publish-with-REGISTRY_TOKEN"),
            ("publish-engine-runtime", "publish-engine-with-REGISTRY_TOKEN"),
        ):
            with self.subTest(job=name):
                selected = job_block(workflow, name)
                self.assertIn("image: publication-image", selected)
                self.assertIn(command, selected)
                self.assertNotIn("builder-image", selected)
                self.assertNotIn("builder-secret", selected)
                # A secret inside the publisher must remain observable to guards.
                contaminated = workflow.replace(
                    f"  {name}:\n", f"  {name}:\n    credentials: publisher-secret\n"
                )
                self.assertIn("credentials: publisher-secret", job_block(contaminated, name))
        with self.assertRaises(AssertionError):
            job_block(workflow, "missing")
        with self.assertRaises(AssertionError):
            job_block(workflow + "  publish:\n    steps: []\n", "publish")



if __name__ == "__main__":
    unittest.main()
