#!/usr/bin/env python3
"""Turn existing benchmark output into records, without changing the benchmark.

Each adapter reads what a benchmark already prints and emits records through
`record.from_samples`. A benchmark that writes records itself needs no adapter.

- `fs-gate LOG...`: the tilo-fs latency benchmarks
  (`crates/system/fs/tests/performance_timing.rs`, `TILO_FS_PERF_GATE=1`) print
  `PERF <name>: <x> ms (SLO <= <y> ms)` per scenario, and its heap footprint
  test (`crates/system/fs/tests/document_media_footprint.rs`) prints the same
  shape in `MiB`. Each log is one run; repeated runs become the samples of
  `fs.<name>`, in the unit the line names.
- `tapp-sizes ENTRIES_JSON`: the public app registry's signed `entries`
  index (`https://apps.tilo.so/entries`). The newest version of every app per
  architecture is one sample of `footprint.tapp.bytes.<arch>`: the bytes a
  user downloads. The apps are those with a budget in
  `tools/perf/tapp-size-budget/` (the default collection); runtime and
  provider stubs and withdrawn packages are left out and named in `meta`.
- `runtime-sizes MEASUREMENTS_JSON`: what `runtime_size_ratchet.py published
  --measurements` read from the newest published shared runtime
  (`libruntime_dylib.so`) of each architecture. Each of its numbers is one
  sample of `footprint.runtime.<metric>.<arch>`: `bytes`, `relocation_bytes`,
  `relocated_bytes` (bytes) and `symbol_relocations` (count).
- `workspace-switch LOG...`: the compositor's `WORKSPACE-SWITCH` session-log
  lines (`crates/session/shell/src/compositor/switch_latency.rs`), one per
  switch, become `latency.workspace_switch.first_frame` (chord to the first
  frame shown), `.settled` (chord to the resting frame shown after the frame
  run) and `.dropped_frames` (refresh intervals in the run that showed no new
  frame). `scripts/perf/latency_bench.py` (headless, nightly) and
  `scripts/perf/device_latency_bench.py` (reference hardware) call it.
- `idle-wakeups LOG...`: the idle-wakeup gate
  (`crates/session/shell/examples/idle-wakeups.rs`) prints one
  `IDLE-WAKEUPS total_per_s=<x> shell_per_s=<y> ...` line per run. Each log is
  one run; repeated runs become the samples of `wakeups.headless_two_apps`
  (the compositor's whole process tree), `.shell` (its own process) and
  `wakeups.frame_loop_ticks.headless_two_apps` (its frame loop's iterations).
- `files-name-index LOG...`: the Files search name-index test
  (`a_watched_change_costs_one_record_not_a_rebuild` in
  `crates/session/shell/src/shell/search/providers/files.rs`) prints one
  `FILES-NAME-INDEX names=<n> rebuild_ms=<x> change_ms=<a,b,...>
  unchanged_ms=<a,b,...> rename_ms=<y> burst64_ms=<z>` line per run: process
  CPU for one watched change to an index of `n` names, for an unchanged
  one, a rename, a 64-file burst, and a whole-index build. Each becomes
  `search.files_name_index.<metric>.<size>` (`change_cpu_ms`,
  `unchanged_cpu_ms`, `rename_cpu_ms`, `burst64_cpu_ms`, `rebuild_cpu_ms`),
  every listed value one sample, with sizes written `50k`.
- `framework-open LOG...`: the `FRAMEWORK-OPEN tasks=<n> samples_ms=<a,b,...>
  allocations=<x> heap_per_task=<y>` lines of
  `crates/sdk/tilo-app/examples/framework-scaling.rs`, one per list
  size: opening a framework app over a record holding `n` items, to its first
  laid-out frame. Each size becomes `render.framework_open.ms.<size>` (every
  open is a sample), `.allocations.<size>` and `.heap_per_item.<size>` (one
  sample per run), with sizes written `5k`, `50k`.
- `framework-scroll LOG...`: the `FRAMEWORK-SCROLL tasks=<n> step=<px>
  copying=<bool> pixels_p50=<x> pixels_max=<y> copied_p50=<z> whole=<w>
  frames=<f> cpu_ms=<c> gpu_ms=<g> gpu_full_ms=<h> differing=<d>` lines of
  the same example's `scroll` mode: a long list scrolled frame by frame,
  each frame presented as a window that copies what a scroll keeps on screen
  presents it. The line with copies and a whole-pixel step becomes, per
  size, `render.framework_scroll.pixels.<size>` (pixels one frame repaints,
  p50), `.whole_frames.<size>` (frames repainted whole), `.cpu_ms.<size>`
  (one scroll frame on the CPU, p50) and `.raster_ms.<size>` (presenting it
  on the GPU, copies included, p50), one sample per run. A run whose
  presented pixels differ from a full render fails. `FRAMEWORK-SCROLL-HOSTED`
  lines are the same list shown in a region of a host's scene, as the shell
  shows an in-process app: its line with copies and a whole-pixel step
  becomes `render.framework_scroll.hosted_pixels.<size>` and
  `.hosted_whole_frames.<size>`. `FRAMEWORK-SCROLL-PEER` lines are the
  window scrolled with a collaborator's pointer resting over the list:
  `render.framework_scroll.peer_pixels.<size>` and `.peer_whole_frames.<size>`.
- `framework-focus LOG...`: the `FRAMEWORK-FOCUS tasks=<n> add_ms=<a,...>
  recompute_ms=<b,...> native_ms=<c,...>` lines of the same example's `focus`
  mode: the keyboard focus order of a window over `n` items (about four
  widgets per item). Each size becomes `render.framework_focus.add_item_ms.<size>`
  (an agent call adding one item, end to end), `.recompute_ms.<size>` (one
  focus-order recomputation inside it) and `.native_recompute_ms.<size>` (one
  recomputation of the live window's order); every timing is a sample.
- `notes-latency LOG...`: the `NOTES-LATENCY {json}` line of
  `apps/notes/examples/notes-latency.rs`: Notes over a 27-note, 838-block
  notebook, each input sent through the host's input path and timed to the
  end of the host frame that shows it and to the GPU raster of that frame.
  Each workload (`type`, `enter`, `backspace`, `scroll`, `pointer`, `idle`,
  `open`) becomes `render.notes_latency.<workload>.to_frame_ms` and
  `.to_raster_ms`, every input (or input-free frame) a sample. A run without
  raster timings (no GPU) fails.
- `hosting-load REPORT_JSON`: the relay load bench's `hosting-load.json`
  (`crates/hosting/tilo-relay/examples/relay_load`). Each KPI it measured
  becomes `<kpi>.n<collaborators>`, one sample per measured minute (fan-out
  latency: one per delivered edit), and each provider in
  `tools/perf/prices.toml` adds `hosting.relay.cost_usd_per_collab_hour.<provider>`
  and `hosting.relay.cost_usd_per_user_month.<provider>`, priced per minute.
- `room-load REPORT_JSON...`: `tilo-room-load bench` reports
  (`crates/dev/room-load`), one per run of the same configuration. Each run
  is one sample of `hosting.room.<kpi>.g<guests>` (`ROOM_LOAD_KPIS`): the
  guests' p99 join and chat delivery, the host's peak resident memory and its
  CPU during the hold. A run in which a guest failed or did not join is not a
  measurement and fails the adapter.
- `browser-qualification REPORT_JSON`: the browser runtime qualification's
  report (`scripts/perf/browser_qualification.mjs`). Each sample list becomes
  one `browser.*` KPI (`BROWSER_KPIS`): every load, open, activation, demo
  frame interval and frame's runtime CPU is a sample, and each round's idle
  frame rate one; the demo's two only when the run had the demo. The report must name the env's renderer, commit and profile,
  and an unthrottled page.
- `native-stream REPORT_JSON`: the native stream qualification's report
  (`scripts/perf/native_stream_qualification.mjs`): a session's window
  streamed to the browser desktop. Each sample list becomes one `stream.*` KPI
  (`STREAM_KPIS`), and the frames sent but never drawn one
  `stream.frame.missed` sample (percent). A stream in a codec other than
  H.264 (`--codec vp9`) is its own series, its codec after the group
  (`stream.vp9.frame.latency`), so it never shares a baseline with H.264's.
  The report must name the env's renderer, commit and profile, and be
  correct: an incorrect run is a failure, never a measurement. A
  qualification that names the browser it drove (`--platform safari`, which
  Safari's WebDriver drives on a macOS host) must have streamed what it asked
  for: over the HTTPS stream, since Safari has no WebTransport, and in the
  codec it asked for.

Usage:

    python3 tools/perf/adapters.py fs-gate --env-json env.json run-*.log >> results.jsonl
    python3 tools/perf/adapters.py tapp-sizes --env-json env.json entries.json >> results.jsonl
    python3 tools/perf/adapters.py runtime-sizes --env-json env.json runtime.json >> results.jsonl
    python3 tools/perf/adapters.py workspace-switch --env-json env.json session.log >> results.jsonl
    python3 tools/perf/adapters.py idle-wakeups --env-json env.json run-*.log >> results.jsonl
    python3 tools/perf/adapters.py files-name-index --env-json env.json run-*.log >> results.jsonl
    python3 tools/perf/adapters.py framework-open --env-json env.json framework-open.log >> results.jsonl
    python3 tools/perf/adapters.py framework-scroll --env-json env.json framework-scroll.log >> results.jsonl
    python3 tools/perf/adapters.py framework-focus --env-json env.json framework-focus.log >> results.jsonl
    python3 tools/perf/adapters.py notes-latency --env-json env.json notes-latency.log >> results.jsonl
    python3 tools/perf/adapters.py hosting-load --env-json env.json hosting-load.json >> results.jsonl
    python3 tools/perf/adapters.py browser-qualification --env-json env.json browser.json >> results.jsonl
    python3 tools/perf/adapters.py native-stream --env-json env.json stream.json >> results.jsonl

An adapter that finds nothing to record fails: a benchmark whose output format
drifted must not turn into a silently empty night.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
import tomllib
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import record  # noqa: E402
import runtime_size_ratchet  # noqa: E402
import tapp_size_ratchet  # noqa: E402

# Searched, not anchored: with --nocapture, libtest's "test NAME ... " prefix can
# share the line with the test's first stderr line when both streams are merged.
FS_GATE_LINE = re.compile(r"(?:^|\s)PERF (?P<name>[a-z0-9][a-z0-9-]*): (?P<value>[0-9.]+) (?P<unit>ms|MiB) "
                          r"\(SLO <= [0-9.]+ (?P=unit)\)$")


def fs_gate(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for the scenarios of one or more tilo-fs gate runs."""
    samples: dict[str, list[float]] = {}
    units: dict[str, str] = {}
    for log in logs:
        seen = set()
        for line in log.splitlines():
            match = FS_GATE_LINE.search(line.strip())
            if match is None:
                continue
            name = match.group("name")
            if name in seen:
                raise ValueError(f"fs gate scenario {name} printed twice in one run")
            seen.add(name)
            if units.setdefault(name, match.group("unit")) != match.group("unit"):
                raise ValueError(f"fs gate scenario {name} printed in both {units[name]} and {match.group('unit')}")
            samples.setdefault(name, []).append(float(match.group("value")))
    if not samples:
        raise ValueError("no `PERF <name>: <x> ms (SLO <= <y> ms)` lines: did the gate run with TILO_FS_PERF_GATE=1?")
    counts = {name: len(values) for name, values in samples.items()}
    if len(set(counts.values())) != 1:
        raise ValueError(f"fs gate scenarios ran unequal times: {counts}")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(f"fs.{name}", units[name], values, meta=meta, **base)
            for name, values in sorted(samples.items())]


