from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
MEASURE = ROOT / "tools/ci/measure.sh"


class CiMeasureTests(unittest.TestCase):
    def test_measured_commands_report_peak_memory_and_failure_evidence(self):
        with self.subTest(phase='failed command reports cgroup memory evidence'):
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
        with self.subTest(phase='success does not add failure diagnostics'):
            with tempfile.TemporaryDirectory() as directory:
                cgroup = Path(directory)
                (cgroup / "memory.events").write_text("oom 0\noom_kill 0\n", encoding="utf-8")
                (cgroup / "memory.peak").write_text("4294967296\n", encoding="utf-8")
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
            # The peak is the headroom evidence parallelism is tuned against.
            self.assertIn("cgroup_memory_peak=4294967296", result.stdout)


    def test_inner_cpu_envelope_and_counters_bracket_the_command(self):
        with tempfile.TemporaryDirectory() as directory:
            cgroup = Path(directory)
            (cgroup / "cpu.max").write_text("600000 100000\n")
            (cgroup / "cpuset.cpus.effective").write_text("0-39\n")
            (cgroup / "cpu.stat").write_text("usage_usec 100\nnr_throttled 1\n")
            result = subprocess.run(
                ["bash", str(MEASURE), "fixture", "--", "bash", "-c",
                 'printf "usage_usec 200\\nnr_throttled 3\\n" > "$1/cpu.stat"', "--", str(cgroup)],
                cwd=ROOT, env=os.environ | {"TILO_CGROUP_ROOT": str(cgroup)},
                text=True, capture_output=True, check=False,
            )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cgroup_cpu_max=600000 100000", result.stdout)
        self.assertIn("cgroup_cpuset_cpus_effective=0-39", result.stdout)
        self.assertIn("cgroup_cpu_stat_before=usage_usec 100 nr_throttled 1", result.stdout)
        self.assertIn("cgroup_cpu_stat_after=usage_usec 200 nr_throttled 3", result.stdout)

    def test_cpu_read_failures_preserve_measured_failure_status(self):
        with tempfile.TemporaryDirectory() as directory:
            cgroup = Path(directory)
            # Directories are readable but cannot be consumed as metric files.
            (cgroup / "cpu.max").mkdir()
            (cgroup / "cpu.stat").write_text("nr_throttled 0\n")
            result = subprocess.run(
                ["bash", str(MEASURE), "fixture", "--", "bash", "-c",
                 'rm "$1/cpu.stat"; exit 7', "--", str(cgroup)],
                cwd=ROOT, env=os.environ | {"TILO_CGROUP_ROOT": str(cgroup)},
                text=True, capture_output=True, check=False,
            )
        self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
        self.assertIn("cgroup_cpu_max=unavailable", result.stdout)
        self.assertIn("cgroup_cpuset_cpus_effective=unavailable", result.stdout)
        self.assertIn("cgroup_cpu_stat_before=nr_throttled 0", result.stdout)
        self.assertIn("cgroup_cpu_stat_after=unavailable", result.stdout)
        self.assertNotIn("No such file", result.stderr)

if __name__ == "__main__":
    unittest.main()
