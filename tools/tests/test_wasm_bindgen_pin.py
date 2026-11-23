"""The wasm-bindgen CLI a stack's build binds with is the one its images carry.

A stack's build happens in a build container with no network, so it cannot install
wasm-bindgen-cli: every image it may start in (the CI builder, os/Containerfile.builder,
and the developer's distrobox, dist/dev/Containerfile.fedora44) carries the CLI, and
the build takes that one when its version is the crate's pin. This holds the pins to
each other, and to the asset list both images install from, so a version bump cannot
leave a stack without a binding tool.
"""
from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / 'crates/core/tilo-browser-runtime/Cargo.toml'
ASSETS = ROOT / 'os/external-assets/github-assets.tsv'
BUILDER = ROOT / 'os/Containerfile.builder'
DEVELOPER = ROOT / 'dist/dev/Containerfile.fedora44'


def pin():
    match = re.search(r'^wasm-bindgen = "=([0-9][^"]*)"$', CRATE.read_text(), re.M)
    return match.group(1) if match else None


class WasmBindgenPinTests(unittest.TestCase):
    def test_the_crate_pins_exactly_one_version(self):
        pins = set(re.findall(r'^wasm-bindgen = "=([0-9][^"]*)"$', CRATE.read_text(), re.M))
        self.assertEqual(len(pins), 1, pins)

    def test_the_asset_list_carries_the_pinned_cli_for_every_architecture(self):
        version = pin()
        names = {line.split('\t')[1] for line in ASSETS.read_text().splitlines()
                 if line and not line.startswith('#')}
        for arch in ('x86_64', 'aarch64'):
            self.assertIn(f'wasm-bindgen-{version}-{arch}-unknown-linux-musl.tar.gz', names)

    def test_the_ci_builder_installs_the_crates_pin(self):
        text = BUILDER.read_text()
        self.assertIn('crates/core/tilo-browser-runtime/Cargo.toml', text)
        self.assertIsNone(
            re.search(r'wasm-bindgen-[0-9]+\.[0-9]+\.[0-9]+-', text),
            'the builder names a version instead of reading the crate manifest')

    def test_the_developer_image_names_no_version_of_its_own(self):
        text = DEVELOPER.read_text()
        self.assertIsNone(
            re.search(r'wasm-bindgen-[0-9]+\.[0-9]+\.[0-9]+-', text),
            'the developer image names a version instead of taking the asset list’s')

    def test_the_builders_asset_list_names_the_pinned_cli(self):
        for arch in ('x86_64', 'aarch64'):
            listed = subprocess.check_output(
                ['bash', str(ROOT / 'tools/ci/publish/ci-builder.sh'), 'assets', arch],
                text=True).split()
            self.assertIn(f'wasm-bindgen-{pin()}-{arch}-unknown-linux-musl.tar.gz', listed)


if __name__ == '__main__':
    unittest.main()