def tapp_sizes(entries: dict[str, Any], env: dict[str, Any],
               population: set[str] | None = None) -> list[record.Record]:
    """Records for the newest published tapp of every app, per architecture.

    `population` is the package ids that count as apps; every other package is
    left out and named in the record's meta. None counts every package.
    """
    by_arch: dict[str, list[float]] = {}
    outside = set()
    for (package, arch), size in tapp_size_ratchet.newest_tapps(entries).items():
        if population is not None and package not in population:
            outside.add(package)
            continue
        by_arch.setdefault(arch, []).append(float(size))
    if not by_arch:
        raise ValueError("the registry index lists no tapp of a budgeted app")
    meta = {**env.get("meta", {}), "registry_sequence": str(entries.get("sequence", ""))}
    if population is not None:
        meta.update(population=tapp_size_ratchet.DIRECTORY, not_counted=",".join(sorted(outside)))
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(f"footprint.tapp.bytes.{arch}", "bytes", sizes, meta=meta, **base)
            for arch, sizes in sorted(by_arch.items())]


def runtime_sizes(measured: dict[str, Any], env: dict[str, Any]) -> list[record.Record]:
    """Records for the published shared runtime of each architecture."""
    runtimes = measured.get("runtimes") or {}
    if not runtimes:
        raise ValueError("the runtime measurements name no architecture")
    base = {name: value for name, value in env.items() if name != "meta"}
    records = []
    for arch, values in sorted(runtimes.items()):
        meta = {**env.get("meta", {}), "registry_sequence": str(measured.get("sequence", "")),
                "version": str(values.get("version", "")), "runtime": str(values.get("runtime", ""))}
        for metric in runtime_size_ratchet.METRICS:
            value = values[metric]
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise ValueError(f"runtime {arch} {metric}: invalid measurement {value!r}")
            unit = "count" if metric == "symbol_relocations" else "bytes"
            records.append(record.from_samples(f"footprint.runtime.{metric}.{arch}", unit, [float(value)],
                                               meta=meta, **base))
    return records


