#!/usr/bin/env python3
"""One diagnostic: identical full browser probes in default/candidate/candidate/default order.

Nothing here changes the required SDK lane or its browser driver. Each sweep
owns fresh browser profiles and uses the production driver, bytes and probes.
"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import threading
import time

CANDIDATES = {
    "2": ["--js-flags=--wasm-num-compilation-tasks=2"],
    # Fits Chrome's signed integer preference. All cache files stay inside
    # the existing driver's fresh profiles and are removed by its supervisor.
    "cache-2gb": ["--disk-cache-size=2000000000"],
}


def write_wrapper(path: Path) -> None:
    modes = {"default": [], **CANDIDATES}
    path.write_text("#!/usr/bin/env python3\nimport os,sys\n"
                    f"modes={modes!r}\n"
                    "args=modes[os.environ['TRIAL_TASKS']]\n"
                    "os.execv(os.environ['TRIAL_REAL_CHROME'],[os.environ['TRIAL_REAL_CHROME'],*args,*sys.argv[1:]])\n")
    path.chmod(0o755)


def counters(root: Path, name: str) -> dict[str, int]:
    return {key: int(value) for key, value in
            (line.split() for line in (root / name).read_text().splitlines())}


def delta(before: dict, after: dict) -> dict:
    result = {key: value - before[key] for key, value in after.items()}
    if any(value < 0 for value in result.values()):
        raise ValueError("cgroup counters decreased")
    return result


def probe_result(line: str) -> dict | None:
    if not line.startswith('{"app":'):
        return None
    value = json.loads(line)
    return {"app": value["app"],
            "presented": value["status"].startswith("presented"),
            "exception": any(item.startswith("EXC") or "panic" in item.lower()
                             for item in value["log"])}


def cache_sizes(scratch: Path) -> dict | None:
    directories = list(scratch.glob("smoke-browsers.*/[012]/Default/Code Cache/wasm"))
    if not directories:
        return None
    sizes = []
    for directory in directories:
        for path in directory.rglob("*"):
            try:
                if path.is_file():
                    sizes.append(path.stat().st_size)
            except FileNotFoundError:
                # Chrome may replace an entry between traversal and stat.
                continue
    return {"directories": len(directories), "bytes": sum(sizes),
            "largest_file_bytes": max(sizes, default=0)}


def sweep(output: Path, apps: list[str], mode: str, root: Path) -> dict:
    output.mkdir()
    shots = output / "shots"
    shots.mkdir()
    scratch = output / "scratch"
    scratch.mkdir()
    before_cpu, before_events = counters(root, "cpu.stat"), counters(root, "memory.events")
    sampled_peak = [int((root / "memory.current").read_text())]
    swap_peak = [int((root / "memory.swap.current").read_text())]
    cache_peak = [None]
    failures = []
    stop = threading.Event()

    def sample():
        cache_sample_at = 0
        try:
            while not stop.wait(.25):
                sampled_peak[0] = max(sampled_peak[0], int((root / "memory.current").read_text()))
                swap_peak[0] = max(swap_peak[0], int((root / "memory.swap.current").read_text()))
                if time.monotonic() >= cache_sample_at:
                    observed = cache_sizes(scratch)
                    if observed and (cache_peak[0] is None or observed["bytes"] > cache_peak[0]["bytes"]):
                        cache_peak[0] = observed
                    cache_sample_at = time.monotonic() + 2
        except Exception as error:
            failures.append(type(error).__name__)

    environment = dict(os.environ, CHROME=str(output.parent / "chrome-wrapper"),
                       TRIAL_REAL_CHROME=os.environ["CHROME"], TRIAL_TASKS=mode,
                       TILO_WEB_SMOKE_SHARDS="3", SHOTS_DIR=str(shots), RUNNER_TEMP=str(scratch))
    sampler = threading.Thread(target=sample)
    sampler.start()
    started = time.monotonic()
    probes = []
    flag_rejected = False
    # Keep only bounded public diagnostic output; continue draining the pipe
    # after the log cap so a verbose browser does not change probe execution.
    written = 0
    process = None
    try:
        with (output / "browser.log").open("w") as log:
            process = subprocess.Popen(
                ["node", "crates/browser/tilo-web-desktop/scripts/smoke.mjs",
                 "http://127.0.0.1:7878", *apps], env=environment,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            for line in process.stdout:
                if written + len(line.encode()) <= 512 * 1024:
                    log.write(line)
                    written += len(line.encode())
                probe = probe_result(line)
                if probe:
                    probes.append(probe)
                if re.search(r"unrecognized (?:JS )?flag|bad option", line, re.I):
                    flag_rejected = True
            status = process.wait()
    except BaseException:
        if process is not None and process.poll() is None:
            # Forward through the existing driver's signal/cleanup path; own
            # only this process, and wait before relinquishing its profiles.
            process.terminate()
            process.wait(timeout=60)
        raise
    finally:
        if process is not None and process.stdout is not None:
            process.stdout.close()
        stop.set()
        sampler.join()
    after_cpu, after_events = counters(root, "cpu.stat"), counters(root, "memory.events")
    elapsed = round(time.monotonic() - started, 3)
    complete = Counter(item["app"] for item in probes) == Counter(apps)
    valid = complete and all(item["presented"] and not item["exception"] for item in probes)
    result = {"mode": mode, "elapsed_seconds": elapsed, "exit": status,
              "expected_apps": apps, "probes": probes, "coverage_valid": valid,
              "flag_rejected": flag_rejected, "sampling_errors": failures,
              "memory_current_sampled_peak_bytes": sampled_peak[0],
              "swap_current_sampled_peak_bytes": swap_peak[0],
              "wasm_code_cache_sampled_peak": cache_peak[0],
              "cgroup_memory_cumulative_peak_bytes": int((root / "memory.peak").read_text()),
              "cpu_delta": delta(before_cpu, after_cpu),
              "memory_events_delta": delta(before_events, after_events)}
    (output / "receipt.json").write_text(json.dumps(result, indent=2) + "\n")
    print("COMPILATION_TRIAL " + json.dumps(result), flush=True)
    if (status or not valid or flag_rejected or failures or swap_peak[0]
            or result["memory_events_delta"]["oom"]
            or result["memory_events_delta"]["oom_kill"]):
        raise RuntimeError("browser sweep did not qualify; receipt preserves the failure")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--candidate", choices=CANDIDATES, default="2")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir()
    disk_free = shutil.disk_usage(output).free
    if args.candidate == "cache-2gb" and disk_free < 24 * 1024**3:
        raise ValueError("cache trial requires 24 GiB of admitted job scratch headroom")
    root = Path("/sys/fs/cgroup")
    quota = (root / "cpu.max").read_text().strip()
    memory = int((root / "memory.max").read_text())
    if quota != "600000 100000" or memory != 22 * 1024**3:
        raise ValueError("trial requires the unchanged six-CPU/22-GiB general envelope")
    apps = [item["id"] for item in json.loads((args.bundle / "apps.json").read_text())["applications"]]
    apps.append("desktop")
    if len(apps) != len(set(apps)) or len(apps) < 2:
        raise ValueError("invalid app sweep")
    module = args.bundle / "pkg/tilo_browser_runtime_bg.wasm"
    with module.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    identity = {"schema": 2, "source": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                "candidate": args.candidate, "candidate_flags": CANDIDATES[args.candidate],
                "disk_free_bytes": disk_free,
                "wasm_sha256": digest, "wasm_bytes": module.stat().st_size,
                "chrome": subprocess.check_output([os.environ["CHROME"], "--version"], text=True).strip(),
                "cpu_max": quota, "memory_max": memory,
                "affinity_cpus": len(os.sched_getaffinity(0)), "cpu_count": os.cpu_count(),
                "cpuset": (root / "cpuset.cpus.effective").read_text().strip()}
    (output / "identity.json").write_text(json.dumps(identity, indent=2) + "\n")
    print("COMPILATION_IDENTITY " + json.dumps(identity), flush=True)
    wrapper = output / "chrome-wrapper"
    write_wrapper(wrapper)
    results = [sweep(output / f"sweep-{index}", apps, mode, root)
               for index, mode in enumerate(("default", args.candidate, args.candidate, "default"), 1)]
    with module.open("rb") as stream:
        if hashlib.file_digest(stream, "sha256").hexdigest() != digest:
            raise ValueError("WASM bytes changed during comparison")
    (output / "comparison.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
