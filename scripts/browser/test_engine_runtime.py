"""The browser engine runtime is named the same everywhere it is built and found."""
import json
from pathlib import Path
import re
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

    def test_the_browser_install_link_requests_the_exact_declared_runtime(self):
        source = (ROOT / "apps/browser/src/browser_widget.rs").read_text()
        match = re.search(r'const ENGINE_INSTALL_URI: &str = "tilo-store:runtime/([^"\s]+)";', source)
        self.assertIsNotNone(match)
        self.assertEqual(match.group(1), rust_const("RUNTIME_REF"))

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
