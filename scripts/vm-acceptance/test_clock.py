"""Unit tests for K01's judgement of fs, without a VM.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.core import CheckFailed, Result  # noqa: E402
from acceptance.checks import clock  # noqa: E402

NOW = 1_800_000_000


class FakeVm:
    """A VM whose clock K01 moves, with or without the device certificate fs serves."""

    def __init__(self, device_cert: bool, fs_state: str):
        self.device_cert, self.fs_state = device_cert, fs_state
        self.clock = NOW
        self.behind = True

    def root_exec(self, script, timeout=120):
        if script.startswith("date +%s"):
            return f"{self.clock}\n"
        if script.startswith("date -s @"):
            self.clock = int(script.split("@", 1)[1].split()[0])
            self.behind = self.clock < NOW
            return ""
        if script.startswith("test -s"):
            return "yes\n" if self.device_cert else "no\n"
        if script.startswith("systemctl show"):
            return f"ActiveState={self.fs_state}\nSubState=x\nNRestarts=0\nExecMainStartTimestampMonotonic=0\n"
        if script.startswith("journalctl"):
            return "1\n"
        return ""

    def restart_session(self, timeout=60):
        pass

    def state(self):
        return {"chrome": {"input_capture": {"onboarding": False},
                           "identity": {"recovery": False, "waits_for_clock": self.behind},
                           "collab": {"available": not self.behind}}}


def ctx_for(vm):
    def wait_state(predicate, timeout=8.0, what="state"):
        st = vm.state()
        if not predicate(st):
            raise CheckFailed(f"{what} not reached")
        return st
    return SimpleNamespace(vm=vm, settle=lambda *a: None, wait_state=wait_state)


class FsHalf(unittest.TestCase):
    def test_a_slot_without_a_device_certificate_says_the_fs_part_was_skipped(self):
        res = Result("K01", "basics", "K01")
        clock.clock_behind_the_certificate(ctx_for(FakeVm(device_cert=False, fs_state="inactive")), res)
        self.assertIn("fs part skipped", res.detail)
        self.assertIn(clock.DEVICE_CERT, res.detail)
        self.assertNotIn("fs_while_behind", res.metrics)

    def test_with_a_device_certificate_a_stopped_fs_still_fails(self):
        res = Result("K01", "basics", "K01")
        with self.assertRaises(CheckFailed) as failed:
            clock.clock_behind_the_certificate(ctx_for(FakeVm(device_cert=True, fs_state="inactive")), res)
        self.assertIn("fs is inactive", str(failed.exception))
        self.assertNotIn("skipped", res.detail)


if __name__ == "__main__":
    unittest.main()
