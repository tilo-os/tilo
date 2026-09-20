"""Exercise bounded capture with real subprocesses, including cleanup."""
from pathlib import Path
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_product as product


class BoundedTransport(unittest.TestCase):
    def capture(self, script, maximum=32, timeout=0.5):
        spawned = []
        real_popen = subprocess.Popen

        def track(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            spawned.append(process)
            return process

        started = time.monotonic()
        try:
            with patch.object(product.subprocess, 'Popen', side_effect=track):
                return product.bounded_output([sys.executable, '-c', script], maximum, timeout)
        finally:
            self.assertEqual(len(spawned), 1)
            self.assertIsNotNone(spawned[0].poll(), 'child must be killed and reaped')
            self.assertTrue(spawned[0].stdout.closed, 'reader must close its pipe')
            self.assertLess(time.monotonic() - started, 5)

    def test_exact_limit_and_empty_success(self):
        self.assertEqual(self.capture("import sys; sys.stdout.buffer.write(b'x'*32)"), b'x'*32)
        self.assertEqual(self.capture('pass', maximum=0), b'')

    def test_oversized_stream_is_stopped_before_producer_finishes(self):
        with self.assertRaisesRegex(ValueError, 'exceeds byte bound'):
            self.capture("import os,time; os.write(1,b'x'*33); time.sleep(60)")

    def test_timeout_kills_and_reaps_silent_child(self):
        with self.assertRaisesRegex(TimeoutError, 'timed out'):
            self.capture('import time; time.sleep(60)')

    def test_eof_does_not_allow_child_to_escape_deadline(self):
        with self.assertRaisesRegex(TimeoutError, 'timed out'):
            self.capture('import os,time; os.close(1); time.sleep(60)')

    def test_failed_command_does_not_expose_stderr(self):
        with self.assertRaisesRegex(ValueError, '^plan API request failed$'):
            self.capture("import sys; sys.stderr.write('credential sentinel'); sys.exit(2)")


if __name__ == '__main__':
    unittest.main()
