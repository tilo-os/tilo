"""Unit tests for P01 on a base image with components swapped in.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks.provenance import commit  # noqa: E402
from acceptance.core import CheckFailed, Result  # noqa: E402
from acceptance.vm import TiloVm, VmError  # noqa: E402

BASE = {"record": {"commit": "76513e59794f5686"}, "booted_digest": "sha256:1"}


def p01(provenance):
    res = Result("P01", "provenance", "t")
    commit(SimpleNamespace(facts={"provenance": provenance}), res)
    return res


def swap(*components):
    return {"ref": "fix/x", "commit": "abcdef0123456789", "components": list(components)}


class P01Test(unittest.TestCase):
    def test_base_and_swapped_components_are_stated(self):
        res = p01({**BASE, "swap": swap({"component": "shell", "slot": "acceptance", "layer": "9f"},
                                        {"component": "app:notes", "slot": "acceptance"})})
        self.assertEqual(res.detail, "base 76513e59794f + shell, app:notes swapped from fix/x (abcdef012345)")

    def test_a_failed_swap_fails(self):
        with self.assertRaisesRegex(CheckFailed, "shell: build failed"):
            p01({**BASE, "swap": swap({"component": "shell", "slot": "acceptance", "error": "build failed"})})

    def test_a_swap_the_slot_does_not_run_fails(self):
        with self.assertRaisesRegex(CheckFailed, "fs: not a layer of slot vm"):
            p01({**BASE, "swap": swap({"component": "fs", "slot": "vm", "layer": None})})

    def test_no_swap_is_the_disk_alone(self):
        self.assertEqual(p01(dict(BASE)).detail, "")


class CommandFailureTest(unittest.TestCase):
    def failure(self, stdout, stderr):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "tilo-vm.log"
            vm = TiloVm("tilo-vm", "owned", "acceptance", log)
            result = subprocess.CompletedProcess([], 1, stdout, stderr)
            with patch("acceptance.vm.subprocess.run", return_value=result):
                with self.assertRaises(VmError) as failure:
                    vm.run("--json", "swap", "app:osk")
            return failure.exception, log.read_text()

    def test_runtime_refusal_survives_build_output_on_both_streams(self):
        reason = "tilo-install: so.tilo.osk needs runtime `exact-build`, absent on this device."
        stdout = "built 59 thin tapp apps\n" + json.dumps({
            "error": reason, "hint": "inspect the receipt", "receipt": "/owned/run.json",
        }, indent=2) + "\n"
        stderr = "Compiling futures-executor v0.3.32\nFinished thin-app profile\n"
        failure, log = self.failure(stdout, stderr)
        self.assertEqual(failure.reason(), "`tilo-vm --json swap app:osk` exited 1: " + reason)
        self.assertIn(stdout.strip(), log)
        self.assertIn(stderr.strip(), log)

    def test_non_error_json_does_not_mask_the_real_structured_failure(self):
        stdout = '{"progress": "packing"}\n{broken progress\n' + json.dumps({"error": "install refused"})
        failure, _ = self.failure(stdout, "compiler progress")
        self.assertEqual(str(failure), "`tilo-vm --json swap app:osk` exited 1: install refused")

    def test_plain_text_errors_retain_existing_fallback(self):
        failure, _ = self.failure("progress", "error: linker failed")
        self.assertEqual(failure.reason(), "`tilo-vm --json swap app:osk` exited 1: error: linker failed")


if __name__ == "__main__":
    unittest.main()