SWITCH_FIELDS = ("first_queued_us", "first_shown_us", "settled_queued_us", "settled_shown_us",
                 "frames", "dropped", "interval_us")
SWITCH_LINE = re.compile(r"(?:^|\s)WORKSPACE-SWITCH ((?:[a-z_]+=[0-9]+ ?)+)$")


def switch_samples(text: str) -> list[dict[str, int]]:
    """Every `WORKSPACE-SWITCH` line of a session log, in order."""
    samples = []
    for line in text.splitlines():
        match = SWITCH_LINE.search(line.rstrip())
        if match is None:
            continue
        fields = dict(pair.split("=", 1) for pair in match.group(1).split())
        missing = [name for name in SWITCH_FIELDS if name not in fields]
        if missing:
            raise ValueError(f"WORKSPACE-SWITCH line lacks {missing}: {line.strip()}")
        samples.append({name: int(fields[name]) for name in SWITCH_FIELDS})
    return samples


def workspace_switch(samples: list[dict[str, int]], env: dict[str, Any],
                     kpi: str = "latency.workspace_switch.{}") -> list[record.Record]:
    """Records for the workspace switches of one run; `kpi` names them from the metric."""
    if not samples:
        raise ValueError("no `WORKSPACE-SWITCH` lines: did the compositor switch workspace, and is it "
                         "new enough to time switches?")
    intervals = sorted({sample["interval_us"] for sample in samples})
    meta = {**env.get("meta", {}), "interval_us": ",".join(str(value) for value in intervals),
            "frames_min": str(min(sample["frames"] for sample in samples))}
    base = {name: value for name, value in env.items() if name != "meta"}
    series = {
        "first_frame": ("ms", [sample["first_shown_us"] / 1000 for sample in samples]),
        "settled": ("ms", [sample["settled_shown_us"] / 1000 for sample in samples]),
        "dropped_frames": ("count", [float(sample["dropped"]) for sample in samples]),
    }
    return [record.from_samples(kpi.format(name), unit, values, meta=meta, **base)
            for name, (unit, values) in series.items()]


IDLE_WAKEUPS_LINE = re.compile(r"(?:^|\s)IDLE-WAKEUPS ((?:[a-z_]+=[0-9.]+ ?)+)$")
IDLE_WAKEUPS_KPIS = {"total_per_s": "wakeups.headless_two_apps",
                     "shell_per_s": "wakeups.headless_two_apps.shell",
                     "loop_ticks_per_s": "wakeups.frame_loop_ticks.headless_two_apps"}


def idle_wakeups(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the idle-wakeup gate."""
    samples: dict[str, list[float]] = {kpi: [] for kpi in IDLE_WAKEUPS_KPIS.values()}
    windows = []
    for log in logs:
        lines = [match for line in log.splitlines() if (match := IDLE_WAKEUPS_LINE.search(line.rstrip()))]
        if len(lines) != 1:
            raise ValueError(f"expected one `IDLE-WAKEUPS` line per run, found {len(lines)}: "
                             "did the idle-wakeups executable finish its measurement?")
        fields = dict(pair.split("=", 1) for pair in lines[0].group(1).split())
        missing = [name for name in (*IDLE_WAKEUPS_KPIS, "window_ms") if name not in fields]
        if missing:
            raise ValueError(f"IDLE-WAKEUPS line lacks {missing}")
        for field, kpi in IDLE_WAKEUPS_KPIS.items():
            samples[kpi].append(float(fields[field]))
        windows.append(fields["window_ms"])
    if not logs:
        raise ValueError("no idle-wakeup gate logs")
    meta = {**env.get("meta", {}), "runs": str(len(logs)), "window_ms": ",".join(sorted(set(windows)))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, "per_s", values, meta=meta, **base) for kpi, values in samples.items()]


FRAMEWORK_OPEN_LINE = re.compile(
    r"^FRAMEWORK-OPEN tasks=(?P<tasks>[0-9]+) samples_ms=(?P<samples>[0-9.]+(?:,[0-9.]+)*) "
    r"allocations=(?P<allocations>[0-9]+) heap_per_task=(?P<heap>[0-9]+)$")


def framework_open_size(tasks: int) -> str:
    """`5k` for 5000 tasks, `250` for 250: the KPI id's size suffix."""
    return f"{tasks // 1000}k" if tasks >= 1000 and tasks % 1000 == 0 else str(tasks)


def framework_open(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the framework-scaling example's open."""
    samples: dict[str, list[float]] = {}
    units: dict[str, str] = {}
    for log in logs:
        lines = [match for line in log.splitlines() if (match := FRAMEWORK_OPEN_LINE.match(line.strip()))]
        if not lines:
            raise ValueError("no `FRAMEWORK-OPEN` line: did framework-scaling run to the end?")
        for line in lines:
            size = framework_open_size(int(line["tasks"]))
            measured = {
                (f"render.framework_open.ms.{size}", "ms"): [float(value) for value in line["samples"].split(",")],
                (f"render.framework_open.allocations.{size}", "count"): [float(line["allocations"])],
                (f"render.framework_open.heap_per_item.{size}", "bytes"): [float(line["heap"])],
            }
            for (kpi, unit), values in measured.items():
                samples.setdefault(kpi, []).extend(values)
                units[kpi] = unit
    if not logs:
        raise ValueError("no framework-scaling logs")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, units[kpi], values, meta=meta, **base) for kpi, values in samples.items()]


