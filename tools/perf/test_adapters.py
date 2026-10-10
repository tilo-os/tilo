"""Tests of the benchmark adapters (tools/perf/adapters.py)."""

from __future__ import annotations

import re
import sys
import tomllib
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
        self.assertEqual(len(reports), 11, reports)
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


class RuntimeSizes(unittest.TestCase):
    MEASURED = {"sequence": 809, "runtimes": {
        "x86_64": {"version": "0.1.9", "runtime": "tilo-runtime/x", "bytes": 241_944_696,
                   "relocation_bytes": 10_115_280, "relocated_bytes": 7_663_616, "symbol_relocations": 62_255},
        "aarch64": {"version": "0.1.9", "runtime": "tilo-runtime/a", "bytes": 223_411_008,
                    "relocation_bytes": 10_089_504, "relocated_bytes": 7_643_136, "symbol_relocations": 62_286}}}

    def test_each_metric_of_each_architecture_is_one_record(self):
        records = {item.kpi: item for item in adapters.runtime_sizes(self.MEASURED, ENV)}
        self.assertEqual(len(records), 8)
        size = records["footprint.runtime.bytes.x86_64"]
        self.assertEqual((size.n, size.p50, size.unit), (1, 241_944_696, "bytes"))
        self.assertEqual(size.meta["registry_sequence"], "809")
        self.assertEqual(size.meta["runtime"], "tilo-runtime/x")
        self.assertEqual(records["footprint.runtime.symbol_relocations.aarch64"].unit, "count")

    def test_every_runtime_kpi_has_a_budget(self):
        budgets = compare.load_budgets(HERE / "kpi-budgets.toml")
        for item in adapters.runtime_sizes(self.MEASURED, ENV):
            self.assertIn(item.kpi, budgets.kpis)
            self.assertEqual(budgets.kpis[item.kpi]["unit"], item.unit)

    def test_nothing_measured_and_a_missing_metric_fail(self):
        with self.assertRaises(ValueError):
            adapters.runtime_sizes({"runtimes": {}}, ENV)
        partial = {"runtimes": {"x86_64": {"bytes": 1}}}
        with self.assertRaises(KeyError):
            adapters.runtime_sizes(partial, ENV)


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


SCALING = ROOT / "crates/sdk/tilo-app/examples/framework-scaling.rs"


def scaling_log(scale: float) -> str:
    """framework-scaling's `open` output: the table, then one line per size."""
    return "\n".join([
        "tasks  open to first frame p50  allocations  heap held  heap per task",
        f" 5000  {130 * scale:>20.3} ms      1707519   24677386         4698 B",
        "",
        f"FRAMEWORK-OPEN tasks=5000 samples_ms={130 * scale:.3f},{140 * scale:.3f},{120 * scale:.3f} "
        "allocations=1707519 heap_per_task=4698",
        f"FRAMEWORK-OPEN tasks=50000 samples_ms={1300 * scale:.3f},{1400 * scale:.3f} "
        "allocations=16556849 heap_per_task=4454",
    ])


class FrameworkOpen(unittest.TestCase):
    def test_every_open_is_a_sample_and_counts_are_per_run(self):
        records = {item.kpi: item for item in adapters.framework_open([scaling_log(1.0), scaling_log(1.1)], ENV)}
        opening = records["render.framework_open.ms.50k"]
        self.assertEqual((opening.unit, opening.n, opening.max), ("ms", 4, 1540.0))
        allocations = records["render.framework_open.allocations.5k"]
        self.assertEqual((allocations.unit, allocations.n, allocations.p50), ("count", 2, 1707519.0))
        self.assertEqual(records["render.framework_open.heap_per_item.50k"].unit, "bytes")
        self.assertEqual(records["render.framework_open.ms.5k"].meta, {"runs": "2"})

    def test_a_run_without_its_lines_fails(self):
        with self.assertRaisesRegex(ValueError, "FRAMEWORK-OPEN"):
            adapters.framework_open([scaling_log(1.0), "tasks  open to first frame p50"], ENV)

    def test_the_fields_are_the_examples(self):
        """The example prints the line the adapter reads."""
        source = SCALING.read_text()
        printed = source[source.index('"FRAMEWORK-OPEN'):]
        for field in ("tasks", "samples_ms", "allocations", "heap_per_task"):
            self.assertIn(f"{field}={{", printed)

    def test_every_nightly_size_has_a_budget(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("render.framework_open.")}
        produced = {item.kpi for item in adapters.framework_open([scaling_log(1.0)], ENV)}
        self.assertEqual(budgeted, produced)


