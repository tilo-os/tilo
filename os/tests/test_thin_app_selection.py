"""Exercise binary selection through the real build script and Cargo resolver."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ThinAppSelectionTests(unittest.TestCase):
    def setUp(self):
        for tool in ("cargo", "rustc", "bash"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required")
        self.temp = tempfile.TemporaryDirectory(prefix="thin-app-selection-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ("os/scripts/build-thin-apps.sh", "tools/ci/rustc-thin-app-wrapper.sh"):
            self.write(name, (ROOT / name).read_text())
        self.write("Cargo.toml", '''[workspace]
members = ["apps/calculator", "apps/other", "runtime", "shared"]
resolver = "2"
[profile.thin-app]
inherits = "dev"
''')
        self.write("shared/Cargo.toml", '''[package]
name = "shared"
version = "0.1.0"
edition = "2024"
[features]
extra = []
''')
        self.write("shared/src/lib.rs", 'pub fn value() -> usize { if cfg!(feature="extra") { 2 } else { 1 } }')
        self.write("runtime/Cargo.toml", '''[package]
name = "runtime-dylib"
version = "0.1.0"
edition = "2024"
[lib]
crate-type = ["dylib"]
[dependencies]
shared = { path = "../shared" }
''')
        self.write("runtime/src/lib.rs", 'pub fn value() -> usize { shared::value() }')
        for name in ("calculator", "other"):
            extra = ', features = ["extra"]' if name == "other" else ''
            self.write(f"apps/{name}/Cargo.toml", f'''[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
[features]
thin = ["dep:runtime-dylib"]
[dependencies]
runtime-dylib = {{ path = "../../runtime", optional = true }}
shared = {{ path = "../../shared"{extra} }}
''')
            self.write(f"apps/{name}/src/main.rs", 'fn main() { assert_eq!(runtime_dylib::value(), 2); println!("unified-runtime-ok"); }')
            self.write(f"apps/{name}/tilo-app.toml", f'bin = "bin/{name}"\n')
        self.run_command("cargo", "generate-lockfile", "--offline")

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, newline="\n")
        path.chmod(0o755)

    def run_command(self, *args, **overrides):
        env = dict(os.environ)
        for key in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                    "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "APP_FEATURES", "EXCLUDE_PACKAGES"):
            env.pop(key, None)
        env.update(REPO=str(self.root), INCLUDE_PACKAGES="calculator other", **overrides)
        return subprocess.run(args, cwd=self.root, env=env, text=True, capture_output=True, timeout=120)

    def test_selected_binary_retains_full_runtime_features_and_hash(self):
        selected = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="calculator")
        self.assertEqual(selected.returncode, 0, selected.stdout + selected.stderr)
        target = self.root / "target/thin-app"
        self.assertTrue((target / "calculator").is_file())
        self.assertFalse((target / "other").exists())
        self.assertNotIn("Compiling other", selected.stderr)
        runtime = target / "libruntime_dylib.so"
        before = hashlib.sha256(runtime.read_bytes()).hexdigest()
        # Resolve the same graph and build the entire fleet. The selected build
        # must already have the full feature union, not a different runtime ABI.
        complete = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="")
        self.assertEqual(complete.returncode, 0, complete.stdout + complete.stderr)
        self.assertTrue((target / "other").is_file())
        self.assertEqual(before, hashlib.sha256(runtime.read_bytes()).hexdigest())
        self.assertNotIn("Compiling runtime-dylib", complete.stderr)
        rustlib = self.run_command("rustc", "--print", "target-libdir").stdout.strip()
        started = self.run_command(str(target / "calculator"), LD_LIBRARY_PATH=f"{target}:{rustlib}")
        self.assertEqual(started.returncode, 0, started.stderr)
        self.assertIn("unified-runtime-ok", started.stdout)

    def test_unknown_selected_package_fails_before_cargo(self):
        result = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="calculator missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside the runtime graph: missing", result.stderr)
        self.assertFalse((self.root / "target").exists())


if __name__ == "__main__":
    unittest.main()