FRAMEWORK_FOCUS_LINE = re.compile(
    r"^FRAMEWORK-FOCUS tasks=(?P<tasks>[0-9]+) add_ms=(?P<add>[0-9.]+(?:,[0-9.]+)*) "
    r"recompute_ms=(?P<recompute>[0-9.]+(?:,[0-9.]+)*) native_ms=(?P<native>[0-9.]+(?:,[0-9.]+)*)$")


def framework_focus(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the framework-scaling example's focus mode."""
    samples: dict[str, list[float]] = {}
    for log in logs:
        lines = [match for line in log.splitlines() if (match := FRAMEWORK_FOCUS_LINE.match(line.strip()))]
        if not lines:
            raise ValueError("no `FRAMEWORK-FOCUS` line: did framework-scaling run to the end?")
        for line in lines:
            size = framework_open_size(int(line["tasks"]))
            for field, kpi in (("add", "add_item_ms"), ("recompute", "recompute_ms"),
                               ("native", "native_recompute_ms")):
                samples.setdefault(f"render.framework_focus.{kpi}.{size}", []).extend(
                    float(value) for value in line[field].split(","))
    if not logs:
        raise ValueError("no framework-scaling logs")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, "ms", values, meta=meta, **base) for kpi, values in samples.items()]


FRAMEWORK_SCROLL_LINE = re.compile(
    r"^FRAMEWORK-SCROLL(?P<kind>-HOSTED|-PEER)? tasks=(?P<tasks>[0-9]+) step=(?P<step>[0-9.]+) copying=(?P<copying>true|false) "
    r"pixels_p50=(?P<pixels>[0-9]+) pixels_max=(?P<max>[0-9]+) copied_p50=(?P<copied>[0-9]+) "
    r"whole=(?P<whole>[0-9]+) frames=(?P<frames>[0-9]+) cpu_ms=(?P<cpu>[0-9.]+) gpu_ms=(?P<gpu>[0-9.]+) "
    r"gpu_full_ms=(?P<gpu_full>[0-9.]+) differing=(?P<differing>[0-9]+)$")


def framework_scroll(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the framework-scaling example's scroll."""
    samples: dict[str, list[float]] = {}
    units: dict[str, str] = {}
    for log in logs:
        lines = [match for line in log.splitlines() if (match := FRAMEWORK_SCROLL_LINE.match(line.strip()))]
        if not lines:
            raise ValueError("no `FRAMEWORK-SCROLL` line: did framework-scaling run to the end?")
        for line in lines:
            if int(line["differing"]) != 0:
                raise ValueError(f"a scroll of {line['tasks']} tasks presented {line['differing']} pixels "
                                 "a full render does not have")
            if line["copying"] != "true" or float(line["step"]) != int(float(line["step"])):
                continue
            size = framework_open_size(int(line["tasks"]))
            if line["kind"]:
                kind = "hosted" if line["kind"] == "-HOSTED" else "peer"
                measured = {
                    (f"render.framework_scroll.{kind}_pixels.{size}", "pixels"): float(line["pixels"]),
                    (f"render.framework_scroll.{kind}_whole_frames.{size}", "count"): float(line["whole"]),
                }
            else:
                measured = {
                    (f"render.framework_scroll.pixels.{size}", "pixels"): float(line["pixels"]),
                    (f"render.framework_scroll.whole_frames.{size}", "count"): float(line["whole"]),
                    (f"render.framework_scroll.cpu_ms.{size}", "ms"): float(line["cpu"]),
                    (f"render.framework_scroll.raster_ms.{size}", "ms"): float(line["gpu"]),
                }
            for (kpi, unit), value in measured.items():
                samples.setdefault(kpi, []).append(value)
                units[kpi] = unit
    if not logs:
        raise ValueError("no framework-scaling logs")
    if not samples:
        raise ValueError("no `FRAMEWORK-SCROLL` line with copies and a whole-pixel step")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, units[kpi], values, meta=meta, **base) for kpi, values in samples.items()]


NOTES_LATENCY_PREFIX = "NOTES-LATENCY "
# The example's workloads, each a pair of KPIs.
NOTES_LATENCY_WORKLOADS = ("type", "enter", "backspace", "scroll", "pointer", "idle", "open")


