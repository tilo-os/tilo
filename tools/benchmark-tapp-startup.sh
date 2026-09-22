#!/usr/bin/env bash
# Compare a conventional static app with its thin tapp binary. The command
# should perform normal initialization and exit; Calculator's zero-click harness
# does that without requiring Wayland or a GPU.
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
STATIC_BIN="${STATIC_BIN:-$REPO/target/release/calculator}"
THIN_BIN="${THIN_BIN:-$REPO/target/thin-app/calculator}"
RUNTIME_LIB_DIR="${RUNTIME_LIB_DIR:-$REPO/target/thin-app}"
RUNS="${RUNS:-30}"
COLD_RUNS="${COLD_RUNS:-20}"

[ -x "$STATIC_BIN" ] || { echo "missing static binary: $STATIC_BIN" >&2; exit 1; }
[ -x "$THIN_BIN" ] || { echo "missing thin binary: $THIN_BIN" >&2; exit 1; }

LIBSTD_DIR="$(rustc --print target-libdir)"
THIN_LD_LIBRARY_PATH="$RUNTIME_LIB_DIR:$LIBSTD_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

python3 - "$STATIC_BIN" "$THIN_BIN" "$THIN_LD_LIBRARY_PATH" "$RUNS" "$COLD_RUNS" <<'PY'
import os
import statistics
import subprocess
import sys
import time

static, thin, thin_libs, warm_runs, cold_runs = sys.argv[1:]
warm_runs, cold_runs = int(warm_runs), int(cold_runs)
thin_env = os.environ | {"LD_LIBRARY_PATH": thin_libs}
args = ["--bench-clicks", "0"]

def dependencies(binary, env):
    output = subprocess.check_output(["ldd", binary], env=env, text=True)
    paths = {binary}
    for line in output.splitlines():
        paths.update(token for token in line.split() if token.startswith("/") and os.path.isfile(token))
    return sorted(paths)

def evict(paths):
    for path in paths:
        try:
            fd = os.open(path, os.O_RDONLY)
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)
        except (AttributeError, OSError):
            pass

def run(binary, env):
    started = time.perf_counter_ns()
    completed = subprocess.run(
        [binary, *args],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or "no stderr"
        raise SystemExit(
            f"{binary} exited {completed.returncode} during benchmark: {detail}"
        )
    return (time.perf_counter_ns() - started) / 1_000_000

def report(label, values):
    ordered = sorted(values)
    p95 = ordered[min(len(ordered) - 1, int(len(ordered) * .95))]
    print(f"{label:14} n={len(values):2} median={statistics.median(ordered):7.3f} ms "
          f"p95={p95:7.3f} min={ordered[0]:7.3f} max={ordered[-1]:7.3f}")

static_deps = dependencies(static, os.environ)
thin_deps = dependencies(thin, thin_env)
for _ in range(3):
    run(static, os.environ)
    run(thin, thin_env)
report("static warm", [run(static, os.environ) for _ in range(warm_runs)])
report("thin warm", [run(thin, thin_env) for _ in range(warm_runs)])

static_cold, thin_cold = [], []
for _ in range(cold_runs):
    evict(static_deps)
    static_cold.append(run(static, os.environ))
    evict(thin_deps)
    thin_cold.append(run(thin, thin_env))
report("static cold", static_cold)
report("thin cold", thin_cold)
print(f"dependency files: static={len(static_deps)} thin={len(thin_deps)}")
PY
