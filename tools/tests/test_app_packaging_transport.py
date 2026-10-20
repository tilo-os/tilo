"""Exercise bounded capture with real subprocesses, including cleanup."""
from pathlib import Path
import signal
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_product as product
from hang import HANG_BOUND


class BoundedTransport(unittest.TestCase):
    def capture(self, script, maximum=32, timeout=HANG_BOUND, killed=False):
        spawned = []
        real_popen = subprocess.Popen

        def track(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            spawned.append(process)
            return process

        try:
            with patch.object(product.subprocess, 'Popen', side_effect=track):
                return product.bounded_output([sys.executable, '-c', script], maximum, timeout)
        finally:
            self.assertEqual(len(spawned), 1)
            self.assertIsNotNone(spawned[0].poll(), 'child must be killed and reaped')
            self.assertTrue(spawned[0].stdout.closed, 'reader must close its pipe')
            if killed:
                # Stopped, not waited out: the child would sleep far longer.
                self.assertEqual(spawned[0].returncode, -signal.SIGKILL)

    def test_bounded_transport_stops_oversize_silent_or_failing_children(self):
        with self.subTest(phase='exact limit and empty success'):
            self.assertEqual(self.capture("import sys; sys.stdout.buffer.write(b'x'*32)"), b'x'*32)
            self.assertEqual(self.capture('pass', maximum=0), b'')
        with self.subTest(phase='oversized stream is stopped before producer finishes'):
            with self.assertRaisesRegex(ValueError, 'exceeds byte bound'):
                self.capture("import os,time; os.write(1,b'x'*33); time.sleep(3600)", killed=True)
        with self.subTest(phase='timeout kills and reaps silent child'):
            with self.assertRaisesRegex(TimeoutError, 'timed out'):
                self.capture('import time; time.sleep(3600)', timeout=0.5, killed=True)
        with self.subTest(phase='eof does not allow child to escape deadline'):
            with self.assertRaisesRegex(TimeoutError, 'timed out'):
                self.capture('import os,time; os.close(1); time.sleep(3600)', timeout=0.5, killed=True)
        with self.subTest(phase='failed command does not expose stderr'):
            with self.assertRaisesRegex(ValueError, '^plan API request failed$'):
                self.capture("import sys; sys.stderr.write('credential sentinel'); sys.exit(2)")


if __name__ == '__main__':
    unittest.main()