def notes_latency(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the Notes input-latency example."""
    samples: dict[str, list[float]] = {}
    for log in logs:
        lines = [line.strip()[len(NOTES_LATENCY_PREFIX):] for line in log.splitlines()
                 if line.strip().startswith(NOTES_LATENCY_PREFIX)]
        if len(lines) != 1:
            raise ValueError(f"expected one `NOTES-LATENCY` line per run, found {len(lines)}: "
                             "did notes-latency run to the end?")
        report = json.loads(lines[0])
        for workload in NOTES_LATENCY_WORKLOADS:
            figures = report["workloads"][workload]
            for stage in ("to_frame_ms", "to_raster_ms"):
                if figures.get(stage) is None:
                    raise ValueError(f"notes-latency took no `{stage}` for `{workload}`: no GPU?")
                values = [float(value) for value in figures[stage]["values"]]
                if not values:
                    raise ValueError(f"notes-latency took no `{workload}` samples")
                samples.setdefault(f"render.notes_latency.{workload}.{stage}", []).extend(values)
    if not logs:
        raise ValueError("no notes-latency logs")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, "ms", values, meta=meta, **base) for kpi, values in samples.items()]


FILES_NAME_INDEX_LINE = re.compile(
    r"(?:^|\s)FILES-NAME-INDEX names=(?P<names>[0-9]+) rebuild_ms=(?P<rebuild_ms>[0-9.]+) "
    r"change_ms=(?P<change_ms>[0-9.]+(?:,[0-9.]+)*) unchanged_ms=(?P<unchanged_ms>[0-9.]+(?:,[0-9.]+)*) "
    r"rename_ms=(?P<rename_ms>[0-9.]+) burst64_ms=(?P<burst64_ms>[0-9.]+)$")
# Line field -> KPI metric. Fields listing several values give one sample each.
FILES_NAME_INDEX_KPIS = {"change_ms": "change_cpu_ms", "unchanged_ms": "unchanged_cpu_ms",
                         "rename_ms": "rename_cpu_ms", "burst64_ms": "burst64_cpu_ms",
                         "rebuild_ms": "rebuild_cpu_ms"}


def files_name_index(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the Files search name-index update test."""
    samples: dict[str, list[float]] = {}
    sizes = set()
    for log in logs:
        lines = [match for line in log.splitlines() if (match := FILES_NAME_INDEX_LINE.search(line.rstrip()))]
        if len(lines) != 1:
            raise ValueError(f"expected one `FILES-NAME-INDEX` line per run, found {len(lines)}: "
                             "did the test run with --nocapture?")
        line = lines[0]
        size = framework_open_size(int(line.group("names")))
        sizes.add(size)
        for field, metric in FILES_NAME_INDEX_KPIS.items():
            kpi = f"search.files_name_index.{metric}.{size}"
            samples.setdefault(kpi, []).extend(float(value) for value in line.group(field).split(","))
    if not logs:
        raise ValueError("no Files search name-index logs")
    if len(sizes) != 1:
        raise ValueError(f"runs measured different index sizes: {sorted(sizes)}")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, "ms", values, meta=meta, **base) for kpi, values in samples.items()]


PRICES = Path(__file__).resolve().parent / "prices.toml"
# The relay load bench's per-minute KPIs, as `relay_load` names them.
HOSTING_KPIS = {
    "hosting.relay.cpu_s_per_collab_hour": "s",
    "hosting.relay.rss_mib_per_collab": "MiB",
    "hosting.relay.egress_bytes_per_collab_hour": "bytes",
    "hosting.relay.frame_bytes_per_collab_hour": "bytes",
    "hosting.relay.client_packets_per_collab_hour": "count",
    "hosting.relay.client_data_packets_per_collab_hour": "count",
    "hosting.relay.datagrams_per_collab_hour": "count",
    "hosting.relay.ack_only_datagrams_per_collab_hour": "count",
    "hosting.relay.fanout_ms": "ms",
    "hosting.relay.presence_pointer_ms": "ms",
    "hosting.relay.presence_caret_ms": "ms",
    "hosting.relay.store_fsyncs_per_edit": "count",
    "hosting.relay.store_listed_entries_per_edit": "count",
    "hosting.relay.compaction_cpu_ms_per_collab_hour": "ms",
    "hosting.relay.compaction_signature_checks_per_edit": "count",
}


def collab_hour_usd(provider: dict[str, Any], prices: dict[str, Any], cpu_s: float, rss_mib: float,
                    egress_bytes: float, idle_rss_mib: float) -> tuple[float, float, str]:
    """Dollars per active collaborator-hour on one provider's VM, with the
    collaborators that VM carries and the resource that binds it."""
    target = float(prices["target_utilisation"])
    hours = float(prices["hours_per_month"])
    by_cpu = float(provider["vcpus"]) * 3600.0 * target / cpu_s if cpu_s > 0 else math.inf
    free_mib = float(provider["memory_gib"]) * 1024.0 * target - idle_rss_mib
    if free_mib <= 0:
        raise ValueError(f"the idle relay ({idle_rss_mib:.0f} MiB) does not fit {provider['description']}")
    by_memory = free_mib / rss_mib if rss_mib > 0 else math.inf
    capacity, binding = (by_cpu, "cpu") if by_cpu <= by_memory else (by_memory, "memory")
    if not math.isfinite(capacity):
        raise ValueError("the run measured neither CPU time nor memory per collaborator")
    compute = float(provider["monthly_usd"]) / hours / capacity
    included_gb = float(provider["included_egress_gb_month"]) / (capacity * hours)
    billable_gb = max(0.0, egress_bytes / 1e9 - included_gb)
    return compute + billable_gb * float(provider["egress_usd_per_gb"]), capacity, binding


# What pricing needs: a report without these cannot be priced at all.
HOSTING_PRICED_KPIS = ("hosting.relay.cpu_s_per_collab_hour", "hosting.relay.rss_mib_per_collab",
                       "hosting.relay.egress_bytes_per_collab_hour")


