from pathlib import Path
import os
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).parents[1] / "ci/check-unused-dependencies.sh"


class UnusedDependencyTests(unittest.TestCase):
    def test_manifest_scope_keeps_standalone_and_nested_first_party(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", directory], check=True)
            paths = ["Cargo.toml", "apps/a/Cargo.toml", "tools/space name/Cargo.toml",
                     "crates/a/vendor/nested/Cargo.toml", "vendor/codec/Cargo.toml"]
            for name in paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("[package]\nname = 'fixture'\nversion = '0.1.0'\n")
            # Include tracked and untracked first-party manifests for local use.
            subprocess.run(["git", "add", "Cargo.toml"], cwd=root, check=True)
            binary = root / "bin"
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENTS"\nexit 1\n')
            cargo.chmod(0o755)
            output = root / "arguments"
            env = dict(os.environ, TILO_REPO_ROOT=str(root), ARGUMENTS=str(output),
                       PATH=str(binary) + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(SCRIPT)], env=env)
            self.assertEqual(result.returncode, 1, "unused-dependency failures must propagate")
            args = output.read_text().splitlines()
            self.assertEqual(args[:2], ["machete", "--with-metadata"])
            self.assertCountEqual(args[2:], ["Cargo.toml", "apps", "tools", "crates"])


if __name__ == "__main__":
    unittest.main()
