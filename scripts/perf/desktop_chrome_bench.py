#!/usr/bin/env python3
"""Measure windowed search input and layout work in a private Cage session.

Uses the real compositor, GPU and existing frame trace. Never drives or
captures the user's desktop. Compare identical dimensions and counts between
binaries; round trips include socket/loop overhead, not panel photon latency.
Requires Cage and a debug (or dev-overrides) shell binary.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

import headless_key_latency as latency
from lock_screen_bench import require_success


def wait_for(predicate, description, proc, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        if proc.poll() is not None:
            raise RuntimeError(f"compositor exited while waiting for {description}")
        threading.Event().wait(0.02)
    raise RuntimeError(f"timed out waiting for {description}")


def viewport_from_targets(reply):
    roots = [entry for entry in reply["targets"] if entry["name"] == "ShellOverlayHost"]
    if len(roots) != 1 or len(roots[0]["rect"]) != 4:
        raise RuntimeError("cannot observe the compositor viewport")
    width, height = roots[0]["rect"][2:]
    if not (width > 0 and height > 0):
        raise RuntimeError("compositor viewport is empty")
    return [width, height]


def measure(args):
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    log = out / f"{args.label}.log"
    report = out / f"{args.label}.json"
    if log.exists() or report.exists():
        raise FileExistsError("choose a new label; benchmark results are not overwritten")
    binary = Path(args.shell_bin).resolve()
    with binary.open("rb") as executable:
        binary_hash = hashlib.file_digest(executable, "sha256").hexdigest()
        binary_stat = os.fstat(executable.fileno())
    bench = latency._bench()
    with tempfile.TemporaryDirectory(prefix="tilo-chrome-bench-", dir=os.environ["TMPDIR"]) as temp:
        root = Path(temp).resolve()
        (root / "home").mkdir(mode=0o700)
        (root / "run").mkdir(mode=0o700)
        (root / "config/tilo/session").mkdir(parents=True)
        ctl = root / "run/control.sock"
        env_args = argparse.Namespace(out=out, label=args.label, outputs=None,
                                      width=args.width, height=args.height, hz=60)
        env = latency.session_env(root, env_args, ctl)
        env.update(WLR_BACKENDS="headless", WLR_LIBINPUT_NO_DEVICES="1",
                   TILO_WINIT_SIZE=f"{args.width}x{args.height}")
        if args.trace_search:
            env["RUST_LOG"] = "shell::search_timing=debug"
        proc = control = None
        try:
            with log.open("wb") as sink:
                proc = subprocess.Popen(
                    ["cage", "-d", "-s", "--", str(binary), "--backend", "winit"],
                    env=env, stdout=sink, stderr=subprocess.STDOUT, start_new_session=True,
                )
                if not bench.wait_for_socket(ctl, 60):
                    raise RuntimeError(f"no control socket; see {log}")
                control = bench.Control(ctl)
                def send(op):
                    return require_success(control.send(op))
                def palette():
                    return send({"op": "state_dump"})["state"]["chrome"]["palette"]
                def sample(operations):
                    samples = []
                    for operation in operations:
                        started = time.perf_counter_ns()
                        operation()
                        samples.append((time.perf_counter_ns() - started) // 1000)
                    return {"round_trip": latency.distribution(samples)}
                send({"op": "modifiers", "ctrl": True, "shift": False, "alt": False, "logo": False})
                send({"op": "key", "key": "Space"})
                send({"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False})
                wait_for(lambda: palette()["visible"], "open search", proc)
                threading.Event().wait(2)
                viewport = viewport_from_targets(send({"op": "list_targets"}))
                if viewport != [args.width, args.height]:
                    raise RuntimeError(f"parent configured viewport {viewport}, not the requested {[args.width, args.height]}")
                result = {"label": args.label, "shell": str(binary), "binary_sha256": binary_hash,
                          "size": viewport, "samples": args.samples}
                result["pointer"] = sample([
                    lambda x=x: send({"op": "pointer_move", "x": x, "y": 1.0})
                    for x in range(1, args.samples + 1)
                ])
                expected = ""
                def type_key(key):
                    nonlocal expected
                    expected = expected[:-1] if key == "Backspace" else expected + key
                    send({"op": "key", "key": key})
                    wait_for(lambda: palette()["query"] == expected, "consumed query edit", proc)
                result["typing"] = sample([
                    lambda key=key: type_key(key)
                    for key in ["a", "Backspace"] * args.samples
                ])
                if binary.stat().st_ino != binary_stat.st_ino or binary.stat().st_mtime_ns != binary_stat.st_mtime_ns:
                    raise RuntimeError("shell binary changed during measurement; use a stable copy")
                report.write_text(json.dumps(result, indent=2) + "\n")
                return result
        finally:
            if control:
                control.close()
            if proc:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shell-bin", default="target/debug/shell")
    parser.add_argument("--out", type=Path, default=Path("target/chrome-bench"))
    parser.add_argument("--label", required=True)
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    parser.add_argument("--samples", type=int, default=40)
    parser.add_argument("--trace-search", action="store_true", help="log provider query/revision costs (adds tracing overhead)")
    args = parser.parse_args()
    if not args.label or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-" for c in args.label):
        parser.error("label must contain only letters, numbers, underscores and hyphens")
    if min(args.width, args.height, args.samples) <= 0:
        parser.error("dimensions and samples must be positive")
    result = measure(args)
    print(json.dumps({"label": result["label"], **{
        phase: result[phase]
        for phase in ("pointer", "typing")
    }}, indent=2))


if __name__ == "__main__":
    main()