def hosting_load(report: dict[str, Any], prices: dict[str, Any], env: dict[str, Any],
                 require_all: bool = True) -> list[record.Record]:
    """Records for one relay load bench run, priced with `prices`.

    The nightly requires exactly today's KPIs. `require_all=False` is the
    same-job A/B lane (tools/perf/ab.py), which also reads an older bench: it
    records the known KPIs the report has and leaves out any it does not know.
    """
    collaborators = int(report["collaborators"])
    variant = f"n{collaborators}"
    kpis = report.get("kpis", {})
    if require_all and set(kpis) != set(HOSTING_KPIS):
        raise ValueError(f"the bench reported {sorted(kpis)}, not {sorted(HOSTING_KPIS)}")
    missing = [kpi for kpi in HOSTING_PRICED_KPIS if kpi not in kpis]
    if missing:
        raise ValueError(f"the bench reported {sorted(kpis)}, without {missing}")
    present = {kpi: unit for kpi, unit in HOSTING_KPIS.items() if kpi in kpis}
    for kpi, unit in present.items():
        if kpis[kpi]["unit"] != unit or not kpis[kpi]["samples"]:
            raise ValueError(f"{kpi}: expected samples in {unit}")
    meta = {**env.get("meta", {}), "collaborators": str(collaborators), "rooms": str(report["rooms"]),
            "room_size": str(report["room_size"]), "window_s": str(report["window_s"]),
            "edits": str(report["edits"]), "presence": str(report["presence"]),
            "undelivered_edits": str(report["undelivered_edits"]), "prices_checked": str(prices["checked"]),
            # The CPUs the relay was confined to (`relay_load --relay-cpus`), which sizes its
            # runtime: "all" is the runner's, four workers; the priced VM has two vCPUs.
            "relay_cpus": str(report.get("relay_cpus") or "all")}
    base = {name: value for name, value in env.items() if name != "meta"}
    records = [record.from_samples(f"{kpi}.{variant}", unit, kpis[kpi]["samples"], meta=meta, **base)
               for kpi, unit in present.items()]
    minutes = list(zip(kpis["hosting.relay.cpu_s_per_collab_hour"]["samples"],
                       kpis["hosting.relay.rss_mib_per_collab"]["samples"],
                       kpis["hosting.relay.egress_bytes_per_collab_hour"]["samples"], strict=True))
    idle = float(report["relay_idle_rss_mib"])
    for name, provider in sorted(prices["provider"].items()):
        priced = [collab_hour_usd(provider, prices, *minute, idle) for minute in minutes]
        hourly = [usd for usd, _, _ in priced]
        capacity = min(collaborators for _, collaborators, _ in priced)
        bindings = sorted({binding for _, _, binding in priced})
        provider_meta = {**meta, "provider": provider["description"],
                         "collaborators_per_vm": f"{capacity:.0f}", "binding": ",".join(bindings)}
        records.append(record.from_samples(f"hosting.relay.cost_usd_per_collab_hour.{name}.{variant}", "usd",
                                           hourly, meta=provider_meta, **base))
        monthly = [usd * float(prices["active_hours_per_user_month"]) for usd in hourly]
        records.append(record.from_samples(f"hosting.relay.cost_usd_per_user_month.{name}.{variant}", "usd",
                                           monthly, meta=provider_meta, **base))
    return records


# What one `tilo-room-load bench` report becomes: KPI, unit, and how to read it.
ROOM_LOAD_KPIS = {
    "hosting.room.join_p99_ms": ("ms", lambda report: report["join_ms"]["p99"]),
    "hosting.room.chat_p99_ms": ("ms", lambda report: report["chat_ms"]["p99"]),
    "hosting.room.rss_mib_peak": ("MiB", lambda report: report["host"]["rss_mib_peak"]),
    # Cores during the hold, as a share of one CPU.
    "hosting.room.cpu_percent": ("percent", lambda report: report["host"]["hold_cpu_cores"] * 100.0),
}


def room_load(reports: list[dict[str, Any]], env: dict[str, Any]) -> list[record.Record]:
    """Records for repeated room-load bench runs of one configuration."""
    if not reports:
        raise ValueError("no room-load report")
    configs = {json.dumps(report["config"], sort_keys=True) for report in reports}
    if len(configs) != 1:
        raise ValueError(f"the reports ran different configurations: {sorted(configs)}")
    config = reports[0]["config"]
    guests = int(config["guests"])
    for number, report in enumerate(reports, 1):
        failures = {**report.get("failures", {}), **report.get("spectators", {}).get("failures", {})}
        if failures or int(report["joined"]) != guests or int(report["chat_rx"]) == 0:
            raise ValueError(f"run {number}: {report['joined']} of {guests} guests joined, "
                             f"{report['chat_rx']} chat changes delivered, failures {failures}")
    meta = {**env.get("meta", {}), "guests": str(guests), "runs": str(len(reports)),
            "rooms": str(config["rooms"]), "cursor_hz": str(config["cursor_hz"]),
            "chat_every_s": str(config["chat_every_s"]), "host_threads": str(config.get("host_threads") or "all"),
            "machine_cores": str(reports[0]["host"]["machine_cores"])}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(f"{kpi}.g{guests}", unit, [read(report) for report in reports],
                                meta=meta, **base)
            for kpi, (unit, read) in ROOM_LOAD_KPIS.items()]


# The browser runtime qualification's report samples (scripts/perf/browser_qualification.mjs)
# and the KPI each becomes, with its unit.
BROWSER_KPIS = {
    "load_ms": ("browser.load.desktop_presented", "ms"),
    "open_ms": ("browser.open.first_frame", "ms"),
    "open_ready_ms": ("browser.open.ready", "ms"),
    "activate_ms": ("browser.activate.first_frame", "ms"),
    "demo_interval_ms": ("browser.demo.frame_interval", "ms"),
    "demo_work_ms": ("browser.demo.frame_work", "ms"),
    "idle_per_s": ("browser.idle.presented_per_s", "per_s"),
}
# Reported only by a run with the demo workload (`--demo-seconds` above 0).
BROWSER_DEMO_FIELDS = frozenset({"demo_interval_ms", "demo_work_ms"})


