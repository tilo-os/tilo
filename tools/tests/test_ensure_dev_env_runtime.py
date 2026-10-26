"""A dev image keeps the env-runtime its base ships, and fails to build with none."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/scripts/ensure-dev-env-runtime.sh"


class EnsureDevEnvRuntimeTests(unittest.TestCase):
    def run_with(self, listing: str):
        """The script with a tilo-install whose `image list` prints `listing`, and no runtime given."""
        with tempfile.TemporaryDirectory() as directory:
            install = Path(directory) / "tilo-install"
            install.write_text("#!/bin/sh\n[ \"$1 $2\" = 'image list' ] || exit 2\n"
                               f"printf '%s' '{listing}'\n")
            install.chmod(0o755)
            source = Path(directory) / "no-runtime"
            source.mkdir()
            return subprocess.run(["bash", str(SCRIPT), str(source), str(install)],
                                  text=True, capture_output=True)

    def test_a_base_that_ships_a_runtime_keeps_it(self):
        result = self.run_with("tapp:" + "a" * 64 + " env-runtime so.tilo.env-runtime 44.2.0\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("the image ships an env-runtime", result.stderr)

    def test_no_runtime_anywhere_fails_the_build(self):
        result = self.run_with("tapp:" + "b" * 64 + " tool so.tilo.git 2.0.0 on tapp:x\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ships no env-runtime", result.stderr)


if __name__ == "__main__":
    unittest.main()
