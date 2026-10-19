"""clippy-check.sh hands Cargo a compiler argument sccache can cache."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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
            # The script picks the package's features through this helper.
            shutil.copy(ROOT / "tools/ci/package_features.py", tree / "tools/ci/package_features.py")
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

    def test_independent_package_failures_are_reported_without_retrying(self) -> None:
        with tempfile.TemporaryDirectory() as folder:
            tree = Path(folder)
            (tree / "tools/ci").mkdir(parents=True)
            shutil.copy(SCRIPT, tree / "tools/ci/clippy-check.sh")
            # The script picks the package's features through this helper.
            shutil.copy(ROOT / "tools/ci/package_features.py", tree / "tools/ci/package_features.py")
            shutil.copy(ROOT / "tools/ci/compiler_inputs.py", tree / "tools/ci/compiler_inputs.py")
            bin_dir = tree / "bin"
            bin_dir.mkdir()
            (bin_dir / "cargo").write_text(
                f"#!{sys.executable}\n"
                "import json, sys\nfrom pathlib import Path\n"
                "if sys.argv[1] == 'metadata':\n"
                "    print(json.dumps({'workspace_members': [], 'packages': []}))\n"
                "    sys.exit(0)\n"
                "calls = Path(__file__).parent.parent / 'clippy-calls'\n"
                "calls.write_text(calls.read_text() + 'clippy\\n' if calls.exists() else 'clippy\\n')\n"
                "packages = ['first'] + (['second'] if '--keep-going' in sys.argv else [])\n"
                "for package in packages:\n"
                "    print(json.dumps({'reason': 'compiler-message', 'message': "
                "{'level': 'error', 'rendered': package + ' package lint failure'}}))\n"
                "print(json.dumps({'reason': 'build-finished', 'success': False}))\n"
                "sys.exit(101)\n")
            (bin_dir / "git").write_text(
                "#!/usr/bin/env bash\n"
                "case \"$1\" in rev-parse) echo head ;; status|ls-files) : ;; esac\n")
            for tool in ("cargo", "git"):
                (bin_dir / tool).chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
            result = subprocess.run(
                ["bash", str(tree / "tools/ci/clippy-check.sh"), "--workspace"],
                env=env, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1, result.stderr)
            for package in ("first", "second"):
                self.assertEqual(result.stderr.count(package + " package lint failure"), 1)
            self.assertEqual((tree / "clippy-calls").read_text(), "clippy\n")
            receipt = json.loads((tree / "target/ci/clippy-head-inputs.json").read_text())
            self.assertEqual(receipt["cargo_exit_code"], 101)

    def test_a_package_tested_with_extra_features_is_also_checked_as_it_ships(self) -> None:
        # The Imager's tests and lints cover the installation; its default
        # build, the Tilo app, is checked on its own too, tests included.
        with tempfile.TemporaryDirectory() as folder:
            tree = Path(folder)
            (tree / "tools/ci").mkdir(parents=True)
            shutil.copy(SCRIPT, tree / "tools/ci/clippy-check.sh")
            shutil.copy(ROOT / "tools/ci/package_features.py", tree / "tools/ci/package_features.py")
            bin_dir = tree / "bin"
            bin_dir.mkdir()
            calls = tree / "clippy-calls"
            (bin_dir / "cargo").write_text(
                "#!/usr/bin/env bash\n"
                f'[ "$1" = clippy ] && echo "$*" >> "{calls}"\n'
                "exit 0\n")
            (bin_dir / "git").write_text("#!/usr/bin/env bash\necho head\n")
            for tool in ("cargo", "git"):
                (bin_dir / tool).chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
            subprocess.run(["bash", str(tree / "tools/ci/clippy-check.sh"), "-p", "imager"],
                           env=env, check=True, capture_output=True)
            runs = calls.read_text().splitlines()
            self.assertEqual(len(runs), 2, runs)
            self.assertIn("--features imager/installation", runs[0])
            self.assertIn("--all-targets", runs[1])
            self.assertIn("-p imager", runs[1])
            self.assertNotIn("--features", runs[1])


if __name__ == "__main__":
    unittest.main()
