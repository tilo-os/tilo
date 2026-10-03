"""clippy-check.sh hands Cargo a compiler argument sccache can cache."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/ci/clippy-check.sh"


class ClippyCheckCacheTests(unittest.TestCase):
    def run_script(self, environment: dict[str, str]) -> str:
        with tempfile.TemporaryDirectory() as folder:
            tree = Path(folder)
            (tree / "tools/ci").mkdir(parents=True)
            shutil.copy(SCRIPT, tree / "tools/ci/clippy-check.sh")
            bin_dir = tree / "bin"
            bin_dir.mkdir()
            seen = tree / "rustc-seen"
            (bin_dir / "cargo").write_text(
                "#!/usr/bin/env bash\n"
                f'[ "$1" = clippy ] && printf "%s" "${{RUSTC-unset}}" > "{seen}"\n'
                "exit 0\n")
            (bin_dir / "git").write_text("#!/usr/bin/env bash\necho head\n")
            for tool in ("cargo", "git"):
                (bin_dir / tool).chmod(0o755)
            env = {key: value for key, value in os.environ.items() if key != "RUSTC"}
            env.update(environment, PATH=f"{bin_dir}:{env['PATH']}")
            subprocess.run(["bash", str(tree / "tools/ci/clippy-check.sh"), "-p", "example"],
                           env=env, check=True, capture_output=True)
            return seen.read_text()

    def test_cargo_names_the_compiler_as_sccache_recognises_it(self) -> None:
        # sccache caches `clippy-driver rustc ...` only for the literal `rustc`.
        self.assertEqual(self.run_script({}), "rustc")

    def test_an_explicit_compiler_is_kept(self) -> None:
        self.assertEqual(self.run_script({"RUSTC": "/opt/rust/bin/rustc"}), "/opt/rust/bin/rustc")


if __name__ == "__main__":
    unittest.main()
