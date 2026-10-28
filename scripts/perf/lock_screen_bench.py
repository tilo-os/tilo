#!/usr/bin/env python3
"""Measure typing into a real lock screen without locking the host desktop.

Compare ordinary dev and release binaries on the same GPU, size and refresh:

    scripts/perf/lock_screen_bench.py --shell-bin target/debug/shell --label dev
    scripts/perf/lock_screen_bench.py --shell-bin target/release/shell --label release

Release binaries need shell's `dev-overrides` feature for synthetic seat input.
The private headless session renders on the real GPU and retires frames on a
simulated vblank grid. The recorded retirement is not physical photon latency.
Only synthetic characters are typed; no authentication attempt is submitted.
"""
from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import time

import headless_key_latency as latency

REPO = Path(__file__).resolve().parents[2]


def require_success(reply: dict) -> dict:
    if "err" in reply or "error" in reply:
        raise RuntimeError(f"control command refused: {reply}")
    return reply


def check_password_state(reply: dict, expected: int) -> None:
    state = require_success(reply)["state"]["chrome"]
    if not state["session_locked"]:
        raise RuntimeError("the measured session is not locked")
    if state["lock_auth"]["password_len"] != expected:
        raise RuntimeError(f"the lock screen did not consume synthetic key {expected}")


def wait_password_state(control, expected: int, timeout: float = 5, previous: int | None = None) -> None:
    # Seat input is queued for the input funnel; its acknowledgement is not
    # proof that the next dispatch has consumed it. Bound that asynchronous
    # handoff rather than mistaking a stale state reply for a dropped key.
    deadline = time.monotonic() + timeout
    previous = expected - 1 if previous is None else previous
    while True:
        reply = require_success(control.send({"op": "state_dump"}))
        state = reply["state"]["chrome"]
        length = state["lock_auth"]["password_len"]
        if not state["session_locked"] or length != previous or time.monotonic() >= deadline:
            check_password_state(reply, expected)
            return
        threading.Event().wait(0.001)


def summarize(trace: Path, log: str, keys: int) -> dict:
    _, rows = latency._bench().read_trace(trace)
    result = {}
    # A lock action can itself produce an input sample. Select the last keys
    # samples in chronological order, not the largest values in the run.
    for field in ("input_latency_us", "input_retirement_latency_us"):
        samples = [row[field] for row in rows if row.get(field)]
        if len(samples) < keys:
            raise ValueError(f"only {len(samples)} of {keys} keys have {field} samples")
        result[field] = latency.distribution(samples[-keys:])
    frames = [int(match[1]) for match in latency.GPU_LINE.finditer(log)]
    if len(frames) < keys:
        raise ValueError(f"only {len(frames)} rendered frames for {keys} keys")
    result["render_frame_us"] = latency.distribution(frames[-keys:])
    painted = list(latency.PAINT_LINE.finditer(log))
    if len(painted) < keys:
        raise ValueError(f"only {len(painted)} paint-area measurements for {keys} keys")
    typing = painted[-keys:]
    result["raster_pixels"] = latency.pixel_distribution([int(match[2]) for match in typing])
    result["dense_frames"] = sum(match[3] == "1" for match in typing)
    # These are CPU wall times, not GPU timestamps. In particular, the render
    # duration includes command submission and waiting for GPU completion.
    for name, pattern in (
        ("render_submit_us", r"CPU-FRAME .*?\bsubmit_us=(\d+)"),
        ("render_settle_us", r"CPU-FRAME .*?\bsettle_us=(\d+)"),
    ):
        values = [int(match[1]) for match in re.finditer(pattern, log)]
        if len(values) < keys:
            raise ValueError(f"missing {name} measurements")
        result[name] = latency.distribution(values[-keys:])
    return result


def measure(args: argparse.Namespace) -> dict:
    bench = latency._bench()
    shell = Path(args.shell_bin).resolve(strict=True)
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    log = args.out / f"{args.label}.log"
    if log.exists():
        raise FileExistsError(f"choose a new label to preserve {log}")
    # Canonicalize the root: the session's safe settings open deliberately
    # rejects symlinked ancestors (for example Fedora's /home -> /var/home).
    root = Path(tempfile.mkdtemp(prefix="lock-bench-", dir=os.environ.get("TMPDIR"))).resolve()
    proc = control = None
    wait = threading.Event().wait
    try:
        (root / "run").mkdir(mode=0o700)
        (root / "home").mkdir(mode=0o700)
        ctl = root / "run/control.sock"
        args.outputs = None
        env = latency.session_env(root, args, ctl)
        settings = root / "config/tilo/session/settings.json"
        settings.parent.mkdir(parents=True)
        settings.write_text(json.dumps({"appearance": {"mode": "dark"}}))
        with log.open("wb") as sink:
            proc = subprocess.Popen(
                [str(shell), "--backend", "headless", "--disable-peer-sync"],
                env=env, stdout=sink, stderr=subprocess.STDOUT, start_new_session=True)
            if not bench.wait_for_socket(ctl, args.startup_timeout):
                raise RuntimeError(f"no control socket; see {log}")
            control = bench.Control(ctl)
            require_success(control.send({"op": "watch", "watching": True}))
            require_success(control.send({"op": "actions_invoke", "provider": "session", "id": "lock"}))
            wait(args.settle)
            wait_password_state(control, 0)
            length = 0
            for index in range(args.keys):
                # Every sample must visibly change the field. Repeatedly adding
                # past its 18-dot cap would benchmark an unchanged image instead.
                adding = index % 32 < 16
                key = "a" if adding else "Backspace"
                previous = length
                length += 1 if adding else -1
                require_success(control.send({"op": "seat_key", "key": key}))
                wait_password_state(control, length, previous=previous)
                wait(args.interval_ms / 1000)
            # Let the last key's owed frame retire before ending the trace.
            wait(max(1.0, 3 / args.hz))
            if proc.poll() is not None:
                raise RuntimeError(f"compositor exited during measurement; see {log}")
    finally:
        if control is not None:
            control.close()
        if proc is not None:
            bench.stop_process_group(proc)
        shutil.rmtree(root)
    result = {
        "label": args.label, "shell": str(shell), "keys": args.keys,
        "size": [args.width, args.height], "hz": args.hz,
        **summarize(args.out / f"{args.label}.trace.csv", log.read_text(errors="replace"), args.keys),
    }
    (args.out / f"{args.label}.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--shell-bin", default=REPO / "target/debug/shell")
    parser.add_argument("--out", type=Path, default=REPO / "target/lock-bench")
    parser.add_argument("--label", required=True)
    parser.add_argument("--width", type=int, default=1800)
    parser.add_argument("--height", type=int, default=1100)
    parser.add_argument("--hz", type=float, default=90)
    parser.add_argument("--keys", type=int, default=60)
    parser.add_argument("--interval-ms", type=float, default=125)
    parser.add_argument("--settle", type=float, default=3)
    parser.add_argument("--startup-timeout", type=float, default=60)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.label):
        parser.error("label must contain only letters, numbers, underscores and hyphens")
    positive = (args.width, args.height, args.hz, args.keys, args.interval_ms, args.startup_timeout)
    if any(not math.isfinite(value) or value <= 0 for value in positive) or not math.isfinite(args.settle) or args.settle < 0:
        parser.error("dimensions, rate, keys, interval and timeout must be positive; settle must be nonnegative")
    print(json.dumps(measure(args), indent=2))


if __name__ == "__main__":
    main()
