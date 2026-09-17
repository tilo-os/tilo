"""Summarize real app runtime benchmarks; all distributions use nearest-rank percentiles."""
import hashlib
import math
import platform
import re
import statistics
import subprocess
import json
from collections import Counter


def distribution(values, scale=1e-6):
    """Input defaults to nanoseconds; emit milliseconds. Never invent empty results."""
    if not values:
        raise ValueError("No timing samples were recorded")
    ordered = sorted(value * scale for value in values)
    return {"n": len(ordered), "min_ms": ordered[0], "mean_ms": statistics.mean(ordered),
            **{f"p{p}_ms": ordered[math.ceil(len(ordered) * p / 100) - 1] for p in [50, 95, 99]},
            "max_ms": ordered[-1]}


def measured_rows(rows, start, end):
    if not 0 <= start <= end <= len(rows):
        raise ValueError("Missing/truncated timing evidence or invalid measurement window")
    return rows[start:end]


def present_rows(lines):
    pattern = re.compile(r"present-frame \d+: acquire=(\d+)us encode=(\d+)us submit=(\d+)us present=(\d+)us")
    return [dict(zip(["acquire_us", "encode_us", "submit_us", "present_us"], map(int, match.groups())))
            for line in lines if (match := pattern.search(line))]


def worker_summary(lines):
    prefix = "qos-render-frame "
    rows = [json.loads(line[len(prefix):]) for line in lines if line.startswith(prefix)]
    if not rows:
        raise ValueError("No worker outcome samples; rebuild with frame-outcome instrumentation")
    if any(row.get("total_us") is None or row.get("outcome") == "SurfaceGone" for row in rows):
        raise ValueError("Render surface failed during the measurement window")
    counts = Counter(row["outcome"] for row in rows)
    presented = [row for row in rows if row["outcome"] == "Presented"]
    return {"outcomes": dict(counts), "skipped_fraction": (counts["Skipped"] + counts["Occluded"]) / len(rows),
            "occluded_fraction": counts["Occluded"] / len(rows),
            "worker_wall": distribution([row["total_us"] for row in rows], scale=0.001),
            "presented_worker_wall": distribution([row["total_us"] for row in presented], scale=0.001)}


def binary_hashes(bin_dir):
    result = {}
    for binary in ["examples/shared-runtime-proof", "photos", "word-processor"]:
        with (bin_dir / binary).open("rb") as stream:
            result[binary] = hashlib.file_digest(stream, "sha256").hexdigest()
    return result


def resource_summary(lines):
    prefix = "qos-resource-stage "
    groups = {}
    for line in lines:
        if not line.startswith(prefix):
            continue
        row = json.loads(line[len(prefix):])
        duration = row.get("duration_us")
        if type(duration) is not int or duration < 0:
            raise ValueError("Invalid resource timing sample")
        groups.setdefault(row["stage"], {}).setdefault(row["cache_result"], []).append(duration)
    if not groups:
        raise ValueError("No resource-stage diagnostics; enable QOS_PROFILE_RESOURCES=1")
    return {stage: {result: distribution(values, scale=0.001)
                    for result, values in outcomes.items()} for stage, outcomes in groups.items()}


def correlate_frames(frames, lines):
    """Correlate identities only: worker and CPU timestamps have different origins."""
    cpu_ids = [row["frame"] for row in frames]
    if any(type(identity) is not int or identity < 0 for identity in cpu_ids):
        raise ValueError("Invalid CPU frame identity")
    if len(set(cpu_ids)) != len(cpu_ids):
        raise ValueError("Duplicate CPU frame identity")
    worker = {}
    unprofiled = 0
    for line in lines:
        if not line.startswith("qos-render-frame "):
            continue
        row = json.loads(line[len("qos-render-frame "):])
        if "cpu_frame_id" not in row:
            raise ValueError("Missing CPU/worker correlation; rebuild instrumented apps")
        identity = row["cpu_frame_id"]
        if identity is None:
            unprofiled += 1
            continue
        if type(identity) is not int or identity < 0 or identity in worker:
            raise ValueError("Invalid or duplicate worker CPU frame identity")
        worker[identity] = row["outcome"]
    cpu = set(cpu_ids)
    return {"matched_outcomes": dict(Counter(worker[i] for i in cpu & worker.keys())),
            "cpu_frames_without_worker_outcome": sorted(cpu - worker.keys()),
            "worker_frames_without_cpu_sample": sorted(worker.keys() - cpu),
            "unprofiled_worker_frames": unprofiled}


