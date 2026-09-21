"""Exercise the shim with real Cargo/rustc and a recording cache wrapper."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SHIM = Path(__file__).resolve().parents[1] / "ci" / "cargo-shim"


class CacheIncrementalPolicy(unittest.TestCase):
    def run_build(self, wrapper=None, incremental=None, missing_cargo_home=False):
        with tempfile.TemporaryDirectory(prefix="qos-cargo-shim-") as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("pub fn answer() -> u32 { 42 }\n")
            (root / "Cargo.toml").write_text(
                '[package]\nname = "shim-probe"\nversion = "0.1.0"\nedition = "2024"\n'
            )
            record = root / "wrapper-env"
            cache = root / "sccache"
            cache.write_text(
                '#!/bin/bash\n'
                'printf "%s\\n" "${CARGO_INCREMENTAL-unset}" >> "$SHIM_TEST_RECORD"\n'
                'exec "$@"\n'
            )
            cache.chmod(0o755)
            custom = root / "custom-wrapper"
            custom.symlink_to(cache)
            env = dict(os.environ)
            for key in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_INCREMENTAL",
                        "QCARGO_BYPASS", "CONTAINER_ID", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"):
                env.pop(key, None)
            env.update(PATH=str(root) + os.pathsep + env["PATH"],
                       CARGO_TARGET_DIR=str(root / "target"),
                       SHIM_TEST_RECORD=str(record))
            if missing_cargo_home:
                env["CARGO_HOME"] = str(root / "empty-cargo-home")
            if wrapper:
                env["RUSTC_WRAPPER"] = str(root / wrapper)
            if incremental is not None:
                env["CARGO_INCREMENTAL"] = incremental
            result = subprocess.run(
                ["bash", str(SHIM), "build", "--offline"], cwd=root, env=env,
                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            )
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertTrue(record.exists(), "Cargo must actually invoke the wrapper")
            return set(record.read_text().splitlines())

    def test_selected_sccache_disables_incremental(self):
        self.assertEqual(self.run_build(), {"0"})

    def test_existing_sccache_path_disables_incremental(self):
        self.assertEqual(self.run_build(wrapper="sccache"), {"0"})

    def test_custom_wrapper_preserves_native_incremental_policy(self):
        self.assertEqual(self.run_build(wrapper="custom-wrapper"), {"unset"})

    def test_explicit_incremental_choice_is_preserved(self):
        self.assertEqual(self.run_build(incremental="1"), {"1"})

    def test_path_fallback_applies_selected_cache_policy(self):
        self.assertEqual(self.run_build(missing_cargo_home=True), {"0"})

    def test_path_fallback_preserves_custom_wrapper_policy(self):
        self.assertEqual(
            self.run_build(wrapper="custom-wrapper", missing_cargo_home=True), {"unset"}
        )


if __name__ == "__main__":
    unittest.main()
