"""Tests of the benchmark adapters (tools/perf/adapters.py)."""

from __future__ import annotations

import json
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
FOOTPRINT = ROOT / "crates/system/fs/tests/document_media_footprint.rs"
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

    def test_footprint_lines_keep_their_unit(self):
        heap = "PERF document-media-upload-64m-peak-heap: {:.3} MiB (SLO <= 16.000 MiB)"
        logs = [gate_log(1.0) + "\n" + heap.format(9.5), gate_log(1.0) + "\n" + heap.format(10.5)]
        by_kpi = {item.kpi: item for item in adapters.fs_gate(logs, ENV)}
        upload = by_kpi["fs.document-media-upload-64m-peak-heap"]
        self.assertEqual((upload.n, upload.unit, upload.max), (2, "MiB", 10.5))
        self.assertEqual(by_kpi["fs.metadata-warm-8x-1000-list"].unit, "ms")
        mixed = gate_log(1.0) + "\nPERF metadata-warm-8x-1000-list: 1.000 MiB (SLO <= 2.000 MiB)"
        with self.assertRaises(ValueError):
            adapters.fs_gate([mixed], ENV)

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
        self.assertEqual(len(reports), 13, reports)
        # The same tail read retains a second, same-run ratio assertion.
        relative = dict(re.findall(
            r'report_relative\(\s*"([a-z0-9-]+)",\s*[^,]+,\s*\([^)]*\),\s*(\w+),?\s*\)', source))
        self.assertEqual(relative, {"document-media-cold-64m-tail-4m-read": "MEDIA_TAIL_READ_SHARE_OF_WHOLE_LOAD"})
        self.assertIn("document-media-cold-64m-tail-4m-read", reports)
        # The daemon heap KPIs of the footprint test, in MiB.
        footprint = FOOTPRINT.read_text()
        for match in re.finditer(r"const (\w+_HEAP_SLO): usize = (\d+)((?: \* 1024)+);", footprint):
            name, value, factors = match.groups()
            slos[name] = float(value) * 1024 ** factors.count("1024") / (1024 * 1024)
        heap = dict(re.findall(r'report_heap\(\s*"([a-z0-9-]+)",\s*[^,]+,\s*(\w+_HEAP_SLO),?\s*\)', footprint))
        self.assertEqual(len(heap), 3, heap)
        reports.update(heap)
        budgets = compare.load_budgets(compare.BUDGETS)
        expected = {f"fs.{name}": slos[constant] for name, constant in reports.items()}
        got = {kpi: entry["max"] for kpi, entry in budgets.kpis.items()
               if kpi.startswith("fs.") and entry.get("enforce")}
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


IDLE_GATE = ROOT / "crates/session/shell/examples/idle-wakeups.rs"


