#!/usr/bin/env python3
"""tools/ci/perf-room-degraded.sh: the degradation it sets, and its scope.

The probe reads join and chat latencies of a host on the same node as the
guests, so two things about the script decide whether the measurement means
anything: the degradation must reach the host (a run that degrades nothing
would pass), and it must reach nothing else (it would slow a shared node's
other jobs and read its own damage).

Both are checked by running it with stand-ins for the tools it calls.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "ci" / "perf-room-degraded.sh"


class ScriptTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "calls.log"
        # Stand-ins for every tool: they record how they were called and touch
        # nothing this machine's scheduling depends on. `unshare` matters most:
        # it records the namespaces, then runs the script again as the
        # command line it was given, as the real one does.
        for name, body in (
            ("chrt", '#!/bin/sh\nprintf "chrt %s\\n" "$*" >> "$LOG"\n'),
            ("tc", '#!/bin/sh\nprintf "tc %s\\n" "$*" >> "$LOG"\n'),
            ("sleep", '#!/bin/sh\nexit 0\n'),
            ("unshare", '#!/bin/sh\nprintf "unshare %s\\n" "$*" >> "$LOG"\n'
                        'while [ $# -gt 0 ] && [ "$1" != -- ]; do shift; done\n'
                        'shift; exec "$@"\n'),
        ):
            tool = self.bin / name
            tool.write_text(body.replace("$LOG", str(self.log)), encoding="utf-8")
            tool.chmod(0o755)
        bash = Path(os.environ.get("BASH", "/bin/bash"))
        (self.bin / "bash").symlink_to(bash)
        # The command the script runs is a real shell, so a test's own `sh -c` runs:
        # the tools under test are only ever the stand-ins above.
        (self.bin / "sh").symlink_to(bash)

    def run_script(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(["bash", str(SCRIPT), *arguments], capture_output=True, text=True, timeout=60,
                              check=False, env={**os.environ, "PATH": str(self.bin)})

    def test_the_command_runs_inside_namespaces_of_its_own(self):
        commands = self.root / "ran"
        result = self.run_script("sh", "-c", f'printf "%s\\n" ran >> {commands}')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(commands.read_text(encoding="utf-8"), "ran\n")
        calls = self.log.read_text(encoding="utf-8")
        unshare = next(line for line in calls.splitlines() if line.startswith("unshare "))
        for namespace in ("--user", "--mount", "--pid", "--net", "--fork"):
            self.assertIn(namespace, unshare)
        # A job holds no capability, so the id map to root would fail: an unprivileged
        # user sets its own batch class and shapes its own namespace's link.
        self.assertNotIn("--map-root-user", unshare)
        # Its own namespaces, not the machine's: nothing else is throttled or shaped.
        self.assertIn("tc qdisc replace dev lo root", calls)
        self.assertIn("tc qdisc del dev lo root", calls, "the shaping is undone after the run")

    def test_every_process_inside_is_throttled(self):
        result = self.run_script("true")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text(encoding="utf-8")
        throttled = [line for line in calls.splitlines() if line.startswith("chrt ")]
        self.assertTrue(throttled, "no process was put in the batch class")
        self.assertTrue(all(line.startswith("chrt -b 0 -p ") for line in throttled), throttled)

    def test_a_machine_that_cannot_be_degraded_fails_instead_of_measuring(self):
        (self.bin / "chrt").unlink()
        result = self.run_script("true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("degradation cannot be applied", result.stderr)
        # The shaping was still undone: a half-degraded machine is no better.
        self.assertIn("tc qdisc del dev lo root", self.log.read_text(encoding="utf-8"))

    def test_no_command_is_a_usage_error(self):
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("usage", result.stderr)


if __name__ == "__main__":
    unittest.main()
