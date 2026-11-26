"""Exercise the UI fixture build script's real Cargo invalidation boundary."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCAN = ROOT / "crates/ui/tilo-component-scan"
ICON = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" '
    'stroke="currentColor" stroke-width="1.5" stroke-linecap="round" '
    'stroke-linejoin="round"><line x1="4" y1="4" x2="20" y2="20"/></svg>'
)


class BuildInputs:
    """A toolkit crate's build script, copied into a package of its own with the inputs it
    decodes: `SOURCE` is the crate, `FIXTURES` its encoded inputs by the file each becomes."""

    SOURCE: Path
    FIXTURES: dict[str, str]
    # Whether the build script reads the component registry's sources through the scan.
    SCANS = False
    # Whether the build script generates the icon set from `assets/icons`.
    ICONS = False

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.package = Path(self.temporary.name) / self.SOURCE.relative_to(ROOT)
        (self.package / "src").mkdir(parents=True)
        manifest = (
            '[package]\nname = "ui-build-inputs"\nversion = "0.0.0"\n'
            'edition = "2024"\nautotests = false\n[workspace]\n'
        )
        if self.SCANS:
            manifest += '[build-dependencies]\ntilo-component-scan = { path = "scan" }\n'
        (self.package / "Cargo.toml").write_text(manifest)
        shutil.copyfile(self.SOURCE / "build.rs", self.package / "build.rs")
        if self.ICONS:
            # The icon set the build script reads: one stroked source on the shared root, which
            # is what every icon in the crate is drawn on.
            icons = self.package / "assets/icons"
            icons.mkdir(parents=True)
            (icons / "fixture.svg").write_text(ICON)
        if self.SCANS:
            # The registry's reader, without the workspace's shared dependency set: the fixture
            # builds offline on its own.
            scan = self.package / "scan"
            (scan / "src").mkdir(parents=True)
            (scan / "Cargo.toml").write_text(
                '[package]\nname = "tilo-component-scan"\nversion = "0.1.0"\n'
                'edition = "2024"\n'
            )
            shutil.copyfile(SCAN / "src/lib.rs", scan / "src/lib.rs")
        self.original = {}
        for source, filename in self.FIXTURES.items():
            destination = self.package / source
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(self.SOURCE / source, destination)
            self.original[filename] = bytes.fromhex(destination.read_text())
        (self.package / "src/lib.rs").write_text("\n".join(
            f'pub static FIXTURE_{index}: &[u8] = '
            f'include_bytes!(concat!(env!("OUT_DIR"), "/{filename}"));'
            for index, filename in enumerate(self.FIXTURES.values())
        ))

    def build(self):
        # Keep this dependency-free fixture independent of caller profile/cache
        # overrides. It uses its own Cargo target directory, never a shared one.
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith(("CARGO_PROFILE_", "CARGO_TARGET_"))
                       and key not in {"CARGO_TARGET_DIR", "CARGO_BUILD_TARGET",
                                       "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}}
        result = subprocess.run(
            ["cargo", "build", "--offline", "--lib", "--message-format=json"],
            cwd=self.package, env=environment, text=True, capture_output=True,
            check=True,
        )
        messages = [json.loads(line) for line in result.stdout.splitlines()]
        artifact = next(message for message in messages
                        if message["reason"] == "compiler-artifact"
                        and message["target"]["kind"] == ["lib"]
                        and message["target"]["name"] == "ui_build_inputs")
        output = next(message for message in messages
                      if message["reason"] == "build-script-executed")
        return artifact["fresh"], Path(output["out_dir"])

    def test_documentation_and_unused_test_edits_keep_the_library_fresh(self):
        fresh, output = self.build()
        self.assertFalse(fresh)
        for filename, expected in self.original.items():
            self.assertEqual((output / filename).read_bytes(), expected)
        for source in ("HIG.md", "tests/unused.rs"):
            with self.subTest(source=source):
                (self.package / source).write_text("// Unrelated edit\n")
                fresh, _ = self.build()
                self.assertTrue(fresh, f"{source} must not rebuild the UI library")

    def test_each_encoded_input_regenerates_the_consumed_fixture(self):
        self.build()
        for source, filename in self.FIXTURES.items():
            with self.subTest(source=source):
                path = self.package / source
                path.write_text(path.read_text() + "\n00\n")
                fresh, output = self.build()
                self.assertFalse(fresh)
                self.original[filename] += b"\0"
                for name, expected in self.original.items():
                    self.assertEqual((output / name).read_bytes(), expected)


class UiBaseBuildInputs(BuildInputs, unittest.TestCase):
    SOURCE = ROOT / "crates/ui/ui-base"
    FIXTURES = {
        "tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex":
            "TiloSansFeaturesFixture-Regular.ttf",
    }
    ICONS = True


class UiBuildInputs(BuildInputs, unittest.TestCase):
    SOURCE = ROOT / "crates/ui/ui-core"
    FIXTURES = {
        "tests/fixtures/test_image.png.hex": "test_image.png",
        "../../engines/paint-stream/tests/fixtures/layers.top.png.hex": "layers.top.png",
    }
    SCANS = True


if __name__ == "__main__":
    unittest.main()
