"""Unit tests for P01 on a base image with components swapped in.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks.provenance import commit  # noqa: E402
from acceptance.core import CheckFailed, Result  # noqa: E402

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


if __name__ == "__main__":
    unittest.main()
