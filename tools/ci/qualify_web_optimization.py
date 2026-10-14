"""Compare serial and bounded parallel optimization on one exact release bundle.

Run through the existing remote builder with its six-CPU/22-GiB job limits.
Retains structured timings and hashes, never raw compiler or browser logs.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time


def checked_source(root):
    source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    dirty = subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=root, text=True)
    if not re.fullmatch(r"[0-9a-f]{40}", source) or dirty:
        raise ValueError("qualification requires an exact clean committed source")
    return source


def execute(root, command, environment):
    phases, finished = [], []
    started = time.monotonic()
    with subprocess.Popen(command, cwd=root, env=environment, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, text=True) as process:
        for line in process.stdout:
            match = re.search(r"TILO_BUILD_TIMING phase=([a-z0-9_-]+) elapsed_seconds=([0-9.]+)", line)
            if match:
                phases.append({"phase": match[1], "seconds": float(match[2])})
                print(match[0], flush=True)
            match = re.search(r"Finished `([a-z-]+)` profile.*target\(s\) in ([0-9a-z. ]+)", line)
            if match:
                finished.append({"profile": match[1], "duration": match[2].strip()})
        status = process.wait()
    record = {"command": command, "status": status, "wall_seconds": time.monotonic() - started,
              "phases": phases[-16:], "cargo_finishes": finished[-16:]}
    if status:
        raise RuntimeError(json.dumps(record))
    return record


def module_hashes(bundle):
    files = sorted((bundle / "pkg").rglob("*"))
    hashes = {str(path.relative_to(bundle)): hashlib.sha256(path.read_bytes()).hexdigest()
              for path in files if path.is_file() and path.suffix in (".wasm", ".js", ".mjs")}
    if len([name for name in hashes if name.endswith(".wasm")]) < 2:
        raise ValueError("qualification requires the real split desktop modules")
    return hashes


def qualify(root, output):
    source = checked_source(root)
    output.mkdir(parents=True, exist_ok=False)
    bundle = root / "target/tilo-app/web/calculator/bundle"
    environment = {**os.environ, "BINARYEN_CORES": "3"}
    execute(root, ["cargo", "build", "--locked", "-p", "tilo-app-cli", "--bin", "tilo-app"], environment)
    results = []
    try:
        for jobs in (1, 2):
            # Only this worktree's generated postprocessor stamp is removed.
            # The source, compiled module, profile, flags and size gate stay identical.
            (bundle / ".wasm-input").unlink(missing_ok=True)
            environment["TILO_WASM_OPT_JOBS"] = str(jobs)
            result = execute(root, ["target/debug/tilo-app", "build", "--target", "web",
                                   "--bundle", "desktop", "--release", "apps/calculator"], environment)
            result["optimizer_jobs"] = jobs
            result["module_hashes"] = module_hashes(bundle)
            results.append(result)
            (output / f"jobs-{jobs}.json").write_text(json.dumps(result, indent=2) + "\n")
        if results[0]["module_hashes"] != results[1]["module_hashes"]:
            raise ValueError("parallel optimization changed module bytes")
        if checked_source(root) != source:
            raise ValueError("source changed during qualification")
        execute(root, ["python3", "tools/web/wasm_size_ratchet.py", "measure", "desktop", str(bundle)], environment)
        receipt = {"source_sha": source, "binaryen_cores_per_process": 3,
                   "module_bytes_identical": True, "results": results}
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"source_sha": source, "module_bytes_identical": True,
                          "results": [{k: row[k] for k in ("optimizer_jobs", "wall_seconds", "phases")}
                                      for row in results]}), flush=True)
    finally:
        (output / "source.json").write_text(json.dumps({"source_sha": source}) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    arguments = parser.parse_args()
    qualify(Path.cwd(), arguments.out)


if __name__ == "__main__":
    main()