def idle_log(total: float, shell: float) -> str:
    # The executable prints the table on stderr and the record on stdout.
    return "\n".join([
        "idle wakeups over 10.0 s: 3.40/s across 1 processes",
        f"IDLE-WAKEUPS total_per_s={total:.3f} shell_per_s={shell:.3f} loop_ticks_per_s=0.000 "
        "cpu_percent=0.400 window_ms=10012 threads_started=0",
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


NAME_INDEX_TEST = ROOT / "crates/session/shell/src/shell/search/providers/files.rs"


def name_index_log(scale: float, names: int = 50_000) -> str:
    # The shape of a --nocapture run: libtest's prefix shares the line.
    return "\n".join([
        "running 1 test",
        "test shell::search::providers::files::tests::a_watched_change_costs_one_record_not_a_rebuild ... "
        f"FILES-NAME-INDEX names={names} rebuild_ms={1500 * scale:.3f} "
        f"change_ms={2.1 * scale:.3f},{2.4 * scale:.3f},{3.0 * scale:.3f} unchanged_ms=0.050,0.060 "
        f"rename_ms={3.2 * scale:.3f} burst64_ms={9.5 * scale:.3f}",
        "ok",
    ])


class FilesNameIndex(unittest.TestCase):
    def test_every_listed_value_is_a_sample(self):
        records = {item.kpi: item for item in adapters.files_name_index(
            [name_index_log(1.0), name_index_log(2.0)], ENV)}
        change = records["search.files_name_index.change_cpu_ms.50k"]
        self.assertEqual((change.unit, change.n, change.max), ("ms", 6, 6.0))
        self.assertEqual(records["search.files_name_index.unchanged_cpu_ms.50k"].n, 4)
        rebuild = records["search.files_name_index.rebuild_cpu_ms.50k"]
        self.assertEqual((rebuild.n, rebuild.max), (2, 3000.0))
        self.assertEqual(records["search.files_name_index.burst64_cpu_ms.50k"].max, 19.0)
        self.assertEqual(records["search.files_name_index.rename_cpu_ms.50k"].n, 2)
        self.assertEqual(change.meta, {"runs": "2"})

    def test_a_run_without_its_line_fails(self):
        with self.assertRaisesRegex(ValueError, "FILES-NAME-INDEX"):
            adapters.files_name_index([name_index_log(1.0), "running 1 test\ntest ... FAILED"], ENV)

    def test_runs_of_different_sizes_are_refused(self):
        with self.assertRaisesRegex(ValueError, "different index sizes"):
            adapters.files_name_index([name_index_log(1.0), name_index_log(1.0, names=20_000)], ENV)

    def test_the_fields_are_the_tests(self):
        """The test prints every field the adapter reads."""
        source = NAME_INDEX_TEST.read_text()
        printed = source[source.index('"FILES-NAME-INDEX'):]
        for field in (*adapters.FILES_NAME_INDEX_KPIS, "names"):
            self.assertIn(f"{field}=", printed)

    def test_every_name_index_kpi_has_a_budget(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        nightly = {kpi for kpi in budgets.kpis if kpi.startswith("search.")}
        produced = {item.kpi for item in adapters.files_name_index([name_index_log(1.0)], ENV)}
        self.assertEqual(nightly, produced)


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
        "FRAMEWORK-SCROLL-HOSTED tasks=5000 step=40 copying=false pixels_p50=983616 pixels_max=983616 "
        "copied_p50=0 whole=0 frames=120 cpu_ms=0.6 gpu_ms=1.6 gpu_full_ms=1.6 differing=0",
        f"FRAMEWORK-SCROLL-HOSTED tasks=5000 step=40 copying=true pixels_p50={pixels + 5000} "
        f"pixels_max={pixels * 2} copied_p50=700000 whole=0 frames=120 cpu_ms=0.7 gpu_ms=0.9 "
        f"gpu_full_ms=1.6 differing={differing}",
        "FRAMEWORK-SCROLL-PEER tasks=5000 step=40 copying=false pixels_p50=990000 pixels_max=990000 "
        "copied_p50=0 whole=0 frames=120 cpu_ms=0.5 gpu_ms=1.6 gpu_full_ms=1.6 differing=0",
        f"FRAMEWORK-SCROLL-PEER tasks=5000 step=40 copying=true pixels_p50={pixels + 9000} "
        f"pixels_max={pixels * 2} copied_p50=700000 whole=0 frames=120 cpu_ms=0.6 gpu_ms=0.9 "
        f"gpu_full_ms=1.6 differing={differing}",
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

    def test_the_hosted_copying_run_is_recorded_apart(self):
        records = {item.kpi: item for item in adapters.framework_scroll([scroll_log(90000)], ENV)}
        self.assertEqual(records["render.framework_scroll.hosted_pixels.5k"].p50, 95000.0)
        self.assertEqual(records["render.framework_scroll.pixels.5k"].p50, 90000.0)
        self.assertEqual(records["render.framework_scroll.hosted_whole_frames.5k"].p50, 0.0)

    def test_the_run_beside_a_peer_is_recorded_apart(self):
        records = {item.kpi: item for item in adapters.framework_scroll([scroll_log(90000)], ENV)}
        self.assertEqual(records["render.framework_scroll.peer_pixels.5k"].p50, 99000.0)
        self.assertEqual(records["render.framework_scroll.peer_whole_frames.5k"].p50, 0.0)
        self.assertEqual(records["render.framework_scroll.pixels.5k"].p50, 90000.0)

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


def focus_log(scale: float) -> str:
    """framework-scaling's `focus` output: the table, then one line per size."""
    return "\n".join([
        "tasks  add one task p50  focus order in it p50  live window focus order p50",
        f"  500  {20 * scale:>13.3} ms  {0.2 * scale:>18.4} ms  {0.15 * scale:>24.4} ms",
        "",
        f"FRAMEWORK-FOCUS tasks=500 add_ms={20 * scale:.4f},{22 * scale:.4f} "
        f"recompute_ms={0.2 * scale:.4f},{0.25 * scale:.4f} native_ms={0.15 * scale:.4f},{0.16 * scale:.4f}",
        f"FRAMEWORK-FOCUS tasks=1000 add_ms={60 * scale:.4f} recompute_ms={0.4 * scale:.4f} "
        f"native_ms={0.3 * scale:.4f}",
    ])


class FrameworkFocus(unittest.TestCase):
    def test_every_timing_is_a_sample(self):
        records = {item.kpi: item for item in adapters.framework_focus([focus_log(1.0), focus_log(2.0)], ENV)}
        adding = records["render.framework_focus.add_item_ms.500"]
        self.assertEqual((adding.unit, adding.n, adding.max), ("ms", 4, 44.0))
        self.assertEqual(records["render.framework_focus.recompute_ms.1k"].n, 2)
        self.assertEqual(records["render.framework_focus.native_recompute_ms.500"].unit, "ms")
        self.assertEqual(records["render.framework_focus.add_item_ms.1k"].meta, {"runs": "2"})

    def test_a_run_without_its_lines_fails(self):
        with self.assertRaisesRegex(ValueError, "FRAMEWORK-FOCUS"):
            adapters.framework_focus([focus_log(1.0), "tasks  add one task p50"], ENV)

    def test_the_fields_are_the_examples(self):
        """The example prints the line the adapter reads."""
        source = SCALING.read_text()
        printed = source[source.index('"FRAMEWORK-FOCUS'):]
        for field in ("tasks", "add_ms", "recompute_ms", "native_ms"):
            self.assertIn(f"{field}={{", printed)

    def test_every_nightly_size_has_a_budget(self):
        """The scheduled lane runs the focus mode at the sizes the budgets name."""
        workflow = (ROOT / ".forgejo/workflows/perf-scheduled.yml").read_text()
        [sizes] = re.findall(r"framework-scaling ([0-9,]+) focus", workflow)
        log = "\n".join(f"FRAMEWORK-FOCUS tasks={tasks} add_ms=1 recompute_ms=1 native_ms=1"
                        for tasks in sizes.split(","))
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("render.framework_focus.")}
        produced = {item.kpi for item in adapters.framework_focus([log], ENV)}
        self.assertEqual(budgeted, produced)


NOTES_LATENCY = ROOT / "apps/notes/examples/notes-latency.rs"


def notes_latency_log(scale: float, gpu: bool = True) -> str:
    """notes-latency's output: the table, then its one JSON line."""
    def figures(values: list[float]) -> dict:
        values = [value * scale for value in values]
        return {"samples": len(values), "p50": sorted(values)[len(values) // 2], "p99": max(values),
                "max": max(values), "values": values}
    workloads = {workload: {"to_frame_ms": figures([1.0, 2.0, 3.0]),
                            "to_raster_ms": figures([2.0, 3.0, 4.0]) if gpu else None}
                 for workload in adapters.NOTES_LATENCY_WORKLOADS}
    report = {"pages": 27, "blocks": 838, "gpu": gpu, "workloads": workloads}
    return "\n".join([
        "Notes input latency, 27 notes / 838 blocks, 1280x900 at scale 1, release build, x86_64",
        "type             3     2.00     3.00     3.00     3.00     4.00     4.00",
        f"NOTES-LATENCY {json.dumps(report)}",
    ])


class NotesLatency(unittest.TestCase):
    def test_every_input_is_a_sample_of_its_workload(self):
        records = {item.kpi: item for item in adapters.notes_latency(
            [notes_latency_log(1.0), notes_latency_log(2.0)], ENV)}
        typing = records["render.notes_latency.type.to_frame_ms"]
        self.assertEqual((typing.unit, typing.n, typing.max), ("ms", 6, 6.0))
        self.assertEqual(records["render.notes_latency.open.to_raster_ms"].max, 8.0)
        self.assertEqual(records["render.notes_latency.scroll.to_frame_ms"].meta, {"runs": "2"})

    def test_a_run_without_its_line_fails(self):
        with self.assertRaisesRegex(ValueError, "NOTES-LATENCY"):
            adapters.notes_latency([notes_latency_log(1.0), "workload  samples  to frame"], ENV)

    def test_a_run_without_a_gpu_fails(self):
        with self.assertRaisesRegex(ValueError, "no GPU"):
            adapters.notes_latency([notes_latency_log(1.0, gpu=False)], ENV)

    def test_the_example_prints_what_the_adapter_reads(self):
        source = NOTES_LATENCY.read_text()
        self.assertIn('"NOTES-LATENCY {record}"', source)
        for field in ("to_frame_ms", "to_raster_ms", "values", *adapters.NOTES_LATENCY_WORKLOADS):
            self.assertIn(f'"{field}"', source)

    def test_every_kpi_has_a_budget_and_the_render_job_runs_it(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("render.notes_latency.")}
        produced = {item.kpi for item in adapters.notes_latency([notes_latency_log(1.0)], ENV)}
        self.assertEqual(budgeted, produced)
        workflow = (ROOT / ".forgejo/workflows/perf-scheduled.yml").read_text()
        self.assertIn("--example notes-latency", workflow)
        self.assertIn("adapters.py notes-latency", workflow)


LOAD_BENCH = ROOT / "crates/hosting/tilo-relay/examples/relay_load/main.rs"


def load_report(collaborators: int = 100, minutes: int = 2) -> dict:
    """A `hosting-load.json` as the bench writes it."""
    per_minute = {"hosting.relay.cpu_s_per_collab_hour": ("s", 36.0),
                  "hosting.relay.rss_mib_per_collab": ("MiB", 1.0),
                  "hosting.relay.egress_bytes_per_collab_hour": ("bytes", 10e6),
                  "hosting.relay.frame_bytes_per_collab_hour": ("bytes", 8e6),
                  "hosting.relay.client_packets_per_collab_hour": ("count", 23_000.0),
                  "hosting.relay.client_data_packets_per_collab_hour": ("count", 6_000.0),
                  "hosting.relay.datagrams_per_collab_hour": ("count", 26_000.0),
                  "hosting.relay.ack_only_datagrams_per_collab_hour": ("count", 7_500.0)}
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
        acks = records["hosting.relay.ack_only_datagrams_per_collab_hour.n100"]
        self.assertEqual((acks.unit, acks.n, acks.p50), ("count", 2, 7_500.0))
        self.assertEqual(cpu.meta["prices_checked"], self.prices["checked"])
        self.assertEqual(cpu.meta["relay_cpus"], "all")

    def test_records_say_how_many_cpus_the_relay_ran_on(self):
        report = {**load_report(), "relay_cpus": 2}
        records = adapters.hosting_load(report, self.prices, ENV)
        self.assertEqual({item.meta["relay_cpus"] for item in records}, {"2"})

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
        """The A/B lane reads a bench from before a KPI existed (#4047's parent)."""
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
        # ... and the room host job, at the guest count the nightly runs.
        workflow = (ROOT / ".forgejo/workflows/perf-scheduled.yml").read_text()
        [guests] = re.findall(r"tilo-room-load bench --guests (\d+)", workflow)
        config = {**room_report()["config"], "guests": int(guests)}
        produced |= {item.kpi for item in adapters.room_load([room_report(config=config, joined=int(guests))], ENV)}
        self.assertTrue(budgeted, "the hosting group has budgets")
        self.assertLessEqual(budgeted, produced)


BROWSER_HARNESS = ROOT / "scripts/perf/browser_qualification.mjs"
BROWSER_ENV = {**ENV, "profile": "release", "renderer": "swiftshader"}


def browser_report(**changes) -> dict:
    """The shape scripts/perf/browser_qualification.mjs writes, reduced."""
    report = {
        "schema": 1, "renderer": "swiftshader",
        "bundle": {"revision": "a" * 40, "dirty": False, "profile": "release", "digest": "d" * 64,
                   "runtimeSha256": "e" * 64},
        "adapter": {"vendor": "google", "architecture": "swiftshader", "device": "", "description": ""},
        "browser": {"product": "Chrome/153.0.8010.12"},
        "viewport": {"width": 1920, "height": 1080, "deviceScaleFactor": 1},
        "workload": {"rounds": 2, "apps": ["so.tilo.notes", "so.tilo.sheets"], "inputHz": 120, "cpuThrottle": 1,
                     "demoSeconds": 5},
        "samples": {"load_ms": [900.0, 1100.0], "open_ms": [20.0, 30.0, 25.0, 35.0],
                    "open_ready_ms": [60.0, 90.0, 70.0, 80.0], "activate_ms": [12.0, 14.0, 40.0],
                    "demo_interval_ms": [16.0, 17.0, 33.0, 18.0], "demo_work_ms": [9.0, 10.0, 11.0],
                    "idle_per_s": [0.0, 0.5]},
    }
    for key, value in changes.items():
        if key in report["bundle"]:
            report["bundle"] = {**report["bundle"], key: value}
        elif key in report["workload"]:
            report["workload"] = {**report["workload"], key: value}
        else:
            report[key] = value
    return report


ROOM_LOAD = ROOT / "crates/dev/room-load/src"


def room_report(join_p99: float = 400.0, rss: int = 700, **changes) -> dict:
    """A `tilo-room-load bench` report, with the fields the adapter reads."""
    report = {
        "config": {"guests": 300, "rooms": 1, "cursor_hz": 30.0, "chat_every_s": 60.0, "host_threads": None},
        "joined": 300, "chat_rx": 12000, "failures": {},
        "join_ms": {"p50": 90.0, "p95": 300.0, "p99": join_p99, "max": 500.0},
        "chat_ms": {"p50": 20.0, "p95": 60.0, "p99": 90.0, "max": 200.0},
        "host": {"rss_mib_peak": rss, "hold_cpu_cores": 1.5, "machine_cores": 12},
    }
    report.update(changes)
    return report


class RoomLoad(unittest.TestCase):
    def test_each_run_is_one_sample_of_each_kpi(self):
        records = {item.kpi: item for item in adapters.room_load(
            [room_report(400.0, 700), room_report(500.0, 720), room_report(450.0, 710)], ENV)}
        self.assertEqual(set(records), {f"{kpi}.g300" for kpi in adapters.ROOM_LOAD_KPIS})
        join = records["hosting.room.join_p99_ms.g300"]
        self.assertEqual((join.n, join.p50, join.max), (3, 450.0, 500.0))
        self.assertEqual(records["hosting.room.rss_mib_peak.g300"].unit, "MiB")
        self.assertEqual(records["hosting.room.cpu_percent.g300"].p50, 150.0)
        self.assertEqual(join.meta["runs"], "3")

    def test_a_run_with_failures_or_missing_guests_is_not_a_measurement(self):
        for broken in (room_report(failures={"join: no Ready before the timeout": 2}),
                       room_report(joined=298),
                       room_report(chat_rx=0)):
            with self.assertRaises(ValueError):
                adapters.room_load([room_report(), broken], ENV)
        with self.assertRaises(ValueError):
            adapters.room_load([room_report(), room_report(config={**room_report()["config"], "guests": 50})], ENV)

    def test_the_fields_read_are_the_ones_the_bench_writes(self):
        source = "\n".join(path.read_text(encoding="utf-8") for path in sorted(ROOM_LOAD.glob("*.rs")))
        for field in ('"p99"', '"rss_mib_peak"', '"hold_cpu_cores"', '"machine_cores"', '"chat_rx"',
                      '"joined"', '"failures"', '"cursor_hz"', '"chat_every_s"', '"host_threads"'):
            self.assertIn(field, source)


class BrowserQualification(unittest.TestCase):
    def test_each_sample_list_becomes_its_kpi(self):
        records = {item.kpi: item for item in adapters.browser_qualification(browser_report(), BROWSER_ENV)}
        self.assertEqual(set(records), {kpi for kpi, _ in adapters.BROWSER_KPIS.values()})
        interval = records["browser.demo.frame_interval"]
        self.assertEqual((interval.unit, interval.n, interval.p50, interval.max), ("ms", 4, 17.0, 33.0))
        idle = records["browser.idle.presented_per_s"]
        self.assertEqual((idle.unit, idle.n, idle.max), ("per_s", 2, 0.5))
        self.assertEqual(records["browser.activate.first_frame"].p99, 40.0)
        meta = interval.meta
        self.assertEqual((meta["runtime_sha256"], meta["viewport"], meta["adapter"]),
                         ("e" * 64, "1920x1080@1", "google swiftshader"))

    def test_a_report_of_another_environment_is_refused(self):
        for report, reason in ((browser_report(renderer="gpu"), "not the env's swiftshader"),
                               (browser_report(profile="dev"), "dev build"),
                               (browser_report(revision="b" * 40), "built from b"),
                               (browser_report(dirty=True), "dirty"),
                               (browser_report(cpuThrottle=4), "throttled 4x"),
                               (browser_report(schema=2), "schema")):
            with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, reason):
                adapters.browser_qualification(report, BROWSER_ENV)

    def test_the_ab_lane_accepts_its_untracked_output_but_not_another_commit(self):
        records = adapters.browser_qualification(browser_report(dirty=True), BROWSER_ENV, require_clean=False)
        self.assertTrue(records)
        with self.assertRaisesRegex(ValueError, "built from b"):
            adapters.browser_qualification(browser_report(revision="b" * 40), BROWSER_ENV, require_clean=False)

    def test_a_measurement_without_samples_fails(self):
        report = browser_report()
        report["samples"] = {**report["samples"], "activate_ms": []}
        with self.assertRaisesRegex(ValueError, "activate_ms"):
            adapters.browser_qualification(report, BROWSER_ENV)

    def test_the_fields_are_the_harnesses(self):
        """The harness writes every sample list the adapter reads."""
        source = BROWSER_HARNESS.read_text()
        written = set(re.findall(r"\b([a-z_]+_(?:ms|per_s)): rounds", source[source.index("samples: {"):]))
        self.assertEqual(written, set(adapters.BROWSER_KPIS))

    def test_a_run_without_the_demo_reports_everything_else(self):
        report = browser_report(demoSeconds=0)
        report["samples"] = {field: values for field, values in report["samples"].items()
                             if field not in adapters.BROWSER_DEMO_FIELDS}
        records = {item.kpi for item in adapters.browser_qualification(report, BROWSER_ENV)}
        self.assertNotIn("browser.demo.frame_interval", records)
        self.assertIn("browser.activate.first_frame", records)
        with self.assertRaisesRegex(ValueError, "demo samples from a run without the demo"):
            adapters.browser_qualification(browser_report(demoSeconds=0), BROWSER_ENV)

    def test_every_kpi_the_scheduled_lane_reports_has_a_budget(self):
        """The nightly job runs without the demo (perf-scheduled.yml: --demo-seconds 0)."""
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("browser.")}
        self.assertEqual(budgeted, {kpi for field, (kpi, _) in adapters.BROWSER_KPIS.items()
                                    if field not in adapters.BROWSER_DEMO_FIELDS})
        workflow = (ROOT / ".forgejo/workflows/perf-scheduled.yml").read_text()
        self.assertIn("--demo-seconds 0", workflow)


STREAM_HARNESS = ROOT / "scripts/perf/native_stream_qualification.mjs"


def stream_report(**changes) -> dict:
    """The shape scripts/perf/native_stream_qualification.mjs writes, reduced."""
    report = {
        "schema": 1, "renderer": "swiftshader",
        "bundle": {"revision": "a" * 40, "dirty": False, "profile": "release", "digest": "d" * 64},
        "browser": {"product": "Chrome/153.0.8010.12"},
        "viewport": {"width": 960, "height": 540, "deviceScaleFactor": 1},
        "session": {"encoder": "x264enc (software, software)", "hardware": False, "fps": 30, "width": 1280,
                    "height": 720, "codec": "h264"},
        "workload": {"seconds": 10, "inputs": 20, "carrier": "webtransport", "codec": "h264"},
        "counts": {"sent": 300, "deliveredPercent": 99.33},
        "correctness": {"failures": []},
        "samples": {"startup_ms": [640.0], "frame_latency_ms": [30.0, 35.0, 80.0], "frame_interval_ms": [33.0, 34.0],
                    "encode_ms": [4.0, 5.0], "input_ms": [3.0, 6.0]},
    }
    for key, value in changes.items():
        if key in report["bundle"]:
            report["bundle"] = {**report["bundle"], key: value}
        else:
            report[key] = value
    return report


def codec_report(codec: str) -> dict:
    """A report of a session streaming `codec` (`vp9`: a session without an H.264 encoder)."""
    report = stream_report()
    return stream_report(session={**report["session"], "encoder": f"{codec} (software, software)", "codec": codec},
                         workload={**report["workload"], "codec": codec})


class NativeStream(unittest.TestCase):
    def test_each_sample_list_becomes_its_kpi_and_misses_one_more(self):
        records = {item.kpi: item for item in adapters.native_stream(stream_report(), BROWSER_ENV)}
        self.assertEqual(set(records), {kpi for kpi, _ in adapters.STREAM_KPIS.values()} | {"stream.frame.missed"})
        latency = records["stream.frame.latency"]
        self.assertEqual((latency.unit, latency.n, latency.p50, latency.max), ("ms", 3, 35.0, 80.0))
        missed = records["stream.frame.missed"]
        self.assertEqual((missed.unit, missed.n, missed.max), ("percent", 1, 0.67))
        self.assertEqual((latency.meta["encoder_hardware"], latency.meta["window"], latency.meta["carrier"],
                          latency.meta["codec"]), ("false", "1280x720", "webtransport", "h264"))

    def test_a_vp9_stream_is_its_own_series(self):
        """VP9's records carry the codec in their ids, so no H.264 baseline ever judges them."""
        records = {item.kpi: item for item in adapters.native_stream(codec_report("vp9"), BROWSER_ENV)}
        self.assertEqual(set(records), {"stream.vp9.startup.first_picture", "stream.vp9.frame.latency",
                                        "stream.vp9.frame.interval", "stream.vp9.encode.frame",
                                        "stream.vp9.input.latency", "stream.vp9.frame.missed"})
        latency = records["stream.vp9.frame.latency"]
        self.assertEqual((latency.unit, latency.n, latency.p50, latency.meta["codec"]), ("ms", 3, 35.0, "vp9"))
        self.assertEqual(records["stream.vp9.frame.missed"].max, 0.67)

    def test_a_session_that_streamed_another_codec_is_refused(self):
        """A session that fell back to H.264 must not fill VP9's series, nor the reverse."""
        fell_back, upgraded, garbled = codec_report("vp9"), stream_report(), codec_report("VP9.x")
        fell_back["session"]["codec"] = "h264"
        upgraded["session"]["codec"] = "vp9"
        for report, reason in ((fell_back, "streamed h264, not the requested vp9"),
                               (upgraded, "streamed vp9, not the requested h264"),
                               (garbled, "names codec")):
            with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, reason):
                adapters.native_stream(report, BROWSER_ENV)

    def test_a_report_of_another_environment_or_an_incorrect_stream_is_refused(self):
        for report, reason in ((stream_report(renderer="gpu"), "not the env's swiftshader"),
                               (stream_report(profile="dev"), "dev build"),
                               (stream_report(revision="b" * 40), "built from b"),
                               (stream_report(dirty=True), "dirty"),
                               (stream_report(schema=2), "schema"),
                               (stream_report(correctness={"failures": ["2 corrupt pictures"]}), "corrupt"),
                               (stream_report(counts={"sent": 3, "deliveredPercent": 140.0}), "not a share")):
            with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, reason):
                adapters.native_stream(report, BROWSER_ENV)

    def test_a_measurement_without_samples_fails(self):
        report = stream_report()
        report["samples"] = {**report["samples"], "input_ms": []}
        with self.assertRaisesRegex(ValueError, "input_ms"):
            adapters.native_stream(report, BROWSER_ENV)

    def test_the_fields_are_the_harnesses(self):
        """The harness writes every sample list the adapter reads."""
        source = STREAM_HARNESS.read_text()
        samples = source[source.index("samples: {"):source.index("counts: {")]
        self.assertEqual(set(re.findall(r"\b([a-z_]+_ms):", samples)), set(adapters.STREAM_KPIS))

    def test_every_kpi_the_scheduled_lane_reports_has_a_budget(self):
        """Each codec the nightly streams (H.264 by default, then each `--codec`) has a budget for every KPI."""
        workflow = (ROOT / ".forgejo/workflows/perf-scheduled.yml").read_text().replace("\\\n", " ")
        runs = [line for line in workflow.splitlines() if line.lstrip().startswith("node scripts/perf/native_stream")]
        codecs = {(re.findall(r"--codec (\w+)", run) or [adapters.STREAM_DEFAULT_CODEC])[0] for run in runs}
        self.assertEqual(codecs, {"h264", "vp9"})
        budgets = compare.load_budgets(compare.BUDGETS)
        budgeted = {kpi for kpi in budgets.kpis if kpi.startswith("stream.")}
        produced = {item.kpi for codec in codecs for item in adapters.native_stream(codec_report(codec), BROWSER_ENV)}
        self.assertEqual(budgeted, produced)


if __name__ == "__main__":
    unittest.main()
