"""Exercise the shim with real Cargo/rustc and a recording cache wrapper."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SHIM = Path(__file__).resolve().parents[1] / "ci" / "cargo-shim"


class CacheIncrementalPolicy(unittest.TestCase):
    def run_build(self, wrapper=None, incremental=None, missing_cargo_home=False):
        with tempfile.TemporaryDirectory(prefix="tilo-cargo-shim-") as directory:
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
                        "QCARGO_BYPASS", "CONTAINER_ID", "CODEX_THREAD_ID",
                        "CODEX_SESSION_ID", "CLAUDECODE", "QCARGO_REMOTE_ONLY",
                        "QCARGO_REMOTE_ACTIVE", "QCARGO_REMOTE_CARGO", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"):
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

    def test_incremental_policy_follows_the_selected_wrapper(self):
        for expected, options in (
            ({"0"}, {}),  # the shim's own sccache selection
            ({"0"}, {"wrapper": "sccache"}),  # an existing sccache path
            ({"unset"}, {"wrapper": "custom-wrapper"}),  # native policy for other wrappers
            ({"1"}, {"incremental": "1"}),  # an explicit choice wins
            ({"0"}, {"missing_cargo_home": True}),  # PATH fallback selects sccache
            ({"unset"}, {"wrapper": "custom-wrapper", "missing_cargo_home": True}),
        ):
            with self.subTest(**options):
                self.assertEqual(self.run_build(**options), expected)

if __name__ == "__main__":
    unittest.main()
