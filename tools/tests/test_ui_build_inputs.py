"""Exercise the UI fixture build script's real Cargo invalidation boundary."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "crates/sdk/ui-core"
FIXTURES = {
    "tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex":
        "TiloSansFeaturesFixture-Regular.ttf",
    "tests/fixtures/test_image.png.hex": "test_image.png",
    "../../browser/paint-stream/tests/fixtures/layers.top.png.hex": "layers.top.png",
}


class UiBuildInputs(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.package = Path(self.temporary.name) / "crates/sdk/ui-core"
        (self.package / "src").mkdir(parents=True)
        (self.package / "Cargo.toml").write_text(
            '[package]\nname = "ui-build-inputs"\nversion = "0.0.0"\n'
            'edition = "2024"\nautotests = false\n[workspace]\n'
        )
        shutil.copyfile(SOURCE / "build.rs", self.package / "build.rs")
        self.original = {}
        for source, filename in FIXTURES.items():
            destination = self.package / source
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SOURCE / source, destination)
            self.original[filename] = bytes.fromhex(destination.read_text())
        (self.package / "src/lib.rs").write_text("\n".join(
            f'pub static FIXTURE_{index}: &[u8] = '
            f'include_bytes!(concat!(env!("OUT_DIR"), "/{filename}"));'
            for index, filename in enumerate(FIXTURES.values())
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
                        and message["target"]["kind"] == ["lib"])
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
        for source, filename in FIXTURES.items():
            with self.subTest(source=source):
                path = self.package / source
                path.write_text(path.read_text() + "\n00\n")
                fresh, output = self.build()
                self.assertFalse(fresh)
                self.original[filename] += b"\0"
                for name, expected in self.original.items():
                    self.assertEqual((output / name).read_bytes(), expected)


if __name__ == "__main__":
    unittest.main()
