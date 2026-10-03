#!/usr/bin/env python3
"""Measure `tilo-app dev` edit-to-pixels for an in-place Rust hot reload.

Starts a headless `shell --mode native` compositor in a private runtime
directory, runs `tilo-app dev --timings --json APP` against it, then edits
one line of the app's `src/lib.rs` several times. Each save is timed from the
moment the file is closed to the first frame the reloaded code presents, and
split into the phases `tilo-app dev --timings` reports.

Nothing here touches the operator's desktop: the compositor is headless,
`DISPLAY`/`WAYLAND_DISPLAY` are scrubbed, and every process this script starts
is killed by process group on exit.

Examples
--------
    scripts/perf/dev-reload-bench.py ~/scratch/my-app --runs 5
    scripts/perf/dev-reload-bench.py apps/hot-reload-demo --json target/reload.json
    # Fail when the median edit-to-pixels exceeds a budget:
    scripts/perf/dev-reload-bench.py APP --budget-ms 1000

The first save after `tilo-app dev` starts is a warm-up and is not counted.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# The order `tilo-app dev --timings` reports them in.
PHASES = [
    "debounce",
    "cargo",
    "compile",
    "link",
    "stage",
    "signal",
    "checkpoint",
    "dlopen",
    "state_handoff",
    "first_frame",
    "present",
]


class Lines:
    """Timestamped lines of one stream, readable while the process runs."""

    def __init__(self, stream, echo: bool, sink):
        self.lines: list[tuple[float, str]] = []
        self.cond = threading.Condition()
        self.echo = echo
        self.sink = sink
        threading.Thread(target=self._pump, args=(stream,), daemon=True).start()

    def _pump(self, stream):
        for raw in iter(stream.readline, b""):
            line = raw.decode("utf-8", "replace").rstrip("\n")
            now = time.time()
            if self.sink:
                self.sink.write(f"{now:.3f} {line}\n")
                self.sink.flush()
            if self.echo:
                print(line, file=sys.stderr)
            with self.cond:
                self.lines.append((now, line))
                self.cond.notify_all()

    def wait_for(self, pattern: re.Pattern, start: int, timeout: float):
        deadline = time.time() + timeout
        with self.cond:
            while True:
                for index in range(start, len(self.lines)):
                    stamp, line = self.lines[index]
                    if pattern.search(line):
                        return index, stamp, line
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None
                self.cond.wait(remaining)


def kill_group(proc: subprocess.Popen, sig=signal.SIGTERM, grace=5.0):
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, sig)
    except ProcessLookupError:
        return
    try:
        proc.wait(grace)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()


def start_compositor(shell: Path, runtime: Path, log) -> tuple[subprocess.Popen, str]:
    env = dict(os.environ)
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"):
        env.pop(key, None)
    env["XDG_RUNTIME_DIR"] = str(runtime)
    # A session of its own: the compositor provisions an identity and state
    # under HOME, which must not be the operator's.
    home = runtime / "compositor-home"
    for key, sub in [
        ("HOME", ""),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CONFIG_HOME", "config"),
    ]:
        (home / sub).mkdir(parents=True, exist_ok=True)
        env[key] = str(home / sub)
    env["TILO_STATE_PATH"] = str(home / "config" / "state.json")
    env["TILO_TEST_CONTROL"] = str(runtime / "tilo-session" / "test-control.sock")
    env["TILO_HEADLESS_SIZE"] = "1280x800"
    proc = subprocess.Popen(
        # `--mode native` is the smithay compositor: the only mode a Wayland
        # client can attach to.
        [str(shell), "--mode", "native", "--backend", "headless"],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    marker = runtime / "tilo-session" / "wayland-display"
    deadline = time.time() + 60
    while time.time() < deadline:
        if marker.is_file() and marker.read_text().strip():
            return proc, marker.read_text().strip()
        if proc.poll() is not None:
            break
        time.sleep(0.1)
    kill_group(proc)
    raise SystemExit(f"compositor did not publish a Wayland display; see {log.name}")


def edit(source: Path, pattern: str, run: int) -> None:
    """Replace the trailing digits of the first `pattern` match with `run`."""
    text = source.read_text()

    def bump(match: re.Match) -> str:
        value = match.group(0)
        head = re.sub(r"\d*(\W*)$", "", value)
        tail = re.search(r"\d*(\W*)$", value).group(1)
        return f"{head}{run}{tail}"

    updated, count = re.subn(pattern, bump, text, count=1)
    if not count:
        raise SystemExit(f"{source}: {pattern!r} matched nothing; pass --edit-pattern")
    source.write_text(updated)


def summarise(runs: list[dict]) -> dict:
    out: dict = {"paths": sorted({run.get("path", "?") for run in runs})}
    for key in ["total", "notify"] + PHASES:
        values = [run[key] for run in runs if isinstance(run.get(key), (int, float))]
        if values:
            out[key] = {
                "median_ms": round(statistics.median(values), 1),
                "min_ms": round(min(values), 1),
                "max_ms": round(max(values), 1),
            }
    return out


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("app", type=Path)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--tilo-app", type=Path, default=REPO / "target/debug/tilo-app")
    parser.add_argument("--shell", type=Path, default=REPO / "target/debug/shell")
    parser.add_argument("--edit-file", type=Path, default=Path("src/lib.rs"))
    parser.add_argument(
        "--edit-pattern",
        default=r'String::from\("world\d*"\)',
        help="regex whose match's trailing digits change on every save",
    )
    parser.add_argument("--debounce-ms", type=int)
    parser.add_argument(
        "--no-timings", action="store_true", help="time from stderr lines only (older CLIs)"
    )
    parser.add_argument("--settle", type=float, default=1.0, help="idle seconds between saves")
    parser.add_argument("--startup-timeout", type=float, default=3600)
    parser.add_argument("--reload-timeout", type=float, default=600)
    parser.add_argument("--json", type=Path, help="write the runs and summary here")
    parser.add_argument("--log", type=Path, help="write timestamped dev stderr here")
    parser.add_argument("--budget-ms", type=float, help="fail when the median total exceeds this")
    parser.add_argument("--echo", action="store_true", help="copy dev stderr to this stderr")
    args = parser.parse_args()

    app = args.app.resolve()
    source = app / args.edit_file
    original = source.read_text()
    base = os.environ.get("XDG_RUNTIME_DIR") or None
    runtime = Path(tempfile.mkdtemp(prefix="dev-reload-bench-", dir=base))
    os.chmod(runtime, 0o700)
    comp_log = open(runtime / "compositor.log", "w")
    sink = open(args.log, "w") if args.log else None
    compositor, display = start_compositor(args.shell, runtime, comp_log)
    dev = None
    runs: list[dict] = []
    try:
        env = dict(os.environ)
        env.pop("DISPLAY", None)
        # An absolute display names the compositor's socket without moving
        # the runtime directory cargo and the build tools share with the user.
        env["WAYLAND_DISPLAY"] = str(runtime / display)
        command = [str(args.tilo_app), "dev", str(app), "--app-only"]
        if not args.no_timings:
            command += ["--timings", "--json"]
        if args.debounce_ms:
            command += ["--debounce-ms", str(args.debounce_ms)]
        dev = subprocess.Popen(
            command,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
        err = Lines(dev.stderr, args.echo, sink)
        out = Lines(dev.stdout, False, None)
        started = err.wait_for(re.compile(r"\[tilo-app\] started \(pid"), 0, args.startup_timeout)
        if not started:
            raise SystemExit("tilo-app dev never started the app")
        # The first frame of a new process says nothing about reloads.
        time.sleep(3)
        # `--timings` prints its summary once the app reports the frame.
        finished = (
            r"tilo: reloaded \d+ open window|build failed|reload rejected"
            if args.no_timings
            else r"\[tilo-app\] reload (\d+ ms|without)|build failed"
        )
        saves = []
        for run in range(args.runs + 1):
            cursor = len(err.lines)
            edit(source, args.edit_pattern, run + 1)
            saved = time.time()
            done = err.wait_for(re.compile(finished), cursor, args.reload_timeout)
            if not done or "failed" in done[2] or "without" in done[2]:
                raise SystemExit(f"run {run}: no reload ({done[2] if done else 'timeout'})")
            saves.append((saved, done[1]))
            print(f"run {run}: {done[2]}", file=sys.stderr)
            time.sleep(args.settle)
    finally:
        source.write_text(original)
        if dev is not None:
            kill_group(dev, signal.SIGINT, grace=15)
        kill_group(compositor)
        comp_log.close()
    reports = []
    if not args.no_timings:
        for _, line in out.lines:
            try:
                document = json.loads(line)
            except json.JSONDecodeError:
                continue
            reports = document.get("result", {}).get("reloads", [])
        # The rollback of the edit on exit may add one more.
        if len(reports) < len(saves):
            raise SystemExit(f"{len(saves)} saves but {len(reports)} timing reports")
    for run, (saved, seen) in enumerate(saves):
        record: dict = {"run": run}
        if args.no_timings:
            record["total"] = (seen - saved) * 1000
        else:
            report = reports[run]
            record.update({key: report["phases_ms"].get(key) for key in PHASES})
            record["total"] = report["first_frame_unix_ms"] - saved * 1000
            # The watcher's view starts at the event; the rest is inotify.
            record["notify"] = report["changed_unix_ms"] - saved * 1000
            record["path"] = report["path"]
        if run > 0:
            runs.append(record)
    summary = summarise(runs)
    print(json.dumps(summary, indent=2))
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(
            json.dumps({"app": str(app), "runs": runs, "summary": summary}, indent=2)
        )
    if args.budget_ms is not None:
        median = summary.get("total", {}).get("median_ms")
        if median is None or median > args.budget_ms:
            print(
                f"FAIL: median edit-to-pixels {median} ms exceeds {args.budget_ms} ms",
                file=sys.stderr,
            )
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
