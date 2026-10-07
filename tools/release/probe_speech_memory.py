#!/usr/bin/env python3
"""Observe a confined public FLEURS decoder child; never opens a microphone."""
import argparse
import json
import os
import resource
from pathlib import Path
import selectors
import shutil
import subprocess
import tempfile
import time

MODELS = {
    "base": "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    "tiny": "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    "small-q5_1": "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
}
COUNTERS = ("VmPeak", "VmSize", "VmHWM", "VmRSS", "Threads")


def observe(command, environment, identity, run, model, case, allocator):
    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    process = subprocess.Popen(
        command, env=environment, **identity, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    peak, phases, phase, buffer = {}, {}, "loading", b""
    deadline = time.monotonic() + 300
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while process.poll() is None:
                if time.monotonic() >= deadline:
                    raise TimeoutError("public memory probe exceeded 300 seconds")
                try:
                    status = Path(f"/proc/{process.pid}/status").read_text()
                except FileNotFoundError:
                    # A completed child may disappear between poll and read.
                    continue
                for line in status.splitlines():
                    key, _, value = line.partition(":")
                    if key in COUNTERS:
                        count = int(value.split()[0])
                        peak[key] = max(peak.get(key, 0), count)
                        target = phases.setdefault(phase, {})
                        target[key] = max(target.get(key, 0), count)
                for key, _ in selector.select(.02):
                    buffer += os.read(key.fd, 65536)
                    while b"\n" in buffer:
                        line, buffer = buffer.split(b"\n", 1)
                        print(line.decode(), flush=True)
                        if line.startswith(b"SPEECH_BENCHMARK_PHASE "):
                            phase = json.loads(line.split(b" ", 1)[1])["phase"]
            remainder = buffer + process.stdout.read()
            if remainder:
                print(remainder.decode(), end="", flush=True)
        if not all(key in peak for key in COUNTERS):
            raise RuntimeError("could not observe the decoder memory counters")
        result = process.wait()
        usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
        wall_seconds = time.monotonic() - started
        cpu_user = usage_after.ru_utime - usage_before.ru_utime
        cpu_system = usage_after.ru_stime - usage_before.ru_stime
        print("SPEECH_MEMORY_PROBE " + json.dumps({
            "run": run, "case": case, "model": model, "exit": result,
            "last_phase": phase, "peak": peak, "phase_peaks": phases,
            "sample_interval_ms": 20, "memory_limit_bytes": 1073741824,
            "allocator": allocator, "memory_values_unit": "KiB except Threads",
            "wall_seconds": wall_seconds, "cpu_user_seconds": cpu_user,
            "cpu_system_seconds": cpu_system,
            "cpu_accounting": "RUSAGE_CHILDREN delta; one serial owned child",
            "allowed_cpu_count": len(os.sched_getaffinity(0)),
        }), flush=True)
        return result == 0
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--benchmark", type=Path, required=True)
    parser.add_argument("--model", choices=MODELS, default="small-q5_1")
    parser.add_argument("--case", default="ar_eg-1")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--allocator", choices=("default", "arena-1", "mmap-128k"),
                        default="default")
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10 or args.manifest.stat().st_size > 1024 * 1024:
        parser.error("invalid bounded public probe arguments")
    manifest = json.loads(args.manifest.read_text())
    if (manifest.get("dataset") != "google/fleurs"
            or manifest.get("revision") != "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
            or len(manifest.get("cases", [])) != 33):
        parser.error("unexpected public corpus receipt")
    model = manifest["models"][args.model]
    if model["sha256"] != MODELS[args.model]:
        parser.error("unexpected reviewed model receipt")
    cases = [case for case in manifest["cases"] if case["id"] == args.case]
    if len(cases) != 1 or not cases[0]["primary"]:
        parser.error("one public short primary clip is required")
    names = (model["file"], cases[0]["file"])
    if any(Path(name).name != name for name in names):
        parser.error("public fixture names must be plain filenames")
    for receipt in (model, cases[0]):
        size = receipt["size"]
        if (not isinstance(size, int) or not 0 < size <= 256 * 1024 * 1024
                or (args.manifest.parent / receipt["file"]).stat().st_size != size):
            parser.error("public fixture size exceeds its bounded receipt")
    benchmark = args.benchmark.resolve(strict=True)
    environment = {"LANG": "C.UTF-8"}
    if args.allocator == "arena-1":
        environment["MALLOC_ARENA_MAX"] = "1"
    elif args.allocator == "mmap-128k":
        environment["MALLOC_MMAP_THRESHOLD_"] = "131072"
    identity = {"user": 65534, "group": 65534, "extra_groups": []} if os.geteuid() == 0 else {}
    success = True
    with tempfile.TemporaryDirectory(prefix="tilo-public-speech-memory-") as directory:
        root = Path(directory)
        for name in ("manifest.json", *names):
            source = args.manifest if name == "manifest.json" else args.manifest.parent / name
            destination = root / name
            shutil.copyfile(source, destination)
            destination.chmod(0o600)
            if identity:
                os.chown(destination, 65534, 65534)
        if identity:
            os.chown(root, 65534, 65534)
        command = [str(benchmark), "--manifest", str(root / "manifest.json"),
                   "--model", args.model, "--single-case", args.case]
        for run in range(args.repeats):
            # Explicit independent diagnostic probes; never production retries.
            passed = observe(command, environment, identity, run, model["sha256"],
                             args.case, args.allocator)
            success = passed and success
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
