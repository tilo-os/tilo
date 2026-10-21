"""The wrapper of t's check and clippy families (tools/ci/t_cargo_compile.py)."""
from pathlib import Path
import sys
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import package_sets  # noqa: E402
import t_cargo_compile  # noqa: E402

FORMAT = ["--message-format", "json-render-diagnostics"]
WASM = {"tilo-browser-runtime", "tilo-net", "tilo-browser-peer"}


class WasmPassTests(unittest.TestCase):
    def test_a_browser_crate_is_linted_for_wasm32_first_as_its_library(self):
        argv = ["cargo", "clippy", *FORMAT, "-p", "shell", "-p", "tilo-browser-runtime", "-p", "tilo-net",
                "--locked", "--all-targets", "--", "-D", "warnings"]
        allow = [arg for lint in package_sets.WASM_CLIPPY_ALLOW for arg in ("-A", lint)]
        self.assertEqual(t_cargo_compile.wasm_passes(argv, WASM), [
            ["cargo", "clippy", *FORMAT, "--locked", "--target", "wasm32-unknown-unknown", "--lib",
             "-p", "tilo-browser-runtime", "-p", "tilo-net", "--", "-D", "warnings", *allow],
        ])

    def test_a_crate_the_runtime_builds_without_its_defaults_is_checked_with_the_runtime_s_features(self):
        argv = ["cargo", "check", *FORMAT, "-p", "tilo-browser-peer", "--locked", "--all-targets"]
        self.assertEqual(t_cargo_compile.wasm_passes(argv, WASM), [
            ["cargo", "check", *FORMAT, "--locked", "--target", "wasm32-unknown-unknown", "--lib",
             "-p", "tilo-browser-peer", *package_sets.WASM_ALSO["tilo-browser-peer"]],
        ])

    def test_native_crates_and_other_commands_get_no_wasm_pass(self):
        self.assertEqual(t_cargo_compile.wasm_passes(
            ["cargo", "clippy", "-p", "shell", "--all-targets"], WASM), [])
        self.assertEqual(t_cargo_compile.wasm_passes(
            ["cargo", "build", "-p", "tilo-net"], WASM), [])

    def test_the_browser_runtime_s_features_for_its_peer_are_the_ones_the_wrapper_uses(self):
        manifest = tomllib.loads((ROOT / "crates/browser/tilo-browser-runtime/Cargo.toml").read_text())
        tables = [manifest.get("dependencies", {}),
                  *(target.get("dependencies", {}) for target in manifest.get("target", {}).values())]
        peer = next(table["tilo-browser-peer"] for table in tables if "tilo-browser-peer" in table)
        flags = (["--no-default-features"] if peer.get("default-features") is False else [])
        flags += [arg for feature in peer.get("features", []) for arg in ("--features", feature)]
        self.assertEqual(list(package_sets.WASM_ALSO["tilo-browser-peer"]), flags)

    def test_the_check_and_clippy_families_run_under_it(self):
        config = tomllib.loads((ROOT / "t.toml").read_text())
        families = {family["name"]: family for family in config["cargo"]}
        for name in ("check", "clippy"):
            with self.subTest(family=name):
                self.assertEqual(families[name]["wrapper"], ["python3", "tools/ci/t_cargo_compile.py"])
                self.assertIn("tools/ci/t_cargo_compile.py", families[name]["inputs"])
                self.assertIn("tools/ci/package_sets.py", families[name]["inputs"])


if __name__ == "__main__":
    unittest.main()
