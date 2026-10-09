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
        self.assertEqual(len(reports), 7, reports)
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

    def test_only_the_population_counts_and_the_rest_is_named(self):
        def version(size):
            return {"tapp_size_bytes": size, "released_at_unix": 1,
                    "target": {"platform": "tilo-os", "architecture": "x86_64"}}

        entries = {"sequence": 805, "entries": [
            {"package_id": "so.tilo.runtime", "versions": [version(11_776)]},
            {"package_id": "so.tilo.a", "versions": [version(2_000_000)]},
            {"package_id": "so.tilo.b", "versions": [version(3_000_000)]},
        ]}
        (x86,) = adapters.tapp_sizes(entries, ENV, {"so.tilo.a", "so.tilo.b"})
        self.assertEqual((x86.n, x86.p50, x86.max), (2, 2_000_000, 3_000_000))
        self.assertEqual(x86.meta["not_counted"], "so.tilo.runtime")
        with self.assertRaises(ValueError):
            adapters.tapp_sizes(entries, ENV, {"so.tilo.gone"})

    def test_an_empty_index_fails(self):
        with self.assertRaises(ValueError):
            adapters.tapp_sizes({"entries": []}, ENV)


SWITCH_SOURCE = ROOT / "crates/session/shell/src/compositor/switch_latency.rs"


def switch_line(first: int, settled: int, dropped: int = 0, interval: int = 16_667) -> str:
    return (f"WORKSPACE-SWITCH first_queued_us={first - 1000} first_shown_us={first} "
            f"settled_queued_us={settled - 1000} settled_shown_us={settled} frames=25 dropped={dropped} "
            f"interval_us={interval}")


class WorkspaceSwitch(unittest.TestCase):
    def test_each_switch_line_is_one_sample(self):
        log = "\n".join(["tilo compositor listening on \"wayland-1\"", switch_line(9_000, 412_000),
                         "KEY-LATENCY app=x key_to_commit_us=1 key_to_submit_us=2 key_to_present_us=3",
                         "2026-10-09T01:20:00Z " + switch_line(17_000, 430_000, dropped=2)])
        samples = adapters.switch_samples(log)
        self.assertEqual([sample["first_shown_us"] for sample in samples], [9_000, 17_000])
        records = {item.kpi: item for item in adapters.workspace_switch(samples, ENV)}
        self.assertEqual(sorted(records), ["latency.workspace_switch.dropped_frames",
                                           "latency.workspace_switch.first_frame",
                                           "latency.workspace_switch.settled"])
        first = records["latency.workspace_switch.first_frame"]
        self.assertEqual((first.unit, first.n, first.p50, first.max), ("ms", 2, 9.0, 17.0))
        self.assertEqual(records["latency.workspace_switch.settled"].max, 430.0)
        dropped = records["latency.workspace_switch.dropped_frames"]
        self.assertEqual((dropped.unit, dropped.max), ("count", 2.0))
        self.assertEqual(first.meta, {"interval_us": "16667", "frames_min": "25"})

    def test_a_named_variant(self):
        samples = adapters.switch_samples(switch_line(5_000, 405_000, interval=6_944))
        kpis = [item.kpi for item in adapters.workspace_switch(samples, ENV, "hw.workspace_switch.{}.144hz")]
        self.assertIn("hw.workspace_switch.first_frame.144hz", kpis)

    def test_no_switch_and_a_truncated_line_fail(self):
        with self.assertRaisesRegex(ValueError, "WORKSPACE-SWITCH"):
            adapters.workspace_switch(adapters.switch_samples("no switches here"), ENV)
        with self.assertRaisesRegex(ValueError, "lacks"):
            adapters.switch_samples("WORKSPACE-SWITCH first_queued_us=1 frames=3")

    def test_the_fields_are_the_compositors(self):
        """The parser reads the fields the Rust `Display` writes, in its order."""
        source = SWITCH_SOURCE.read_text()
        written = re.findall(r"\b([a-z_]+)=\{\}", source[source.index('"WORKSPACE-SWITCH'):])
        self.assertEqual(tuple(written), adapters.SWITCH_FIELDS)

    def test_every_nightly_latency_kpi_has_a_budget(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        nightly = {kpi for kpi in budgets.kpis if kpi.startswith("latency.")}
        produced = {item.kpi for item in adapters.workspace_switch(
            adapters.switch_samples(switch_line(5_000, 405_000)), ENV)} | {"latency.input_to_frame.pointer"}
        self.assertEqual(nightly, produced)


IDLE_GATE = ROOT / "crates/session/shell/tests/idle_wakeups.rs"


def idle_log(total: float, shell: float) -> str:
    # The shape of a --nocapture run: the table on stderr, the line on stdout.
    return "\n".join([
        "running 1 test",
        "idle wakeups over 10.0 s: 3.40/s across 1 processes",
        f"IDLE-WAKEUPS total_per_s={total:.3f} shell_per_s={shell:.3f} loop_ticks_per_s=0.000 "
        "cpu_percent=0.400 window_ms=10012 threads_started=0",
        "test idle_wakeups::an_idle_desktop_with_two_apps_stays_within_its_wakeup_budget ... ok",
    ])


class IdleWakeups(unittest.TestCase):
    def test_each_run_is_one_sample(self):
        records = {item.kpi: item for item in adapters.idle_wakeups(
            [idle_log(3.4, 2.1), idle_log(4.0, 2.5), idle_log(3.6, 2.2)], ENV)}
        total = records["wakeups.headless_two_apps"]
        self.assertEqual((total.unit, total.n, total.p50, total.max), ("per_s", 3, 3.6, 4.0))
        self.assertEqual(records["wakeups.headless_two_apps.shell"].max, 2.5)
        self.assertEqual(records["wakeups.frame_loop_ticks.headless_two_apps"].max, 0.0)
        self.assertEqual(total.meta, {"runs": "3", "window_ms": "10012"})

    def test_a_run_without_its_line_fails(self):
        with self.assertRaisesRegex(ValueError, "IDLE-WAKEUPS"):
            adapters.idle_wakeups([idle_log(3.4, 2.1), "running 1 test\ntest ... FAILED"], ENV)

    def test_the_fields_are_the_gates(self):
        """The gate prints every field the adapter reads."""
        source = IDLE_GATE.read_text()
        printed = set(re.findall(r"\b([a-z_]+)=\{", source[source.index('"IDLE-WAKEUPS'):]))
        self.assertLessEqual({*adapters.IDLE_WAKEUPS_KPIS, "window_ms"}, printed)

    def test_every_idle_kpi_has_a_budget(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        nightly = {kpi for kpi in budgets.kpis if kpi.startswith("wakeups.")}
        self.assertEqual(nightly, set(adapters.IDLE_WAKEUPS_KPIS.values()))


if __name__ == "__main__":
    unittest.main()
