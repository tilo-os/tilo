"""The native fixture supervisor waits for only its own descendant processes."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).with_name("run-native-fixture.py")


class NativeFixtureTests(unittest.TestCase):
    def test_reaps_descendants_before_returning(self):
        with tempfile.TemporaryDirectory() as root:
            finished = Path(root) / "finished"
            command = (
                "import os,pathlib,time; "
                "child=os.fork(); "
                "os._exit(0) if child else None; "
                "time.sleep(0.05); pathlib.Path(" + repr(str(finished)) + ").write_text('flushed')"
            )
            result = subprocess.run([sys.executable, str(HELPER), sys.executable, "-c", command], check=False)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(finished.read_text(), "flushed")

    def test_preserves_fixture_failure(self):
        result = subprocess.run([sys.executable, str(HELPER), sys.executable, "-c", "raise SystemExit(7)"], check=False)
        self.assertEqual(result.returncode, 7)

    def test_fixture_writes_no_core_dumps(self):
        command = "import resource; raise SystemExit(resource.getrlimit(resource.RLIMIT_CORE) != (0, 0))"
        result = subprocess.run([sys.executable, str(HELPER), sys.executable, "-c", command], check=False)
        self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