def scroll_log(pixels: int, differing: int = 0) -> str:
    """framework-scaling's `scroll` output: the table, then one line per run."""
    return "\n".join([
        "tasks  step  copies  frames  whole  repainted px p50  max  copied px p50",
        "",
        "FRAMEWORK-SCROLL tasks=5000 step=40 copying=false pixels_p50=1024000 pixels_max=1024000 "
        "copied_p50=0 whole=120 frames=120 cpu_ms=0.436 gpu_ms=1.647 gpu_full_ms=1.642 differing=0",
        f"FRAMEWORK-SCROLL tasks=5000 step=40 copying=true pixels_p50={pixels} pixels_max={pixels * 2} "
        f"copied_p50=700000 whole=0 frames=120 cpu_ms=0.512 gpu_ms=0.9 gpu_full_ms=1.6 differing={differing}",
        "FRAMEWORK-SCROLL tasks=5000 step=40.5 copying=true pixels_p50=900000 pixels_max=900000 "
        "copied_p50=0 whole=0 frames=120 cpu_ms=0.420 gpu_ms=1.6 gpu_full_ms=1.6 differing=0",
    ])


class FrameworkScroll(unittest.TestCase):
    def test_the_copying_whole_pixel_run_is_recorded(self):
        records = {item.kpi: item for item in adapters.framework_scroll([scroll_log(90000), scroll_log(110000)],
                                                                        ENV)}
        pixels = records["render.framework_scroll.pixels.5k"]
        self.assertEqual((pixels.unit, pixels.n, pixels.max), ("pixels", 2, 110000.0))
        self.assertEqual(records["render.framework_scroll.whole_frames.5k"].p50, 0.0)
        self.assertEqual(records["render.framework_scroll.cpu_ms.5k"].unit, "ms")
        self.assertEqual(records["render.framework_scroll.raster_ms.5k"].p50, 0.9)

    def test_pixels_a_full_render_does_not_have_fail(self):
        with self.assertRaisesRegex(ValueError, "full render"):
            adapters.framework_scroll([scroll_log(90000, differing=3)], ENV)

    def test_a_run_without_its_lines_fails(self):
        with self.assertRaisesRegex(ValueError, "FRAMEWORK-SCROLL"):
            adapters.framework_scroll(["tasks  step  copies"], ENV)

    def test_the_fields_are_the_examples(self):
        """The example prints the line the adapter reads."""
        source = SCALING.read_text()
        printed = source[source.index('"FRAMEWORK-SCROLL'):]
        for field in ("tasks", "step", "copying", "pixels_p50", "pixels_max", "copied_p50", "whole",
                      "frames", "cpu_ms", "gpu_ms", "gpu_full_ms", "differing"):
            self.assertIn(f"{field}={{", printed)

    def test_every_nightly_scroll_kpi_has_a_budget(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("render.framework_scroll.")}
        produced = {item.kpi for item in adapters.framework_scroll([scroll_log(90000)], ENV)}
        self.assertEqual(budgeted, produced)


LOAD_BENCH = ROOT / "crates/hosting/tilo-relay/examples/relay_load/main.rs"


def load_report(collaborators: int = 100, minutes: int = 2) -> dict:
    """A `hosting-load.json` as the bench writes it."""
    per_minute = {"hosting.relay.cpu_s_per_collab_hour": ("s", 36.0),
                  "hosting.relay.rss_mib_per_collab": ("MiB", 1.0),
                  "hosting.relay.egress_bytes_per_collab_hour": ("bytes", 10e6),
                  "hosting.relay.frame_bytes_per_collab_hour": ("bytes", 8e6)}
    kpis = {kpi: {"unit": unit, "samples": [value] * minutes} for kpi, (unit, value) in per_minute.items()}
    kpis["hosting.relay.fanout_ms"] = {"unit": "ms", "samples": [4.0, 6.0, 30.0]}
    kpis["hosting.relay.presence_pointer_ms"] = {"unit": "ms", "samples": [20.0, 40.0, 55.0]}
    kpis["hosting.relay.presence_caret_ms"] = {"unit": "ms", "samples": [6.0, 8.0, 20.0]}
    kpis["hosting.relay.store_fsyncs_per_edit"] = {"unit": "count", "samples": [2.1] * minutes}
    kpis["hosting.relay.store_listed_entries_per_edit"] = {"unit": "count", "samples": [0.0] * minutes}
    kpis["hosting.relay.compaction_cpu_ms_per_collab_hour"] = {"unit": "ms", "samples": [90.0] * minutes}
    kpis["hosting.relay.compaction_signature_checks_per_edit"] = {"unit": "count", "samples": [1.0] * minutes}
    return {"collaborators": collaborators, "rooms": collaborators // 4, "room_size": 4, "window_s": 60,
            "windows": minutes, "relay_idle_rss_mib": 20.0, "edits": 6000, "presence": 36000,
            "undelivered_edits": 0, "kpis": kpis}


