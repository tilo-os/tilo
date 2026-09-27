"""Exercise binary selection through the real build script and Cargo resolver."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


def text_size(binary: Path) -> int:
    out = subprocess.check_output(["readelf", "-W", "-S", str(binary)], text=True)
    for line in out.splitlines():
        if "PROGBITS" not in line:
            continue
        parts = line.split()
        try:
            name_i = parts.index(".text")
        except ValueError:
            continue
        return int(parts[name_i + 4], 16)
    raise AssertionError(f"no .text in readelf -S for {binary}\n{out}")


class ThinAppSelectionTests(unittest.TestCase):
    def setUp(self):
        for tool in ("cargo", "rustc", "bash", "readelf"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required")
        self.temp = tempfile.TemporaryDirectory(prefix="thin-app-selection-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ("os/scripts/build-thin-apps.sh", "tools/ci/rustc-thin-app-wrapper.sh"):
            self.write(name, (ROOT / name).read_text())
        self.write("Cargo.toml", """[workspace]
members = ["apps/calculator", "apps/other", "apps/browser", "runtime", "shared", "app_sdk"]
resolver = "2"
[profile.thin-app]
inherits = "dev"
codegen-units = 1
""")
        self.write("shared/Cargo.toml", """[package]
name = "shared"
version = "0.1.0"
edition = "2024"
[features]
extra = []
webview = []
""")
        self.write("shared/src/lib.rs", """pub fn value() -> usize {
    #[cfg(feature = "webview")]
    {
        // Link the webview .text into whatever crate graph enables the feature.
        core::hint::black_box(webview_bloat());
    }
    if cfg!(feature = "extra") { 2 } else { 1 }
}

#[cfg(feature = "webview")]
#[inline(never)]
pub fn webview_bloat() -> u32 {
    let mut total = 0u32;
    let mut cell = 1u32;
    for _ in 0..250_000 {
        cell = cell.wrapping_mul(1664525).wrapping_add(1013904223);
        total = total.wrapping_add(cell);
        core::hint::black_box(cell);
    }
    core::hint::black_box(total)
}
""")
        self.write("app_sdk/Cargo.toml", """[package]
name = "app-sdk"
version = "0.1.0"
edition = "2024"
[features]
webview = ["shared/webview"]
[dependencies]
shared = { path = "../shared" }
""")
        self.write("app_sdk/src/lib.rs", "pub use shared::*;\n")
        self.write("runtime/Cargo.toml", """[package]
name = "runtime-dylib"
version = "0.1.0"
edition = "2024"
[lib]
crate-type = ["dylib"]
[dependencies]
shared = { path = "../shared" }
""")
        self.write("runtime/src/lib.rs", """pub use shared;
