"""Tests of the benchmark adapters (tools/perf/adapters.py)."""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

import adapters  # noqa: E402
import compare  # noqa: E402

GATE = ROOT / "crates/system/fs/tests/performance_gate.rs"
ENV = {"commit": "a" * 40, "profile": "dev", "renderer": "none", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu", "started_at": "2026-10-09T01:15:00Z"}


def gate_log(scale: float) -> str:
    # The shape of a real --nocapture run with stdout and stderr merged.
    return "\n".join([
        "running 2 tests",
        "test collaborative_fs_release_performance_gate ... "
        f"PERF metadata-cold-open-and-1000-list: {85.9 * scale:.3f} ms (SLO <= 2000.000 ms)",
        f"PERF metadata-warm-8x-1000-list: {115.1 * scale:.3f} ms (SLO <= 4000.000 ms)",
        "ok",
        "test mount_ranged_read_under_remote_apply_performance_gate ... "
        "PERF remote deltas applied during the mount reads: 512",
        f"PERF mount-64x256k-ranged-read-under-remote-apply: {253.5 * scale:.3f} ms (SLO <= 4000.000 ms)",
        "ok",
    ])


class FsGate(unittest.TestCase):
    def test_repeated_runs_become_samples(self):
        records = adapters.fs_gate([gate_log(1.0), gate_log(2.0), gate_log(1.5)], ENV)
        by_kpi = {item.kpi: item for item in records}
        self.assertEqual(sorted(by_kpi), ["fs.metadata-cold-open-and-1000-list", "fs.metadata-warm-8x-1000-list",
                                          "fs.mount-64x256k-ranged-read-under-remote-apply"])
        cold = by_kpi["fs.metadata-cold-open-and-1000-list"]
        self.assertEqual((cold.n, cold.unit, cold.p50, cold.max), (3, "ms", 128.85, 171.8))
        self.assertEqual(cold.meta, {"runs": "3"})

    def test_a_log_without_gate_lines_fails(self):
        with self.assertRaisesRegex(ValueError, "TILO_FS_PERF_GATE"):
            adapters.fs_gate(["collaborative FS performance gate disabled"], ENV)

    def test_a_scenario_missing_from_one_run_fails(self):
        partial = "\n".join(line for line in gate_log(1.0).splitlines() if "warm-8x" not in line)
        with self.assertRaisesRegex(ValueError, "unequal"):
            adapters.fs_gate([gate_log(1.0), partial], ENV)

    def test_budgets_mirror_the_gate_ceilings(self):
        """kpi-budgets.toml enforces exactly the SLOs the Rust gate asserts."""
        source = GATE.read_text()
        slos = {}
        for match in re.finditer(r"const (\w+_SLO): Duration = Duration::from_(secs|millis)\((\d+)\);", source):
            name, unit, value = match.groups()
            slos[name] = float(value) * (1000.0 if unit == "secs" else 1.0)
        reports = dict(re.findall(r'report\(\s*"([a-z0-9-]+)",\s*[^,]+,\s*(\w+_SLO),?\s*\)', source))
        self.assertEqual(len(reports), 6, reports)
        budgets = compare.load_budgets(compare.BUDGETS)
        expected = {f"fs.{name}": slos[constant] for name, constant in reports.items()}
        got = {kpi: entry["max"] for kpi, entry in budgets.kpis.items() if kpi.startswith("fs.")}
        self.assertEqual(got, expected)
        for kpi, constant in ((f"fs.{name}", constant) for name, constant in reports.items()):
            self.assertTrue(budgets.kpis[kpi]["enforce"], kpi)
            self.assertEqual(budgets.kpis[kpi]["source"], constant, kpi)


class TappSizes(unittest.TestCase):
    def test_newest_version_per_app_and_architecture(self):
        def version(arch, size, released, platform="tilo-os"):
            return {"tapp_size_bytes": size, "released_at_unix": released,
                    "target": {"platform": platform, "architecture": arch}}

        entries = {"sequence": 753, "entries": [
            {"package_id": "so.tilo.a", "versions": [version("x86_64", 9_000_000, 1), version("x86_64", 2_000_000, 2),
                                                      version("aarch64", 1_000_000, 2)]},
            {"package_id": "so.tilo.b", "versions": [version("x86_64", 4_000_000, 5),
                                                      version("x86_64", 1, 9, platform="other")]},
        ]}
        records = {item.kpi: item for item in adapters.tapp_sizes(entries, {**ENV, "profile": "release"})}
        x86 = records["footprint.tapp.bytes.x86_64"]
        self.assertEqual((x86.n, x86.p50, x86.max, x86.unit), (2, 2_000_000, 4_000_000, "bytes"))
        self.assertEqual(x86.meta["registry_sequence"], "753")
        self.assertEqual(records["footprint.tapp.bytes.aarch64"].n, 1)

    def test_an_empty_index_fails(self):
        with self.assertRaises(ValueError):
            adapters.tapp_sizes({"entries": []}, ENV)


if __name__ == "__main__":
    unittest.main()
