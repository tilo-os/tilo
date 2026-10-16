"""Exercise binary selection through the real build script and Cargo resolver."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

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
        for name in ("os/scripts/build-thin-apps.sh", "tools/ci/rustc-thin-app-wrapper.sh",
                     "tools/ci/runtime-dylib-lazy-link.py", "tools/ci/thin-app/sccache"):
            self.write(name, (ROOT / name).read_text())
        self.write("Cargo.toml", """[workspace]
members = ["apps/calculator", "apps/other", "apps/extra", "runtime", "shared", "app_sdk",
           "crates/runtime/engines/fixture"]
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
        # A runtime engine: a dylib chained on the runtime, built
        # with it whichever apps are selected, and linked only by `other`.
        self.write("crates/runtime/engines/fixture/Cargo.toml", """[package]
name = "tilo-fixture-engine"
version = "0.1.0"
edition = "2024"
[lib]
name = "tilo_fixture_engine"
crate-type = ["dylib"]
[[bin]]
name = "tilo-fixture-engine-check"
path = "src/check.rs"
required-features = ["thin"]
[features]
thin = ["dep:runtime-dylib"]
[dependencies]
runtime-dylib = { path = "../../../../runtime", optional = true }
""")
        self.write("crates/runtime/engines/fixture/src/lib.rs", """#[cfg(feature = "thin")]
pub fn engine_value() -> usize { runtime_dylib::value() + 40 }
""")
        self.write("crates/runtime/engines/fixture/src/check.rs",
                   'fn main() { assert_eq!(tilo_fixture_engine::engine_value() & 1, 0); '
                   'println!("engine-runs-against-runtime"); }\n')
        for name in ("calculator", "other"):
            extra = ', features = ["extra"]' if name == "other" else ""
            engine = ('runtime-engine = ["thin", "tilo-fixture-engine/thin"]\n'
                      if name == "other" else "")
            engine_dependency = ('tilo-fixture-engine = { path = "../../crates/runtime/engines/fixture", '
                                 'optional = true }\n' if name == "other" else "")
            self.write(f"apps/{name}/Cargo.toml", f"""[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
[lib]
name = "{name}_support"
[features]
thin = ["dep:runtime-dylib"]
{engine}[dependencies]
runtime-dylib = {{ path = "../../runtime", optional = true }}
{engine_dependency}app-sdk = {{ path = "../../app_sdk" }}
shared = {{ path = "../../shared"{extra} }}
""")
            uses_engine = ('#[cfg(feature = "runtime-engine")]\nextern crate tilo_fixture_engine;\n'
                           if name == "other" else "")
            engine_call = ('#[cfg(feature = "runtime-engine")] '
                           'assert_eq!(tilo_fixture_engine::engine_value() & 1, 0); '
                           if name == "other" else "")
            self.write(
                f"apps/{name}/src/main.rs",
                uses_engine + 'fn main() { assert_eq!(runtime_dylib::value() & 1, 0); '
                'assert_eq!(app_sdk::value() & 1, 0); ' + engine_call + 'println!("unified-runtime-ok"); }',
            )
            self.write(f"apps/{name}/src/lib.rs", "pub fn library_value() -> usize { 42 }\n")
            self.write(f"apps/{name}/tilo-app.toml", f'bin = "bin/{name}"\n')
        # An app outside a collection whose feature unifies more code into the
        # shared crates when the image's thin graph pulls it in (#1158).
        self.write("apps/extra/Cargo.toml", """[package]
name = "extra"
version = "0.1.0"
edition = "2024"
[features]
thin = ["dep:runtime-dylib"]
bloat = ["app-sdk/webview"]
[dependencies]
runtime-dylib = { path = "../../runtime", optional = true }
app-sdk = { path = "../../app_sdk" }
""")
        self.write("apps/extra/src/main.rs", 'fn main() { let _ = app_sdk::value(); }\n')
        self.write("apps/extra/tilo-app.toml", 'bin = "bin/extra"\n')
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

    def test_explicit_compiler_selection(self):
        # Real Cargo must select the caller's compiler before it can compile
        # anything. Refusing -vV gives this production-path test zero codegen.
        tools = self.root / "compiler-probes"
        tools.mkdir()
        marker = self.root / "compiler-selected"
        for name, label in (("rustc", "default"), ("chosen-compiler", "explicit")):
            self.write(f"compiler-probes/{name}",
                       f"#!/bin/sh\nprintf '{label}\\n' >> '{marker}'\nexit 97\n")
        compiler = tools / "chosen-compiler"
        cargo_home = self.root / "cargo-home"
        cargo_home.mkdir()
        config_dir = self.root / ".cargo"
        config_dir.mkdir()
        cli_config = self.root / "compiler.toml"
        cli_config.write_text(f'[build]\nrustc = "{compiler}"\n')
        cases = (
            ("RUSTC", {"RUSTC": str(compiler)}, None),
            ("CARGO_BUILD_RUSTC", {"CARGO_BUILD_RUSTC": str(compiler)}, None),
            ("inline config", {"EXTRA_CARGO_ARGS": f'--config build.rustc="{compiler}"'}, None),
            ("CLI config file", {"EXTRA_CARGO_ARGS": f"--config {cli_config}"}, None),
            ("project config", {}, config_dir / "config.toml"),
            ("legacy project config", {}, config_dir / "config"),
            ("Cargo home config", {}, cargo_home / "config.toml"),
        )
        with patch.dict(os.environ):
            for key in ("RUSTC", "CARGO_BUILD_RUSTC", "EXTRA_CARGO_ARGS"):
                os.environ.pop(key, None)
            for name, overrides, config in cases:
                with self.subTest(compiler_source=name):
                    marker.unlink(missing_ok=True)
                    if config is not None:
                        config.write_text(cli_config.read_text())
                    try:
                        result = self.run_command(
                            "bash", "os/scripts/build-thin-apps.sh",
                            PATH=str(tools) + os.pathsep + os.environ["PATH"],
                            CARGO_HOME=str(cargo_home), **overrides,
                        )
                        self.assertNotEqual(result.returncode, 0, "probe compiler unexpectedly succeeded")
                        self.assertTrue(marker.exists(), result.stdout + result.stderr)
                        selected = marker.read_text().splitlines()
                        self.assertIn("explicit", selected, result.stdout + result.stderr)
                        self.assertNotIn("default", selected, "script masked Cargo's explicit compiler")
                    finally:
                        if config is not None:
                            config.unlink()

    def test_package_selection(self):
        with self.subTest(phase="selected_binary_retains_full_runtime_features_and_hash"):
            maps = self.root / "link-maps"
            selected = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="calculator",
                                        TILO_THIN_LINK_MAP_DIR=str(maps), EXTRA_CARGO_ARGS="-v")
            self.assertEqual(selected.returncode, 0, selected.stdout + selected.stderr)
            library_compile = [line for line in selected.stderr.splitlines()
                               if "Running `" in line and "--crate-name calculator_support" in line]
            self.assertEqual(len(library_compile), 1, selected.stderr)
            self.assertIn("-C opt-level=z", library_compile[0])
            self.assertIn("-C codegen-units=1", library_compile[0])
            target = self.root / "target/thin-app"
            self.assertTrue((target / "calculator").is_file())
            # The size wrapper wrote the app's link map, so it compiled the app.
            self.assertTrue((maps / "calculator.map").is_file())
            self.assertFalse((target / "other").exists())
            self.assertNotIn("Compiling other", selected.stderr)
            runtime = target / "libruntime_dylib.so"
            before = hashlib.sha256(runtime.read_bytes()).hexdigest()
            # The engine is built and checked although no app that uses it
            # was selected: the runtime bundle cannot depend on the selection.
            engine = target / "libtilo_fixture_engine.so"
            self.assertEqual((target / "runtime-engines").read_text(), "libtilo_fixture_engine.so\n")
            self.assertIn("engine-runs-against-runtime", selected.stdout)
            needed = subprocess.check_output(["readelf", "-dW", str(engine)], text=True)
            self.assertIn("[libruntime_dylib.so]", needed)
            self.assertNotIn("libtilo_fixture_engine.so",
                             subprocess.check_output(["readelf", "-dW", str(target / "calculator")], text=True))
            engine_before = hashlib.sha256(engine.read_bytes()).hexdigest()
            complete = self.run_command("bash", "os/scripts/build-thin-apps.sh", BUILD_PACKAGES="",
                                        APP_FEATURES="other/runtime-engine")
            self.assertEqual(complete.returncode, 0, complete.stdout + complete.stderr)
            self.assertTrue((target / "other").is_file())
            self.assertEqual(before, hashlib.sha256(runtime.read_bytes()).hexdigest())
            self.assertEqual(engine_before, hashlib.sha256(engine.read_bytes()).hexdigest())
            # The one app that uses the engine loads it, against the same runtime.
            self.assertIn("[libtilo_fixture_engine.so]",
                          subprocess.check_output(["readelf", "-dW", str(target / "other")], text=True))
            rustlib = self.run_command("rustc", "--print", "target-libdir").stdout.strip()
            other = self.run_command(str(target / "other"), LD_LIBRARY_PATH=f"{target}:{rustlib}")
            self.assertEqual(other.returncode, 0, other.stderr)
            self.assertIn("unified-runtime-ok", other.stdout)
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
        with self.subTest(phase="a_divergent_graph_unifies_more_into_runtime_text"):
            # A thin graph that pulls in a package or feature the collection
            # leaves out unifies more code into the shared crates (#1158).
            ci = self.run_command(
                "bash", "os/scripts/build-thin-apps.sh",
                INCLUDE_PACKAGES="calculator other",
                EXCLUDE_PACKAGES="extra",
                BUILD_PACKAGES="calculator",
            )
            self.assertEqual(ci.returncode, 0, ci.stdout + ci.stderr)
            ci_app = text_size(self.root / "target/thin-app/calculator")
            ci_rt = text_size(self.root / "target/thin-app/libruntime_dylib.so")
            restart_fixture(self)
            image = self.run_command(
                "bash", "os/scripts/build-thin-apps.sh",
                INCLUDE_PACKAGES="calculator other extra",
                BUILD_PACKAGES="calculator",
                APP_FEATURES="extra/bloat",
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
                f"prefer-dynamic should keep extra/bloat out of the tapp .text; "
                f"ci_app={ci_app} image_app={image_app}",
            )
            self.assertGreater(
                image_rt, ci_rt,
                f"extra/bloat must enlarge the shared runtime .text; "
                f"ci_rt={ci_rt} image_rt={image_rt}",
            )


if __name__ == "__main__":
    unittest.main()
