"""os/scripts/cargo-freshness.sh never lets Cargo reuse an artifact of old sources.

The image builders keep their target directory between builds and mark sources
by content. A source a build changed counts as built only once that build
succeeded: a build that stopped first (a failed release graph takes the thin
apps building beside it down too) left that profile's artifacts of the old
sources, and a vendored crate's changed API then failed to resolve in the next
build, which had marked the crate unchanged.
"""

from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/scripts/cargo-freshness.sh"

OLD_API = "pub fn value() -> u32 { 1 }\n"
NEW_API = "pub fn value() -> u32 { 1 }\npub fn announce() -> u32 { 2 }\n"


class CargoFreshness(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.workspace = self.dir / "build"
        self.record = self.workspace / "target/.built-sources"
        self.write("Cargo.toml", '[workspace]\nmembers = ["app", "vendor/dep"]\nresolver = "2"\n')
        self.write(
            "app/Cargo.toml",
            '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n\n'
            '[dependencies]\ndep = { path = "../vendor/dep" }\n',
        )
        self.write("app/src/main.rs", "fn main() { println!(\"{}\", dep::value()); }\n")
        self.write(
            "vendor/dep/Cargo.toml",
            '[package]\nname = "dep"\nversion = "0.30.13"\nedition = "2021"\n',
        )
        self.write("vendor/dep/src/lib.rs", OLD_API)
        (self.workspace / "target").mkdir()

    def write(self, path: str, text: str) -> None:
        file = self.workspace / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)

    def freshness(self, *args: str) -> int:
        return subprocess.run(
            ["sh", str(SCRIPT), *args], cwd=self.dir, stdout=subprocess.DEVNULL, check=False
        ).returncode

    def prepare(self) -> int:
        return self.freshness(str(self.workspace), str(self.record))

    def built(self) -> None:
        self.assertEqual(self.freshness("--built", str(self.record)), 0)

    def marked_changed(self, path: str) -> bool:
        return os.stat(self.workspace / path).st_mtime > 1

    def test_first_build_cleans_and_records_only_after_it_succeeds(self) -> None:
        self.assertEqual(self.prepare(), 3)
        self.assertFalse(self.record.exists())
        self.assertFalse(self.marked_changed("vendor/dep/src/lib.rs"))
        self.built()
        self.assertEqual(self.prepare(), 0)
        self.assertFalse(self.marked_changed("vendor/dep/src/lib.rs"))

    def test_vendor_change_stays_marked_until_a_build_succeeds(self) -> None:
        self.prepare()
        self.built()
        self.write("vendor/dep/src/lib.rs", NEW_API)
        # Two builds that stop before compiling the vendored crate.
        for _ in range(2):
            self.assertEqual(self.prepare(), 0)
            self.assertTrue(self.marked_changed("vendor/dep/src/lib.rs"))
            self.assertFalse(self.marked_changed("app/src/main.rs"))
        self.built()
        self.assertEqual(self.prepare(), 0)
        self.assertFalse(self.marked_changed("vendor/dep/src/lib.rs"))

    def test_built_without_a_finished_prepare_vouches_for_nothing(self) -> None:
        self.prepare()
        self.built()
        Path(f"{self.record}.next").unlink(missing_ok=True)
        self.built()
        self.assertFalse(self.record.exists())
        self.assertEqual(self.prepare(), 3)

    @unittest.skipUnless(shutil.which("cargo"), "needs cargo")
    def test_cargo_rebuilds_a_vendored_crate_after_a_stopped_build(self) -> None:
        env = {
            **os.environ,
            "CARGO_TARGET_DIR": str(self.workspace / "target"),
            "CARGO_HOME": str(self.dir / "cargo-home"),
            "RUSTC_WRAPPER": "",
            "CARGO_BUILD_RUSTC_WRAPPER": "",
        }

        def cargo_build() -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                ["cargo", "build", "--offline", "-q", "-p", "app"],
                cwd=self.workspace, env=env, capture_output=True, text=True, check=False,
            )

        self.assertEqual(self.prepare(), 3)
        self.assertEqual(cargo_build().returncode, 0)
        self.built()
        self.write("vendor/dep/src/lib.rs", NEW_API)
        self.write("app/src/main.rs", "fn main() { println!(\"{}\", dep::announce()); }\n")
        self.assertEqual(self.prepare(), 0)
        # This build stops before Cargo runs; the next one must still see the change.
        self.assertEqual(self.prepare(), 0)
        result = cargo_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        app = subprocess.run(
            [str(self.workspace / "target/debug/app")], capture_output=True, text=True, check=True
        )
        self.assertEqual(app.stdout, "2\n", "Cargo kept the app and its dependency of old sources")


if __name__ == "__main__":
    unittest.main()