def browser_qualification(report: dict[str, Any], env: dict[str, Any],
                          require_clean: bool = True) -> list[record.Record]:
    """Records for one browser runtime qualification report.

    The report must have measured the environment the records claim: its
    WebGPU adapter is the env's renderer, its bundle a build of the env's
    commit in the env's profile, clean unless `require_clean=False` (the A/B
    lane, whose output directory sits untracked in the checkout it builds),
    and its page CPU unthrottled: a throttled run stands in for older
    hardware and is never this series. The demo's KPIs come from a run with
    the demo and only from one.
    """
    if report.get("schema") != 1:
        raise ValueError(f"browser qualification report schema {report.get('schema')!r}, not 1")
    bundle, workload = report["bundle"], report["workload"]
    if report["renderer"] != env["renderer"]:
        raise ValueError(f"the report measured on {report['renderer']}, not the env's {env['renderer']}")
    if bundle["profile"] != env["profile"]:
        raise ValueError(f"the bundle is a {bundle['profile']} build, not the env's {env['profile']}")
    if bundle["revision"] != env["commit"] or (require_clean and bundle["dirty"]):
        raise ValueError(f"the bundle was built from {bundle['revision']}"
                         f"{' (dirty)' if bundle['dirty'] else ''}, not {env['commit']}")
    if workload.get("cpuThrottle", 1) != 1:
        raise ValueError(f"the page's CPU was throttled {workload['cpuThrottle']}x")
    samples = report["samples"]
    demo = float(workload.get("demoSeconds", 0)) > 0
    fields = [field for field in BROWSER_KPIS if demo or field not in BROWSER_DEMO_FIELDS]
    missing = [field for field in fields if not samples.get(field)]
    if missing:
        raise ValueError(f"the report has no samples for {missing}")
    if not demo and any(samples.get(field) for field in BROWSER_DEMO_FIELDS):
        raise ValueError("the report has demo samples from a run without the demo")
    adapter = report.get("adapter") or {}
    meta = {**env.get("meta", {}), "bundle_digest": bundle["digest"], "runtime_sha256": bundle["runtimeSha256"],
            "browser": report["browser"]["product"],
            "adapter": " ".join(str(value) for value in adapter.values() if value) or "unknown",
            "viewport": "{width}x{height}@{deviceScaleFactor}".format(**report["viewport"]),
            "rounds": str(workload["rounds"]), "apps": ",".join(workload["apps"]),
            "input_hz": str(workload["inputHz"])}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(*BROWSER_KPIS[field], samples[field], meta=meta, **base) for field in fields]


STREAM_KPIS = {
    "startup_ms": ("stream.startup.first_picture", "ms"),
    "frame_latency_ms": ("stream.frame.latency", "ms"),
    "frame_interval_ms": ("stream.frame.interval", "ms"),
    "encode_ms": ("stream.encode.frame", "ms"),
    "input_ms": ("stream.input.latency", "ms"),
}
# The codec whose series carries no codec in its ids: H.264's series predate
# the others and keep their ids, so their baselines continue.
STREAM_DEFAULT_CODEC = "h264"


def stream_kpi(kpi: str, codec: str) -> str:
    """`kpi` for a stream in `codec`: `stream.vp9.frame.latency` for VP9's
    `stream.frame.latency`, unchanged for H.264's."""
    if codec == STREAM_DEFAULT_CODEC:
        return kpi
    group, rest = kpi.split(".", 1)
    return f"{group}.{codec}.{rest}"