def report(output, events, bin_dir, hashes):
    import json
    if sum(e.get("event") == "benchmark_passed" for e in events) != 1:
        raise ValueError("Benchmark did not finish")
    config = next(e for e in events if e.get("event") == "benchmark_config")
    markers = {e["event"]: e["boundaries"] for e in events
               if e.get("phase") == "measure" and e["event"] in ["phase_start", "phase_end"]}
    result = {"system": platform.platform(), "architecture": platform.machine(),
              "binary_directory": str(bin_dir.resolve()), "binary_sha256_before_launch": hashes, "config": config,
              "limitations": ["Instrumented run; profiling and log writes add overhead.",
                              "CPU frame build and render-thread stages are separate, overlapping timelines; do not add their percentiles.",
                              "Present call is not GPU completion, display scanout, input-to-photon latency, or FPS.",
                              "No model inference; action latency includes local broker dispatch and app completion.",
                              "Window covers both workloads plus pacing and 500ms tail; no action-to-frame correlation.",
                              "Word Processor document grows by one image per insertion; original Photos blob is reused.",
                              "Startup is a single observation with uncontrolled filesystem cache state; surface discovery is polled and sequential."],
              "runtime_startup_ms": next(e["elapsed_ns"] / 1e6 for e in events if e.get("event") == "runtime_ready"),
              "apps": {}}
    if binary_hashes(bin_dir) != hashes:
        raise ValueError("Binaries changed during measurement; rerun after builds finish")
    if platform.system() == "Darwin":
        result["cpu"] = subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
        result["memory_bytes"] = int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True))
        display_info = json.loads(subprocess.check_output(
            ["system_profiler", "SPDisplaysDataType", "-json"], text=True, timeout=30))
        result["display_configuration_reported_after_run"] = [
            {"name": display.get("_name"), "pixels": display.get("_spdisplays_pixels"),
             "logical_mode": display.get("_spdisplays_resolution"),
             "online": display.get("spdisplays_online")}
            for gpu in display_info.get("SPDisplaysDataType", [])
            for display in gpu.get("spdisplays_ndrvs", [])]
    for app in ["dev.qos.photos", "dev.qos.word-processor"]:
        samples = [e for e in events if e.get("event") == "action_sample" and e["phase"] == "measure" and e["app"] == app]
        if len(samples) != config["samples_per_app"] or not all(e["succeeded"] for e in samples):
            raise ValueError(f"Incomplete/failed action samples for {app}")
        selected = {}
        for suffix in ["frames.jsonl", "log"]:
            selected[suffix] = measured_rows((output / f"{app}.{suffix}").read_text().splitlines(),
                markers["phase_start"][app][suffix], markers["phase_end"][app][suffix])
        frames = [json.loads(line) for line in selected["frames.jsonl"]]
        render = present_rows(selected["log"])
        if not frames or not render:
            raise ValueError(f"{app}: missing CPU/presented-frame samples; keep both windows visible and rerun")
        result["apps"][app] = {
            "worker": worker_summary(selected["log"]),
            "action_surface_observed_ms": next(e["elapsed_ns"] / 1e6 for e in events if e.get("event") == "action_surface_ready" and e["app"] == app),
            "action_latency": distribution([e["elapsed_ns"] for e in samples]),
            "broker_start_return": distribution([e["accepted_ns"] for e in samples]),
            "surface_lookup": distribution([e["discovery_ns"] for e in samples]),
            "cpu_frame_build": distribution([e["total_us"] for e in frames], scale=0.001),
            "cpu_frame_build_above_16_667ms": sum(e["total_us"] > 16667 for e in frames),
            "cpu_frame_passes": {key: distribution([e[key] for e in frames], scale=0.001)
                                for key in ["sync_us", "build_layout_us", "apply_layout_us", "draw_us", "handle_event_us",
                                            "resource_availability_us", "scene_pool_wait_us", "presence_overlay_us"]},
            "render_thread_stages": {key: distribution([e[key] for e in render], scale=0.001)
                                     for key in ["acquire_us", "encode_us", "submit_us", "present_us"]}}
        if config.get("profile_resources"):
            result["apps"][app]["resource_stages"] = resource_summary(selected["log"])
        if config.get("cpu_worker_correlation"):
            result["apps"][app]["frame_correlation"] = correlate_frames(frames, selected["log"])
    return result
