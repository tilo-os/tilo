from pathlib import Path
import re
import unittest
import sys


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import t_matrix  # noqa: E402


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
        t = t_matrix.load(ROOT)
        self.checks = {check["name"]: check for check in t["check"]}
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
        with self.subTest(phase='both native platforms are built and tested before publication'):
            self.doCleanups()
            self.setUp()
            for arch in ("x86_64", "aarch64"):
                build = self.checks[f"image-release-tools-{arch}"]
                self.assertEqual(build["requires"], [f"arch={arch}"])
                self.assertIn("os/tests/release_tools_image.sh", build["run"])
                self.assertNotIn("id_token", build)
                self.assertNotIn("secrets", build)
            index = self.checks["image-release-tools"]
            self.assertEqual(
                index["needs"], ["image-release-tools-x86_64", "image-release-tools-aarch64"]
            )
            self.assertIn("oci-index.py", index["run"])
            smoke = (ROOT / "os/tests/release_tools_image.sh").read_text()
            self.assertIn("max_image_bytes=536870912", smoke)
        with self.subTest(phase='dispatched: one immutable index, proven anonymously pullable'):
            self.doCleanups()
            self.setUp()
            publish = self.checks["publish-release-tools"]
            self.assertEqual(publish["needs"], ["image-release-tools", "signing-tools"])
            self.assertEqual(publish["on"], ["dispatch"])
            self.assertEqual(publish["id_token"], ["registry.tilo.so"])
            self.assertIn("registry.tilo.so/tilo/tilo-release-tools", publish["run"])
            self.assertIn("'sha-{commit}'", publish["run"])
            self.assertNotRegex(publish["run"], r"latest|:main")
            self.assertIn("oci-anonymous.sh", publish["run"])
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
