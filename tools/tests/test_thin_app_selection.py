#!/usr/bin/env python3
"""Exercise real Cargo target filtering and the production thin-app ABI check.

The fixture has no registry dependencies. It proves that an unbuilt app's
dependency features remain in the runtime graph, using the pinned compiler.
It is not a substitute for a native full-collection release qualification.
"""
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
        for tool in ("bash", "cargo", "rustc"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required")
        self.temp = tempfile.TemporaryDirectory(prefix="thin-app-selection-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for relative in ("os/scripts/build-thin-apps.sh", "tools/ci/rustc-thin-app-wrapper.sh",
                         "rust-toolchain.toml"):
            self.write(relative, (ROOT / relative).read_text())
        self.write("Cargo.toml", '''[workspace]
resolver = "2"
members = ["shared", "runtime-dylib", "apps/first", "apps/second"]
[profile.thin-app]
inherits = "release"
panic = "unwind"
lto = false
''')
        self.write("shared/Cargo.toml", '''[package]
name = "shared"
version = "0.1.0"
edition = "2024"
[features]
extra = []
''')
        self.write("shared/src/lib.rs", '''pub fn base() -> u32 { 1 }
#[cfg(feature = "extra")] pub fn extra() -> u32 { 42 }
''')
        self.write("runtime-dylib/Cargo.toml", '''[package]
name = "runtime-dylib"
version = "0.1.0"
edition = "2024"
[lib]
crate-type = ["dylib"]
[dependencies]
shared = { path = "../shared" }
''')
        self.write("runtime-dylib/src/lib.rs", "pub use shared::*;\n")
        for name in ("first", "second"):
            extra = ', features = ["extra"]' if name == "second" else ""
            self.write(f"apps/{name}/Cargo.toml", f'''[package]
name = "{name}-package"
version = "0.1.0"
edition = "2024"
[lib]
name = "{name}"
[[bin]]
name = "{name}"
path = "src/main.rs"
[features]
thin = ["dep:runtime-dylib"]
[dependencies]
runtime-dylib = {{ path = "../../runtime-dylib", optional = true }}
shared = {{ path = "../../shared"{extra} }}
''')
            function = "extra" if name == "second" else "base"
            self.write(f"apps/{name}/src/lib.rs",
                       f'pub fn value() -> u32 {{ shared::{function}() }}\n')
            self.write(f"apps/{name}/src/main.rs",
                       f'extern crate runtime_dylib;\nfn main() {{ println!("{{}}", {name}::value()); }}\n')
            self.write(f"apps/{name}/tilo-app.toml", f'[entrypoint]\nbin = "bin/{name}"\n')
        self.env = dict(os.environ, REPO=str(self.root), INCLUDE_PACKAGES="first-package second-package",
                        CARGO_NET_OFFLINE="true")
        for name in ("CARGO_TARGET_DIR", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                     "CARGO_BUILD_TARGET", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                     "BUILD_APP_PACKAGES", "APP_FEATURES", "EXCLUDE_PACKAGES", "PROFILE"):
            self.env.pop(name, None)
        self.run_command(["cargo", "generate-lockfile", "--offline"])

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")
        if path.suffix == ".sh":
            path.chmod(0o755)

    def run_command(self, command, *, env=None, succeeds=True):
        result = subprocess.run(command, cwd=self.root, env=env or self.env,
                                capture_output=True, text=True, timeout=60)
        if succeeds:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def build(self, selection=None, *, succeeds=True):
        env = dict(self.env)
        if selection is not None:
            env["BUILD_APP_PACKAGES"] = selection
        return self.run_command(["bash", "os/scripts/build-thin-apps.sh"],
                                env=env, succeeds=succeeds)

    def test_cold_subset_preserves_runtime_and_unbuilt_apps_feature_union(self):
        result = self.build()
        self.assertIn("built 2 thin qpkg apps", result.stdout)
        output = self.root / "target/thin-app"
        runtime_hash = hashlib.sha256((output / "libruntime_dylib.so").read_bytes()).hexdigest()
        saved_second = self.root / "previous-second"
        shutil.copy2(output / "second", saved_second)
        # A clean target proves there is no dependence on a stale root dylib or
        # a previously compiled excluded app. Cargo must still resolve second's
        # shared/extra feature without compiling second's failing source.
        shutil.rmtree(self.root / "target")
        self.write("apps/second/src/main.rs", 'compile_error!("unselected app was compiled");\n')
        self.write("apps/second/src/lib.rs", 'compile_error!("unselected app was compiled");\n')
        result = self.build("first-package")
        self.assertIn("built 1 thin qpkg apps", result.stdout)
        self.assertFalse((output / "second").exists())
        self.assertEqual(hashlib.sha256((output / "libruntime_dylib.so").read_bytes()).hexdigest(),
                         runtime_hash)
        stdlib = self.run_command(["rustc", "--print", "target-libdir"]).stdout.strip()
        env = dict(self.env, LD_LIBRARY_PATH=f"{output}:{stdlib}")
        self.assertEqual(self.run_command([str(saved_second)], env=env).stdout.strip(), "42")
        result = self.build(succeeds=False)
        self.assertIn("unselected app was compiled", result.stderr)

    def test_unknown_or_excluded_selection_fails_before_compilation(self):
        for selection in ("typo", "first-package typo", "first", "", "   "):
            with self.subTest(selection=selection):
                self.build(selection, succeeds=False)
                self.assertFalse((self.root / "target").exists())
        self.env["EXCLUDE_PACKAGES"] = "second-package"
        result = self.build("second-package", succeeds=False)
        self.assertIn("outside the runtime graph", result.stderr)
        self.assertFalse((self.root / "target").exists())

    def test_selected_compile_failure_is_not_hidden(self):
        self.write("apps/first/src/main.rs", 'compile_error!("selected app is broken");\n')
        result = self.build("first-package", succeeds=False)
        self.assertIn("selected app is broken", result.stderr)


if __name__ == "__main__":
    unittest.main()
