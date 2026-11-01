"""The package sets t's checks lint and build (tools/ci/package_sets.py)."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from ci import package_sets  # noqa: E402


class PackageSetsTests(unittest.TestCase):
    def test_android_apps_declare_an_android_example(self):
        packages = {
            "notes": {"targets": [{"name": "notes", "kind": ["lib"]},
                                  {"name": "notes-android", "kind": ["example"]}]},
            "clock": {"targets": [{"name": "clock-android", "kind": ["bin"]}]},
            "files": {"targets": [{"name": "notes-android", "kind": ["example"]}]},
        }
        self.assertEqual(package_sets.android_apps(packages), {"notes"})

    def test_browser_crates_depend_on_wasm_bindgen_for_their_library(self):
        packages = {
            "web": {"dependencies": [{"name": "wasm-bindgen", "kind": None}]},
            "tests-only": {"dependencies": [{"name": "wasm-bindgen", "kind": "dev"}]},
            "native": {"dependencies": [{"name": "serde", "kind": None}]},
        }
        self.assertEqual(package_sets.browser_crates(packages), {"web"})

    def test_example_apps_live_under_examples_apps(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            packages = {
                "counter": {"manifest_path": str(root / "examples/apps/counter/Cargo.toml")},
                "gallery": {"manifest_path": str(root / "examples/gallery/Cargo.toml")},
                "notes": {"manifest_path": str(root / "apps/notes/Cargo.toml")},
            }
            self.assertEqual(package_sets.example_apps(packages, root), {"counter"})

    def test_a_unit_differs_between_architectures_by_its_cpu_cfgs_branches_and_intrinsics(self):
        differs = [
            '#[cfg(target_arch = "x86_64")]\nconst SYS_OPEN: i64 = 2;',
            'if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" }',
            '#[cfg(not(target_arch = "x86_64"))]\nfn fallback() {}',
            '#[cfg_attr(target_arch = "aarch64", ignore)]\n#[test]\nfn t() {}',
            '#[cfg(all(target_os = "linux", target_arch = "x86_64"))]\nmod sys;',
            'if std::env::consts::ARCH == "aarch64" { 1 } else { 2 }',
            'let other = if "x86_64" != consts::ARCH { 1 } else { 2 };',
            'match std::env::consts::ARCH { "x86_64" => 1, _ => 2 }',
            'std::arch::is_aarch64_feature_detected!("sve2")',
            'unsafe { core::arch::asm!("nop") }',
            'cmake.define("CMAKE_SYSTEM_PROCESSOR", env::var("CARGO_CFG_TARGET_ARCH")?);',
            'let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();\nlet x86 = arch == "x86_64";',
        ]
        same = [
            '#[cfg(target_arch = "wasm32")]\nmod web;',
            '#[cfg(not(target_arch = "wasm32"))]\nmod native;',
            '#[cfg(all(windows, target_arch = "aarch64"))]\nmod arm_windows;',
            '#[cfg(all(target_os = "macos", target_arch = "aarch64"))]\nfn sign() {}',
            'json!({"architecture": std::env::consts::ARCH})',
            'if record.arch != std::env::consts::ARCH { return Err(()) }',
            'path.join(std::env::consts::ARCH)',
            'if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") { return; }',
            'let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();\nlet web = arch == "wasm32";',
            '// target_arch = "x86_64" in a comment outside any cfg',
        ]
        for text in differs:
            with self.subTest(text=text):
                self.assertTrue(package_sets.arch_dependent(text))
        for text in same:
            with self.subTest(text=text):
                self.assertFalse(package_sets.arch_dependent(text))

    def test_per_arch_packages_are_those_with_such_a_file_of_their_own(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {
                "crates/seccomp/src/lib.rs": '#[cfg(target_arch = "x86_64")]\nconst N: u32 = 1;',
                "crates/seccomp/build.rs": "fn main() {}",
                "crates/probe/tests/isa.rs": 'if std::env::consts::ARCH == "aarch64" {}',
                "crates/plain/src/lib.rs": "pub fn f() {}",
                # A nested package's files are its own, and build output is nobody's.
                "crates/plain/nested/src/lib.rs": 'match std::env::consts::ARCH { _ => () }',
                "crates/plain/target/debug/build/out.rs": '#[cfg(target_arch = "x86_64")] fn g() {}',
                "crates/web/src/lib.rs": '#[cfg(target_arch = "wasm32")] mod page;',
            }
            for path, text in files.items():
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                (root / path).write_text(text)

            def package(path: str) -> dict:
                lib = root / path / "src/lib.rs"
                return {"manifest_path": str(root / path / "Cargo.toml"),
                        "targets": [{"kind": ["lib"], "src_path": str(lib)}] if lib.exists() else []}

            packages = {name: package(path) for name, path in (
                ("seccomp", "crates/seccomp"), ("probe", "crates/probe"), ("plain", "crates/plain"),
                ("nested", "crates/plain/nested"), ("web", "crates/web"))}
            self.assertEqual(package_sets.per_arch(packages), {"seccomp", "probe", "nested"})

    def test_the_workspace_packages_known_to_differ_between_architectures_are_found(self):
        # Each has an x86_64 and an aarch64 code path of its own; the derivation must keep
        # finding them, whatever else it finds.
        known = {
            "tilo-sandbox": "clone3 assembly, microvm or virt machine",
            "shell": "syscall table, open or creat",
            "tilo-agent-participant": "seccomp confinement",
            "app-cli": "ISA detection",
            "tilo-environment": "the host architecture",
            "ush": "ELF loader, runtime image by ISA",
            "fs-native": "open or creat",
            "tilo-sandbox-core": "seccompiler target",
            "windows-vm": "the guest architecture",
            "local-model-service": "CMAKE_SYSTEM_PROCESSOR, the AOT CPU check",
            "imager-backend": "capture build --arch",
            "store": "the native target in tests",
            "launcher": "seccomp in tests",
            "tilo-app-cli": "the other architecture in tests",
        }
        sys.path.insert(0, str(ROOT / "tools/ci"))
        import workspace_graph
        packages = {p["name"]: p for p in workspace_graph.cargo_metadata(ROOT)["packages"]}
        found = package_sets.per_arch(packages)
        self.assertEqual(sorted(set(known) - found), [])

    def test_the_crates_built_for_wasm_beyond_the_browser_crates_are_in_the_set(self):
        # A crate whose wasm build compiles only when this set builds it is the point: it is
        # built with its own features, so a target table it alone owns is what gets tested.
        packages = {name: {} for name in (*package_sets.WASM_ALSO, "native-only")}
        self.assertEqual(package_sets.wasm_crates(packages),
                         {"tilo-browser-peer", "channel-crypto", "tilo-peer-state"})

    def test_vendored_test_crates_are_workspaces_the_runner_tests_by_default(self):
        for crate in package_sets.VENDORED_TEST_CRATES:
            with self.subTest(crate=crate):
                self.assertTrue(crate.startswith("vendor/"))
                self.assertIn("[workspace]", (ROOT / crate / "Cargo.toml").read_text().splitlines())
        runner = (ROOT / "tools/ci/test-vendored-crates.sh").read_text()
        self.assertIn("from ci.package_sets import VENDORED_TEST_CRATES", runner)

    def test_an_unknown_set_is_refused(self):
        result = subprocess.run([sys.executable, str(ROOT / "tools/ci/package_sets.py"), "desktop"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: package_sets.py android|browser", result.stderr)


if __name__ == "__main__":
    unittest.main()
