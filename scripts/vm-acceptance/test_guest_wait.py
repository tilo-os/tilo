"""The in-guest waits return when their event happens, not after a poll interval:

    python3 -m unittest scripts/vm-acceptance/test_guest_wait.py
"""

import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import guest_wait  # noqa: E402


class LineWaitTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.log = Path(self.dir.name) / "session.log"
        self.log.write_text("one\nAPP-STARTUP app=so.x total_ms=1 (an earlier launch)\n")

    def tearDown(self):
        self.dir.cleanup()

    def append_later(self, text, delay=0.3):
        def write():
            time.sleep(delay)
            with self.log.open("a") as f:
                f.write(text)
        threading.Thread(target=write).start()

    def test_a_line_written_later_ends_the_wait_and_earlier_lines_do_not(self):
        self.append_later("noise\n\x1b[3mpackage_id\x1b[0m\x1b[2m=\x1b[0mso.x launch requested\n"
                          "first frame presented launch_id=3 \x1b[3mpackage_id\x1b[0m\x1b[2m=\x1b[0mso.x surface_id=1\n")
        started = time.monotonic()
        status = guest_wait.wait_line(str(self.log), 3, 10, [r"APP-STARTUP app=so\.x ",
                                                             r"first frame presented .*package_id=so\.x "])
        self.assertEqual(status, 0)
        self.assertLess(time.monotonic() - started, 3)

    def test_a_line_never_written_runs_out(self):
        self.assertEqual(guest_wait.wait_line(str(self.log), 3, 0.3, [r"APP-STARTUP app=so\.x "]), 1)


class ExitWaitTest(unittest.TestCase):
    def test_returns_when_the_last_process_exits(self):
        proc = subprocess.Popen(["sleep", "0.5"])
        threading.Thread(target=proc.wait).start()
        alive = lambda app: [proc.pid] if proc.poll() is None else []  # noqa: E731
        original, guest_wait.app_pids = guest_wait.app_pids, alive
        try:
            started = time.monotonic()
            self.assertEqual(guest_wait.wait_exit("so.x", 10), 0)
            self.assertLess(time.monotonic() - started, 3)
        finally:
            guest_wait.app_pids = original

    def test_an_app_with_no_process_is_gone_at_once(self):
        self.assertEqual(guest_wait.app_pids("so.tilo.no-such-app"), [])
        self.assertEqual(guest_wait.wait_exit("so.tilo.no-such-app", 0), 0)

    def test_the_cgroup_rule_matches_core(self):
        from acceptance import core
        path = "/user.slice/user-1001.slice/user@1001.service/app.slice/tilo-apps-7.scope/app-so.tilo.file\\x2dexplorer-5/main"
        self.assertEqual(guest_wait._cgroup_app(path), core.app_of_cgroup(path))
        self.assertEqual(guest_wait._cgroup_app(path), "so.tilo.file-explorer")


if __name__ == "__main__":
    unittest.main()
