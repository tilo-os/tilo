"""The browser engine runtime is named the same everywhere it is built and found."""
import json
from pathlib import Path
import re
import os
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def rust_const(name):
    source = (ROOT / "crates/sdk/ui-core/src/webview/chromium/runtime.rs").read_text()
    match = re.search(rf'pub const {name}: &str = "([^"]+)";', source)
    assert match, name
    return match.group(1)


class EngineRuntimeNameTests(unittest.TestCase):
    def test_the_image_builds_the_version_the_browser_looks_for(self):
        containerfile = (ROOT / "os/Containerfile").read_text()
        built = re.findall(r"^ARG CHROMIUM_RUNTIME_VERSION=(\S+)$", containerfile, re.MULTILINE)
        self.assertEqual(built, [rust_const("RUNTIME_VERSION")])

    def test_release_and_default_images_cannot_omit_the_offline_engine(self):
        containerfile = (ROOT / "os/Containerfile").read_text()
        self.assertEqual(re.findall(r"^ARG TILO_WITH_BROWSER=(\S+)$", containerfile, re.MULTILINE), ["1"])
        workflow = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        step = workflow.split("- name: Select the OS validation profile\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n      #", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines() if line.strip())
        for ref, event, requested, expected in (
            ("refs/tags/v1.0.0", "push", "0", "1"),
            ("refs/tags/v1.0.0", "workflow_dispatch", "0", "1"),
            ("refs/heads/main", "push", "0", "1"),
            ("refs/heads/main", "workflow_dispatch", "", "1"),
            ("refs/heads/test", "workflow_dispatch", "0", "0"),
        ):
            with self.subTest(ref=ref, event=event, requested=requested), tempfile.TemporaryDirectory() as tmp:
                output = Path(tmp) / "env"
                result = subprocess.run(["bash", "-euc", script], env={
                    **os.environ, "GITHUB_REF": ref, "GITHUB_EVENT_NAME": event,
                    "REQUESTED_BROWSER": requested, "GITHUB_ENV": str(output),
                }, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output.read_text(), f"TILO_WITH_BROWSER={expected}\n")

    def test_the_final_image_verifies_the_baseline_engine_not_just_when_present(self):
        # The rootfs stage: the derived layer's checks of the finished root filesystem.
        rootfs = re.split(r"^FROM \S+ AS rootfs\n", (ROOT / "os/Containerfile").read_text(), maxsplit=1,
                          flags=re.M)[1]
        rootfs = re.split(r"^FROM ", rootfs, maxsplit=1, flags=re.M)[0]
        self.assertIn("ARG TILO_WITH_BROWSER\n", rootfs)
        guard = rootfs.split('if [ "${TILO_WITH_BROWSER}" != 0 ]; then', 1)[1].split("fi; }", 1)[0]
        self.assertIn('test -x "$engine/tilo-chromium-host"', guard)
        self.assertIn('test -s "$engine/libcef.so"', guard)
        self.assertIn("/usr/bin/tilo-runtime-bundle verify", guard)
        self.assertIn('--require-publisher "$(cat /usr/share/tilo/browser-runtime-publisher.hex)"', guard)

    def test_the_browser_install_link_requests_the_exact_declared_runtime(self):
        source = (ROOT / "apps/browser/src/browser_widget.rs").read_text()
        match = re.search(r'const ENGINE_INSTALL_URI: &str =\s*"tilo-store:runtime/([^"\s]+)";', source)
        self.assertIsNotNone(match)
        owner, runtime = match.group(1).split("/", 1)
        self.assertEqual(owner, "so.tilo.browser")
        self.assertEqual(runtime, rust_const("RUNTIME_REF"))

    def test_the_browser_manifest_asks_for_that_runtime(self):
        manifest = (ROOT / "apps/browser/tilo-app.toml").read_text()
        self.assertIn(f'optional-runtimes = ["{rust_const("RUNTIME_REF")}"]', manifest)

    def test_the_image_fetches_the_pinned_distribution_for_each_architecture(self):
        lock = json.loads((ROOT / "scripts/browser/cef-lock.json").read_text())
        assets = {}
        for line in (ROOT / "os/external-assets/github-assets.tsv").read_text().splitlines():
            if line and not line.startswith("#"):
                sha256, name, url = line.split("\t")
                assets[name] = (sha256, url)
        for platform in ("linux64", "linuxarm64"):
            with self.subTest(platform=platform):
                entry = lock["platforms"][platform]
                name = f"cef-{lock['chromium_version']}-{platform}-minimal.tar.bz2"
                self.assertIn(name, assets)
                sha256, url = assets[name]
                self.assertEqual(sha256, entry["sha256"])
                self.assertTrue(url.endswith(entry["name"].replace("+", "%2B")), url)


if __name__ == "__main__":
    unittest.main()
