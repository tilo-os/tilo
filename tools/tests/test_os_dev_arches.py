"""Build / OS dev picks x86_64 for pull requests and adds aarch64 when needed."""
import importlib.util
import fnmatch
import re
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("os_dev_arches", ROOT / "tools/ci/os_dev_arches.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OsDevArchesTests(unittest.TestCase):
    def test_workflow_excludes_only_windows_host_transport(self):
        workflow = (ROOT / ".forgejo/workflows/os-dev.yml").read_text()
        event = re.search(r"(?ms)^  pull_request:\n(.*?)(?=^[^ ].*|\Z)", workflow)
        self.assertIsNotNone(event, "Cannot observe the pull_request event")
        patterns = re.findall(r'^      - "([^\"]+)"$', event[1], re.MULTILINE)
        self.assertIn("os/**", patterns)

        def selected(path):
            included = False
            for pattern in patterns:
                negative = pattern.startswith("!")
                if fnmatch.fnmatchcase(path, pattern[1:] if negative else pattern):
                    included = not negative
            return included

        host = ["os/dev/vm-windows.ps1", "os/tests/vm-windows-tool-resolver-test.ps1"]
        for path in host:
            self.assertFalse(selected(path), path)
        payload = ["os/Containerfile", "os/Containerfile.dev", "os/Containerfile.installer",
                   "os/usr/libexec/tilo-installer-code-refresh.py",
                   "os/dev/windows_provider.py", "os/dev/uki.sh",
                   "os/dev/prepare-hardware-profile.py", "os/kernel/config/aarch64",
                   "os/hardware/profiles/lenovo-83ed.toml",
                   "os/dev/vm-windows.ps1.example"]
        for path in payload:
            with self.subTest(path=path):
                self.assertTrue(selected(path), path)
                self.assertTrue(any(selected(item) for item in host + [path]))
        self.assertFalse(any(selected(path) for path in host))
        # Full dev images are explicit; ordinary pushes never request one.
        self.assertNotRegex(workflow, r"(?m)^  push:$")
        self.assertEqual(MODULE.arches("push", paths=host), ["x86_64"])

    def test_architecture_choice(self):
        both = ["x86_64", "aarch64"]
        self.assertEqual(MODULE.arches("push"), ["x86_64"])
        self.assertEqual(MODULE.arches("pull_request", paths=["os/etc/chrony.conf"]), ["x86_64"])
        self.assertEqual(MODULE.arches("pull_request", labels=["os-arm64"]), both)
        for path in ("os/kernel/config/aarch64", "os/hardware/profiles/lenovo-83ed.toml",
                     "os/kernel-buildroot-snapshot.env", "os/dev/uki.sh",
                     "os/external-assets/fex-source.env"):
            with self.subTest(path=path):
                self.assertEqual(MODULE.arches("pull_request", paths=[path]), both)
        self.assertEqual(MODULE.arch_specific(["os/Containerfile", "os/kernel/x"]), ["os/kernel/x"])

    def test_image_requests_are_explicit_before_native_admission(self):
        workflow = (ROOT / ".forgejo/workflows/os-dev.yml").read_text()
        self.assertIn("    types: [opened, synchronize, reopened, labeled]", workflow)
        self.assertIn("  workflow_dispatch:", workflow)
        arches = workflow.split("  arches:\n", 1)[1].split("\n  image:", 1)[0]
        self.assertIn("    if: github.event_name == 'workflow_dispatch' || "
                      "contains(github.event.pull_request.labels.*.name, 'os-image') || "
                      "contains(github.event.pull_request.labels.*.name, 'os-arm64')", arches)
        image = workflow.split("\n  image:\n", 1)[1]
        self.assertIn("    needs: arches\n", image)
        self.assertNotIn("always()", image)
        self.assertIn("    uses: ./.forgejo/workflows/build-image.yml", image)
        release = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        self.assertRegex(release, r"(?m)^  push:$")
        self.assertIn('tags: ["v*"]', release)

    def test_explicit_main_builds_newest_commit_one_at_a_time_per_arch(self):
        for name, arch in (("os-dev-arm.yml", "aarch64"), ("os-dev-x86.yml", "x86_64")):
            with self.subTest(arch=arch):
                workflow = (ROOT / ".forgejo/workflows" / name).read_text()
                self.assertRegex(workflow, r"(?m)^  workflow_dispatch:$")
                self.assertNotRegex(workflow, r"(?m)^  push:$")
                self.assertIn(f"  group: os-dev-{arch}-main\n  cancel-in-progress: false\n", workflow)
                self.assertIn('if [[ $head == "$GITHUB_SHA" ]]; then', workflow)
                self.assertIn("    if: needs.newest.outputs.build == 'true'\n"
                              "    uses: ./.forgejo/workflows/build-image.yml\n"
                              "    with:\n"
                              f"      arches: '[\"{arch}\"]'\n", workflow)


if __name__ == "__main__":
    unittest.main()
