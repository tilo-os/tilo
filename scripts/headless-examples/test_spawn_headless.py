"""Production script preflight checks; no compositor or application is launched."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest


class SocketPathPreflight(unittest.TestCase):
    def invoke(self, path):
        with tempfile.TemporaryDirectory(prefix="tilo-spawn-preflight-") as directory:
            root = Path(directory)
            script = root / "scripts/headless-examples/spawn-headless.sh"
            script.parent.mkdir(parents=True)
            shutil.copy2(Path(__file__).with_name("spawn-headless.sh"), script)
            # There is deliberately no target/debug/shell. A short path must
            # reach the ordinary missing-binary refusal; nothing can launch.
            return subprocess.run(
                ["bash", str(script), path], capture_output=True, text=True, timeout=5,
                env={**os.environ,"TILO_HEADLESS_BINARY":str(root/"target/debug/shell")},
            )

    def test_long_path_refuses_before_binary_or_profile(self):
        result = self.invoke("/tmp/" + "a" * 110 + "/control.sock")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unix sockets allow at most 107", result.stderr)
        self.assertNotIn("compositor not built", result.stderr)

    def test_path_limit_counts_encoded_bytes(self):
        result = self.invoke("/tmp/" + "é" * 60 + "/control.sock")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unix sockets allow at most 107", result.stderr)
        self.assertNotIn("compositor not built", result.stderr)

    def test_short_private_path_reaches_normal_binary_check(self):
        result = self.invoke("/tmp/private-runtime/control.sock")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compositor not built", result.stderr)
        self.assertNotIn("Unix sockets allow", result.stderr)

    def test_pathname_boundary_preserves_the_terminating_nul(self):
        accepted = self.invoke("/tmp/" + "a" * 102)
        rejected = self.invoke("/tmp/" + "a" * 103)
        self.assertIn("compositor not built", accepted.stderr)
        self.assertIn("108 bytes", rejected.stderr)
        self.assertNotIn("compositor not built", rejected.stderr)


if __name__ == "__main__":
    unittest.main()
