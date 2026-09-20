import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from watch_receipt import acquire


class WatchReceiptTests(unittest.TestCase):
    def claim(self, path):
        return acquire(path, cwd=path.parent, session_id="task", sha="commit")

    def test_real_process_crash_recovery_preserves_discovery_and_excludes_owner(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            code = """import sys
from pathlib import Path
from watch_receipt import acquire
p=Path(sys.argv[1])
r=acquire(p,cwd=p.parent,session_id='task',sha='commit')
print(r.started.isoformat(),flush=True)
sys.stdin.readline()
"""
            child = subprocess.Popen([sys.executable, "-c", code, str(path)],
                                     cwd=Path(__file__).parent, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, text=True)
            try:
                started = child.stdout.readline().strip()
                self.assertTrue(started)
                self.assertIsNone(child.poll())
                self.assertIsNone(self.claim(path))
                child.terminate()
                child.wait(timeout=10)
                with self.claim(path) as recovered:
                    self.assertEqual(recovered.started.isoformat(), started)
                    self.assertEqual(recovered.data["attempt"], 2)
                    self.assertIsNone(self.claim(path))
                    recovered.save(state="reported", message="done")
                self.assertIsNone(self.claim(path))
            finally:
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=10)
                child.stdin.close()
                child.stdout.close()

    def test_recovery_is_bounded_and_ambiguous_delivery_is_not_repeated(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bounded.json"
            for attempt in range(1, 4):
                with self.claim(path) as receipt:
                    self.assertEqual(receipt.data["attempt"], attempt)
            self.assertIsNone(self.claim(path))
            path = Path(directory) / "delivery.json"
            with self.claim(path) as receipt:
                receipt.save(state="delivering")
            self.assertIsNone(self.claim(path))

    def test_corrupt_receipt_does_not_guess_that_delivery_never_happened(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            path.write_text("partial write")
            self.assertIsNone(self.claim(path))


if __name__ == "__main__":
    unittest.main()