pub fn value() -> usize { shared::value() }
""")
        for name in ("calculator", "other"):
            extra = ', features = ["extra"]' if name == "other" else ""
            self.write(f"apps/{name}/Cargo.toml", f"""[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
[features]
thin = ["dep:runtime-dylib"]
[dependencies]
runtime-dylib = {{ path = "../../runtime", optional = true }}
app-sdk = {{ path = "../../app_sdk" }}
shared = {{ path = "../../shared"{extra} }}
""")
            self.write(
                f"apps/{name}/src/main.rs",
                'fn main() { assert_eq!(runtime_dylib::value() & 1, 0); '
                'assert_eq!(app_sdk::value() & 1, 0); println!("unified-runtime-ok"); }',
            )
            self.write(f"apps/{name}/tilo-app.toml", f'bin = "bin/{name}"\n')
        # Models apps/browser: excluded from the CI collection. When pulled into
        # the image thin graph, browser/wpe unified ui/webview into shared deps.
        self.write("apps/browser/Cargo.toml", """[package]
name = "browser"
version = "0.1.0"
edition = "2024"
[features]
thin = ["dep:runtime-dylib"]
wpe = ["app-sdk/webview"]
[dependencies]
runtime-dylib = { path = "../../runtime", optional = true }
app-sdk = { path = "../../app_sdk" }
""")
        self.write("apps/browser/src/main.rs", 'fn main() { let _ = app_sdk::value(); }\n')
        self.write("apps/browser/tilo-app.toml", 'bin = "bin/browser"\n')
        self.run_command("cargo", "generate-lockfile", "--offline")

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, newline="\n")
        path.chmod(0o755)

    def run_command(self, *args, **overrides):
        env = dict(os.environ)
        for key in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                    "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "APP_FEATURES", "EXCLUDE_PACKAGES",
                    "INCLUDE_PACKAGES", "BUILD_PACKAGES",
                    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CARGO_PROFILE_RELEASE_LTO"):
            env.pop(key, None)
        env["REPO"] = str(self.root)
        env.setdefault("INCLUDE_PACKAGES", "calculator other")
        env.update(overrides)
        return subprocess.run(args, cwd=self.root, env=env, text=True, capture_output=True, timeout=120)

    def test_package_selection(self):
        with self.subTest(phase="selected_binary_retains_full_runtime_features_and_hash"):
            selected = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="calculator")
            self.assertEqual(selected.returncode, 0, selected.stdout + selected.stderr)
            target = self.root / "target/thin-app"
            self.assertTrue((target / "calculator").is_file())
            self.assertFalse((target / "other").exists())
            self.assertNotIn("Compiling other", selected.stderr)
            runtime = target / "libruntime_dylib.so"
            before = hashlib.sha256(runtime.read_bytes()).hexdigest()
            complete = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="")
            self.assertEqual(complete.returncode, 0, complete.stdout + complete.stderr)
            self.assertTrue((target / "other").is_file())
            self.assertEqual(before, hashlib.sha256(runtime.read_bytes()).hexdigest())
            self.assertNotIn("Compiling runtime-dylib", complete.stderr)
            rustlib = self.run_command("rustc", "--print", "target-libdir").stdout.strip()
            started = self.run_command(str(target / "calculator"), LD_LIBRARY_PATH=f"{target}:{rustlib}")
            self.assertEqual(started.returncode, 0, started.stderr)
            self.assertIn("unified-runtime-ok", started.stdout)
        restart_fixture(self)
        with self.subTest(phase="unknown_selected_package_fails_before_cargo"):
            result = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="calculator missing")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("outside the runtime graph: missing", result.stderr)
            self.assertFalse((self.root / "target").exists())
        restart_fixture(self)
        with self.subTest(phase="browser_wpe_unifies_webview_into_runtime_text"):
            # CI collection excludes browser. The image used to add browser/wpe
            # to the thin graph (#1158), unifying webview into shared crates.
            ci = self.run_command(
                "bash", "os/scripts/build-thin-apps.sh",
                INCLUDE_PACKAGES="calculator other",
                EXCLUDE_PACKAGES="browser",
                BUILD_PACKAGES="calculator",
            )
            self.assertEqual(ci.returncode, 0, ci.stdout + ci.stderr)
            ci_app = text_size(self.root / "target/thin-app/calculator")
            ci_rt = text_size(self.root / "target/thin-app/libruntime_dylib.so")
            restart_fixture(self)
            image = self.run_command(
                "bash", "os/scripts/build-thin-apps.sh",
                INCLUDE_PACKAGES="calculator other browser",
                BUILD_PACKAGES="calculator",
                APP_FEATURES="browser/wpe",
            )
            self.assertEqual(image.returncode, 0, image.stdout + image.stderr)
            image_app = text_size(self.root / "target/thin-app/calculator")
            image_rt = text_size(self.root / "target/thin-app/libruntime_dylib.so")
            # Prefer-dynamic keeps the unified webview code in the dylib. A
            # check that cannot see the graph difference must fail: require the
            # runtime .text to grow by the bloat loop (~1.9 MB class).
            # Prefer-dynamic keeps unified webview code out of the tapp, so the
            # app .text stays flat; the runtime must grow or the check is blind.
            self.assertEqual(
                image_app, ci_app,
                f"prefer-dynamic should keep browser/wpe out of the tapp .text; "
                f"ci_app={ci_app} image_app={image_app}",
            )
            self.assertGreater(
                image_rt, ci_rt,
                f"browser/wpe must enlarge the shared runtime .text; "
                f"ci_rt={ci_rt} image_rt={image_rt}",
            )


if __name__ == "__main__":
    unittest.main()