class HostingLoad(unittest.TestCase):
    prices = tomllib.loads(adapters.PRICES.read_text())

    def test_measurements_become_records_named_by_scale(self):
        records = {item.kpi: item for item in adapters.hosting_load(load_report(), self.prices, ENV)}
        cpu = records["hosting.relay.cpu_s_per_collab_hour.n100"]
        self.assertEqual((cpu.unit, cpu.n, cpu.p50), ("s", 2, 36.0))
        fanout = records["hosting.relay.fanout_ms.n100"]
        self.assertEqual((fanout.n, fanout.p50, fanout.max), (3, 6.0, 30.0))
        self.assertEqual(cpu.meta["prices_checked"], self.prices["checked"])

    def test_the_price_sheet_prices_each_minute(self):
        records = {item.kpi: item for item in adapters.hosting_load(load_report(), self.prices, ENV)}
        # 36 CPU-s per collaborator-hour: 2 vCPU x 3600 s x 0.7 / 36 = 140 collaborators per VM,
        # well before memory binds; AWS bills the 10 MB of egress, Hetzner's 20 TB covers it.
        aws = records["hosting.relay.cost_usd_per_collab_hour.aws-t4g-small.n100"]
        self.assertAlmostEqual(aws.p50, 15.5 / 730 / 140 + 0.01 * 0.09)
        self.assertEqual((aws.unit, aws.meta["collaborators_per_vm"], aws.meta["binding"]), ("usd", "140", "cpu"))
        hetzner = records["hosting.relay.cost_usd_per_collab_hour.hetzner-cx22.n100"]
        self.assertAlmostEqual(hetzner.p50, 5.0 / 730 / 140)
        month = records["hosting.relay.cost_usd_per_user_month.hetzner-cx22.n100"]
        self.assertAlmostEqual(month.p50, hetzner.p50 * 88)

    def test_memory_binds_when_it_runs_out_first(self):
        usd, capacity, binding = adapters.collab_hour_usd(
            self.prices["provider"]["aws-t4g-small"], self.prices, cpu_s=1.0, rss_mib=10.0,
            egress_bytes=0.0, idle_rss_mib=33.6)
        self.assertEqual(binding, "memory")
        self.assertAlmostEqual(capacity, (2 * 1024 * 0.7 - 33.6) / 10)
        self.assertAlmostEqual(usd, 15.5 / 730 / capacity)

    def test_a_report_missing_a_kpi_fails(self):
        report = load_report()
        del report["kpis"]["hosting.relay.fanout_ms"]
        with self.assertRaisesRegex(ValueError, "reported"):
            adapters.hosting_load(report, self.prices, ENV)

    def test_an_older_bench_is_read_when_not_all_kpis_are_required(self):
        """The A/B lane reads a bench from before a KPI existed (the parent of the idle-barrier KPI)."""
        report = load_report()
        del report["kpis"]["hosting.relay.store_fsyncs_per_edit"]
        report["kpis"]["hosting.relay.future_metric"] = {"unit": "count", "samples": [1.0]}
        records = {item.kpi for item in adapters.hosting_load(report, self.prices, ENV, require_all=False)}
        self.assertIn("hosting.relay.cpu_s_per_collab_hour.n100", records)
        self.assertIn("hosting.relay.cost_usd_per_collab_hour.aws-t4g-small.n100", records)
        self.assertNotIn("hosting.relay.store_fsyncs_per_edit.n100", records)
        self.assertFalse(any("future_metric" in kpi for kpi in records))

    def test_a_report_that_cannot_be_priced_fails_even_partially(self):
        report = load_report()
        del report["kpis"]["hosting.relay.cpu_s_per_collab_hour"]
        with self.assertRaisesRegex(ValueError, "without"):
            adapters.hosting_load(report, self.prices, ENV, require_all=False)

    def test_the_kpis_are_the_benchs(self):
        """The adapter reads the KPI ids and units the Rust bench writes."""
        written = dict(re.findall(r'"(hosting\.relay\.[a-z_]+)": kpi\("(\w+)"', LOAD_BENCH.read_text()))
        self.assertEqual(written, adapters.HOSTING_KPIS)

    def test_every_hosting_budget_is_produced_nightly(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("hosting.")}
        produced = {item.kpi for item in adapters.hosting_load(load_report(), self.prices, ENV)}
        self.assertTrue(budgeted, "the hosting group has budgets")
        self.assertLessEqual(budgeted, produced)


if __name__ == "__main__":
    unittest.main()
