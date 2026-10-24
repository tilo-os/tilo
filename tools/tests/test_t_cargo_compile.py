"""The wrapper of t's check and clippy families (tools/ci/t_cargo_compile.py)."""
from pathlib import Path
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import package_features  # noqa: E402
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

    def test_clippy_lints_a_package_with_the_features_its_tests_are_verified_with(self):
        argv = ["cargo", "clippy", *FORMAT, "-p", "shell", "-p", "tilo-media",
                "--locked", "--all-targets", "--", "-D", "warnings"]
        # Through workspace-unify, which depends on tilo-media with them, never as --features.
        self.assertEqual(t_cargo_compile.unified(argv),
                         [*argv[:-3], "-p", "workspace-unify", "--", "-D", "warnings"])
        anchor = tomllib.loads((ROOT / "crates/dev/workspace-unify/Cargo.toml").read_text())
        for table in anchor["target"].values():
            media = table.get("dependencies", {}).get("tilo-media")
            if media is not None:
                self.assertLessEqual(set(package_features.NATIVE_TEST_FEATURES["tilo-media"]), set(media["features"]))
        self.assertEqual(t_cargo_compile.with_test_features(["cargo", "clippy", "-p", "shell"]),
                         ["cargo", "clippy", "-p", "shell"])

    def test_the_workspace_form_lints_every_package_s_test_features_and_default_builds(self):
        argv = ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"]
        features = package_features.arguments(list(package_features.NATIVE_TEST_FEATURES))
        self.assertEqual(t_cargo_compile.unified(argv),
                         [*argv[:-3], *features, "--", "-D", "warnings"])
        self.assertEqual(t_cargo_compile.default_build_passes(argv), [
            ["cargo", "clippy", "--locked", "--all-targets", "-p", name, "--", "-D", "warnings"]
            for name in sorted(package_features.DEFAULT_BUILDS_ALSO_CHECKED)])

    def test_a_package_tested_with_features_it_does_not_ship_is_also_linted_as_it_ships(self):
        argv = ["cargo", "clippy", *FORMAT, "-p", "imager", "-p", "shell",
                "--locked", "--all-targets", "--", "-D", "warnings"]
        self.assertIn("imager", package_features.DEFAULT_BUILDS_ALSO_CHECKED)
        self.assertEqual(t_cargo_compile.default_build_passes(argv), [
            ["cargo", "clippy", *FORMAT, "--locked", "--all-targets", "-p", "imager", "--", "-D", "warnings"],
        ])

    def test_check_gets_neither_test_features_nor_default_build_passes(self):
        argv = ["cargo", "check", *FORMAT, "-p", "imager", "-p", "tilo-media", "--locked", "--all-targets"]
        self.assertEqual(t_cargo_compile.with_test_features(argv), argv)
        self.assertEqual(t_cargo_compile.default_build_passes(argv), [])

    def test_the_default_build_pass_passes_on_all_but_its_artifacts(self):
        artifact = '{"reason":"compiler-artifact","target":{"name":"imager"}}'
        error = '{"reason":"compiler-message","message":{"level":"error"}}'
        script = f"print({artifact!r}); print({error!r}); print('plain'); raise SystemExit(101)"
        with tempfile.TemporaryFile("w+") as out:
            saved = sys.stdout
            sys.stdout = out
            try:
                code = t_cargo_compile.without_artifacts([sys.executable, "-c", script])
            finally:
                sys.stdout = saved
            out.seek(0)
            self.assertEqual(out.read().splitlines(), [error, "plain"])
        self.assertEqual(code, 101)

    def test_the_check_and_clippy_families_run_under_it(self):
        config = tomllib.loads((ROOT / "t.toml").read_text())
        families = {family["name"]: family for family in config["cargo"]}
        for name in ("check", "clippy"):
            with self.subTest(family=name):
                self.assertEqual(families[name]["wrapper"], ["python3", "tools/ci/t_cargo_compile.py"])
                self.assertIn("tools/ci/t_cargo_compile.py", families[name]["inputs"])
                self.assertIn("tools/ci/package_sets.py", families[name]["inputs"])
                self.assertIn("tools/ci/package_features.py", families[name]["inputs"])


if __name__ == "__main__":
    unittest.main()
