from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
MEASURE = ROOT / "tools/ci/measure.sh"


class CiMeasureTests(unittest.TestCase):
    def test_failed_command_reports_cgroup_memory_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cgroup = Path(directory)
            (cgroup / "memory.events").write_text(
                "low 0\nhigh 0\nmax 3\noom 1\noom_kill 1\n", encoding="utf-8"
            )
            (cgroup / "memory.max").write_text("10737418240\n", encoding="utf-8")
            (cgroup / "memory.peak").write_text("10737418240\n", encoding="utf-8")
            result = subprocess.run(
                ["bash", str(MEASURE), "fixture", "--", "bash", "-c", "exit 7"],
                cwd=ROOT,
                env=os.environ | {"TILO_CGROUP_ROOT": str(cgroup)},
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
        self.assertEqual(result.returncode, 7, result.stdout)
        self.assertIn("cgroup_memory_max=10737418240", result.stdout)
        self.assertIn("oom_kill 1", result.stdout)

    def test_success_does_not_add_failure_diagnostics(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cgroup = Path(directory)
            (cgroup / "memory.events").write_text("oom 0\noom_kill 0\n", encoding="utf-8")
            result = subprocess.run(
                ["bash", str(MEASURE), "fixture", "--", "true"],
                cwd=ROOT,
                env=os.environ | {"TILO_CGROUP_ROOT": str(cgroup)},
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("cgroup_memory_events", result.stdout)


if __name__ == "__main__":
    unittest.main()