def native_stream(report: dict[str, Any], env: dict[str, Any]) -> list[record.Record]:
    """Records for one native stream qualification report.

    Like the browser qualification's, the report must have measured the
    environment the records claim: the env's WebGPU renderer and a clean
    build of the env's commit in its profile. A run the harness judged
    incorrect (a decoder error, a corrupt or stale picture, a lost press) is
    refused: its numbers measure a broken stream.
    """
    if report.get("schema") != 1:
        raise ValueError(f"native stream report schema {report.get('schema')!r}, not 1")
    bundle, session = report["bundle"], report["session"]
    if report["renderer"] != env["renderer"]:
        raise ValueError(f"the report measured on {report['renderer']}, not the env's {env['renderer']}")
    if bundle["profile"] != env["profile"]:
        raise ValueError(f"the bundle is a {bundle['profile']} build, not the env's {env['profile']}")
    if bundle["revision"] != env["commit"] or bundle["dirty"]:
        raise ValueError(f"the bundle was built from {bundle['revision']}"
                         f"{' (dirty)' if bundle['dirty'] else ''}, not {env['commit']}")
    failures = report["correctness"]["failures"]
    if failures:
        raise ValueError(f"the stream was incorrect: {'; '.join(failures)}")
    codec = report["workload"]["codec"]
    if not re.fullmatch(r"[a-z][a-z0-9]*", codec):
        raise ValueError(f"the report names codec {codec!r}")
    if session["codec"] != codec:
        raise ValueError(f"the session streamed {session['codec']}, not the requested {codec}")
    # Safari has no WebTransport, so a run that names it took the HTTPS
    # stream: a record claiming otherwise measured another browser.
    if report.get("browser", {}).get("driver") == "webdriver" and report["workload"]["carrier"] != "https":
        raise ValueError(f"the Safari run streamed over {report['workload']['carrier']}, not the HTTPS stream Safari takes")
    samples = report["samples"]
    missing = [field for field in STREAM_KPIS if not samples.get(field)]
    if missing:
        raise ValueError(f"the report has no samples for {missing}")
    counts = report["counts"]
    delivered = float(counts["deliveredPercent"])
    if not 0.0 <= delivered <= 100.0:
        raise ValueError(f"delivered {delivered} % is not a share")
    meta = {**env.get("meta", {}), "bundle_digest": bundle["digest"], "browser": report["browser"]["product"],
            "encoder": session["encoder"], "encoder_hardware": str(bool(session["hardware"])).lower(),
            "window": f"{session['width']}x{session['height']}", "fps": str(session["fps"]),
            "carrier": report["workload"]["carrier"], "codec": codec,
            "viewport": "{width}x{height}@{deviceScaleFactor}".format(**report["viewport"]),
            "frames_sent": str(counts["sent"])}
    base = {name: value for name, value in env.items() if name != "meta"}
    records = [record.from_samples(stream_kpi(kpi, codec), unit, samples[field], meta=meta, **base)
               for field, (kpi, unit) in STREAM_KPIS.items()]
    records.append(record.from_samples(stream_kpi("stream.frame.missed", codec), "percent",
                                       [round(100.0 - delivered, 2)], meta=meta, **base))
    return records


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    gate = commands.add_parser("fs-gate", help="records from tilo-fs performance gate logs")
    gate.add_argument("--env-json", required=True, type=Path)
    gate.add_argument("logs", nargs="+", type=Path)
    sizes = commands.add_parser("tapp-sizes", help="records from the registry entries index")
    sizes.add_argument("--env-json", required=True, type=Path)
    sizes.add_argument("entries", type=Path)
    runtime = commands.add_parser("runtime-sizes", help="records from the published runtime's measurements")
    runtime.add_argument("--env-json", required=True, type=Path)
    runtime.add_argument("measurements", type=Path)
    switch = commands.add_parser("workspace-switch", help="records from WORKSPACE-SWITCH session-log lines")
    switch.add_argument("--env-json", required=True, type=Path)
    switch.add_argument("logs", nargs="+", type=Path)
    idle = commands.add_parser("idle-wakeups", help="records from idle-wakeup gate logs")
    idle.add_argument("--env-json", required=True, type=Path)
    idle.add_argument("logs", nargs="+", type=Path)
    names = commands.add_parser("files-name-index", help="records from FILES-NAME-INDEX test lines")
    names.add_argument("--env-json", required=True, type=Path)
    names.add_argument("logs", nargs="+", type=Path)
    opening = commands.add_parser("framework-open", help="records from framework-scaling's FRAMEWORK-OPEN lines")
    opening.add_argument("--env-json", required=True, type=Path)
    opening.add_argument("logs", nargs="+", type=Path)
    scrolling = commands.add_parser("framework-scroll",
                                    help="records from framework-scaling's FRAMEWORK-SCROLL lines")
    scrolling.add_argument("--env-json", required=True, type=Path)
    scrolling.add_argument("logs", nargs="+", type=Path)
    focusing = commands.add_parser("framework-focus",
                                   help="records from framework-scaling's FRAMEWORK-FOCUS lines")
    focusing.add_argument("--env-json", required=True, type=Path)
    focusing.add_argument("logs", nargs="+", type=Path)
    notes = commands.add_parser("notes-latency", help="records from notes-latency's NOTES-LATENCY line")
    notes.add_argument("--env-json", required=True, type=Path)
    notes.add_argument("logs", nargs="+", type=Path)
    hosting = commands.add_parser("hosting-load", help="records from the relay load bench's hosting-load.json")
    hosting.add_argument("--env-json", required=True, type=Path)
    hosting.add_argument("--prices", type=Path, default=PRICES)
    hosting.add_argument("report", type=Path)
    rooms = commands.add_parser("room-load", help="records from tilo-room-load bench reports")
    rooms.add_argument("--env-json", required=True, type=Path)
    rooms.add_argument("reports", nargs="+", type=Path)
    browser = commands.add_parser("browser-qualification",
                                  help="records from scripts/perf/browser_qualification.mjs's report")
    browser.add_argument("--env-json", required=True, type=Path)
    browser.add_argument("report", type=Path)
    stream = commands.add_parser("native-stream",
                                 help="records from scripts/perf/native_stream_qualification.mjs's report")
    stream.add_argument("--env-json", required=True, type=Path)
    stream.add_argument("report", type=Path)
    args = parser.parse_args(argv)
    try:
        env = record.load_env(args.env_json)
        if args.command == "browser-qualification":
            records = browser_qualification(json.loads(args.report.read_text(encoding="utf-8")), env)
        elif args.command == "native-stream":
            records = native_stream(json.loads(args.report.read_text(encoding="utf-8")), env)
        elif args.command == "hosting-load":
            records = hosting_load(json.loads(args.report.read_text(encoding="utf-8")),
                                   tomllib.loads(args.prices.read_text(encoding="utf-8")), env)
            for item in records:
                if item.unit == "usd":
                    print(f"{item.kpi}: p50 ${item.p50:.6f}, max ${item.max:.6f} "
                          f"({item.meta['collaborators_per_vm']} collaborators per VM, "
                          f"bound by {item.meta['binding']})", file=sys.stderr)
        elif args.command == "room-load":
            records = room_load([json.loads(path.read_text(encoding="utf-8")) for path in args.reports], env)
        elif args.command == "fs-gate":
            records = fs_gate([path.read_text(encoding="utf-8", errors="replace") for path in args.logs], env)
        elif args.command == "workspace-switch":
            samples = [sample for path in args.logs
                       for sample in switch_samples(path.read_text(encoding="utf-8", errors="replace"))]
            records = workspace_switch(samples, env)
        elif args.command == "runtime-sizes":
            records = runtime_sizes(json.loads(args.measurements.read_text(encoding="utf-8")), env)
        elif args.command == "framework-open":
            records = framework_open([path.read_text(encoding="utf-8", errors="replace") for path in args.logs],
                                     env)
        elif args.command == "framework-scroll":
            records = framework_scroll([path.read_text(encoding="utf-8", errors="replace") for path in args.logs],
                                       env)
        elif args.command == "framework-focus":
            records = framework_focus([path.read_text(encoding="utf-8", errors="replace") for path in args.logs],
                                      env)
        elif args.command == "notes-latency":
            records = notes_latency([path.read_text(encoding="utf-8", errors="replace") for path in args.logs],
                                    env)
        elif args.command == "idle-wakeups":
            records = idle_wakeups([path.read_text(encoding="utf-8", errors="replace") for path in args.logs], env)
        elif args.command == "files-name-index":
            records = files_name_index([path.read_text(encoding="utf-8", errors="replace") for path in args.logs],
                                       env)
        else:
            records = tapp_sizes(json.loads(args.entries.read_text(encoding="utf-8")), env,
                                 tapp_size_ratchet.load().apps())
    except (ValueError, KeyError) as error:
        print(f"adapters.py: {error}", file=sys.stderr)
        return 1
    for item in records:
        print(item.to_json())
    return 0


if __name__ == "__main__":
    sys.exit(main())
