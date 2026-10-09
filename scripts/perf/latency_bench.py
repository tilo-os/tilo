#!/usr/bin/env python3
"""Compositor-side input and workspace-switch latency on a headless compositor.

The nightly half of the latency KPIs (TIL-587; the Linear document
"Performance and cost KPIs"): one disposable `shell --backend headless`
session, driven through its test-control socket, measures

- `latency.workspace_switch.first_frame`, `.settled`, `.dropped_frames`:
  Super+Page Down / Page Up, alternately, each timed by the compositor's own
  `WORKSPACE-SWITCH` line (chord to its first frame, chord to the resting frame
  after the 400 ms frame run, refresh intervals in the run without a new frame);
- `latency.input_to_frame.pointer`: a pointer move to the frame that shows it,
  from the frame-loop trace's `input_latency_us` (the loop taking the input to
  that frame's submit returning; the headless presenter is synchronous, so the
  frame is rendered when it returns).

What it is not: input reaches the compositor through the control socket, not
libinput, and the headless backend has no page flip or panel. The number
follows the compositor's own work from input to a finished frame, repeatably,
on whatever renderer the runner has. Input to photon on real hardware is
`scripts/perf/device_latency_bench.py` (docs/benchmarks/reference-hardware.md).

Every requested sample must be observed: a chord without its `WORKSPACE-SWITCH`
line, or a pointer phase that yields too few frames, fails the run rather than
reporting a smaller set.

    scripts/perf/latency_bench.py --shell-bin target/debug/shell --out target/latency \\
        --env-json env.json --records target/latency/latency.jsonl
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/perf"))

import adapters  # noqa: E402
import record  # noqa: E402

# Inputs closer together than this belong to one burst; the pointer phase is
# the trailing burst of the trace, after a pause longer than any gap in it.
BURST_GAP_US = 500_000
PHASE_PAUSE_S = 2.0


def _frame_loop_bench():
    spec = importlib.util.spec_from_file_location("frame_loop_bench", REPO / "scripts/perf/frame-loop-bench.py")
    if spec is None or spec.loader is None:
        raise ImportError("scripts/perf/frame-loop-bench.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


bench = _frame_loop_bench()


def trailing_burst(rows: list[dict], max_gap_us: int = BURST_GAP_US) -> list[int]:
    """`input_latency_us` of the last burst of input-carrying ticks, in order."""
    carrying = [row for row in rows if row.get("input_latency_us") is not None]
    burst: list[dict] = []
    for row in reversed(carrying):
        if burst and burst[-1]["start_us"] - row["start_us"] > max_gap_us:
            break
        burst.append(row)
    return [row["input_latency_us"] for row in reversed(burst)]


def ok(reply: dict, what: str) -> dict:
    if not isinstance(reply, dict) or "err" in reply:
        raise RuntimeError(f"{what} refused: {reply}")
    return reply


def chord(ctl, key: str) -> None:
    """Super+`key`, pressed as a person presses it: hold Super, tap the key, release."""
    for command in ({"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": True},
                    {"op": "key", "key": key},
                    {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False}):
        ok(ctl.send(command), f"{command['op']} {key}")


def state(ctl) -> dict:
    reply = ok(ctl.send({"op": "state_dump"}), "state_dump")
    return reply.get("state", reply)


def wait_for(predicate, timeout: float, what: str, interval: float = 0.1):
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise RuntimeError(f"timed out after {timeout:.0f} s waiting for {what}")
        time.sleep(interval)


def ensure_two_workspaces(ctl) -> None:
    """Add a workspace through the overview's own control when only one exists."""
    count = len(state(ctl)["workspace_tree"]["workspaces"])
    if count >= 2:
        return
    ok(ctl.send({"op": "actions_invoke", "provider": "actions", "id": "overview.toggle"}), "overview.toggle")

    def add_target() -> bool:
        reply = ok(ctl.send({"op": "list_targets"}), "list_targets")
        targets = reply.get("targets", reply.get("ok", []))
        return any(isinstance(entry, dict) and entry.get("name") == "add_workspace" for entry in targets)

    wait_for(add_target, 10, "the overview's add_workspace control")
    ok(ctl.send({"op": "click_named", "name": "add_workspace"}), "click add_workspace")
    wait_for(lambda: len(state(ctl)["workspace_tree"]["workspaces"]) > count, 10, "a second workspace")
    if state(ctl)["chrome"]["overlays"].get("overview"):
        ok(ctl.send({"op": "key", "key": "Escape"}), "Escape")
        wait_for(lambda: not state(ctl)["chrome"]["overlays"].get("overview"), 10, "the overview to close")


def switch_lines(log: Path) -> list[dict[str, int]]:
    return adapters.switch_samples(log.read_text(encoding="utf-8", errors="replace"))


def drive_switches(ctl, log: Path, count: int, pause: float) -> list[dict[str, int]]:
    """`count` switches, each waited for until the compositor reported it."""
    before = len(switch_lines(log))
    tree = state(ctl)["workspace_tree"]
    ids = [workspace["id"] for workspace in tree["workspaces"]]
    # Page Down walks towards the end of the list; from the last one walk back first.
    keys = ["PageDown", "PageUp"]
    if ids.index(tree["active"]) == len(ids) - 1:
        keys.reverse()
    for index in range(count):
        chord(ctl, keys[index % 2])
        wait_for(lambda: len(switch_lines(log)) >= before + index + 1, 10,
                 f"WORKSPACE-SWITCH for switch {index + 1} of {count}")
        time.sleep(pause)
    lines = switch_lines(log)[before:]
    if len(lines) != count:
        raise RuntimeError(f"{count} chords produced {len(lines)} WORKSPACE-SWITCH lines")
    return lines


def session_env(root: Path, args: argparse.Namespace, ctl_path: Path, trace: Path) -> dict[str, str]:
    env = dict(os.environ)
    for name in ("WAYLAND_DISPLAY", "DISPLAY", "TILO_RUN_SECONDS"):
        env.pop(name, None)
    env.update({
        "TILO_FRAME_TRACE": str(trace),
        "TILO_FRAME_WARMUP_MS": "0",
        "TILO_TEST_CONTROL": str(ctl_path),
        "TILO_HEADLESS_SIZE": f"{args.width}x{args.height}",
        "TILO_REFRESH_HZ": str(args.refresh_hz),
        "HOME": str(root / "home"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "state"),
        "XDG_RUNTIME_DIR": str(root / "run"),
        "TILO_STATE_PATH": str(root / "config" / "state.json"),
        "TILO_IDENTITY_ROOT": str(root / "identity-root"),
        "TILO_PEER_KEY_PATH": str(root / "keys" / "peer-key"),
        "RUST_BACKTRACE": "1",
    })
    return env


def measure(args: argparse.Namespace) -> dict[str, Any]:
    shell = Path(args.shell_bin)
    if not shell.is_file():
        raise RuntimeError(f"no shell binary at {shell}")
    args.out.mkdir(parents=True, exist_ok=True)
    log = args.out / "compositor.log"
    trace = args.out / "frame-trace.csv"
    root = Path(tempfile.mkdtemp(prefix="tilo-latency-bench-"))
    (root / "run").mkdir(mode=0o700)
    ctl_path = root / "run" / "test-control.sock"
    env = session_env(root, args, ctl_path, trace)
    proc = None
    try:
        provision = subprocess.run([str(shell), "--provision-identity", "--disable-peer-sync"], env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                                   timeout=args.startup_timeout, check=False)
        if provision.returncode != 0:
            raise RuntimeError(f"could not provision the disposable identity:\n{provision.stdout}")
        with log.open("wb") as sink:
            proc = subprocess.Popen([str(shell), "--mode", "native", "--backend", "headless",
                                     "--disable-peer-sync"],
                                    env=env, stdout=sink, stderr=subprocess.STDOUT, start_new_session=True)
        if not bench.wait_for_socket(ctl_path, timeout=args.startup_timeout):
            raise RuntimeError("the control socket never appeared:\n"
                               + log.read_text(errors="replace")[-3000:])
        ctl = bench.Control(ctl_path)
        bench.dismiss_first_login(ctl)
        ensure_two_workspaces(ctl)
        time.sleep(1.0)
        switches = drive_switches(ctl, log, args.switches, args.switch_pause)
        print(f"[latency] {len(switches)} workspace switches timed", flush=True)
        # Nothing touches the socket in the pause, so the pointer burst that
        # follows is the trace's last.
        time.sleep(PHASE_PAUSE_S)
        sent = bench.drive_pointer(ctl, args.pointer_hz, args.pointer_count / args.pointer_hz,
                                   args.width, args.height)
        time.sleep(0.5)
        ctl.close()
    finally:
        if proc is not None:
            bench.stop_process_group(proc)
        shutil.rmtree(root, ignore_errors=True)
    _, rows = bench.read_trace(trace)
    pointer = trailing_burst(rows)
    if len(pointer) < args.min_pointer_ratio * sent:
        raise RuntimeError(f"{sent} pointer moves produced only {len(pointer)} timed frames "
                           f"(< {args.min_pointer_ratio:.0%})")
    print(f"[latency] {sent} pointer moves, {len(pointer)} timed frames", flush=True)
    return {"switches": switches, "pointer_us": pointer, "pointer_sent": sent}


def records(result: dict[str, Any], env: dict[str, Any], args: argparse.Namespace) -> list[record.Record]:
    meta = {**env.get("meta", {}), "backend": "headless", "input": "test-control",
            "refresh_hz": str(args.refresh_hz), "viewport": f"{args.width}x{args.height}"}
    env = {**env, "meta": meta}
    base = {name: value for name, value in env.items() if name != "meta"}
    pointer = record.from_samples("latency.input_to_frame.pointer", "ms",
                                  [value / 1000 for value in result["pointer_us"]],
                                  meta={**meta, "pointer_moves": str(result["pointer_sent"])}, **base)
    return [pointer, *adapters.workspace_switch(result["switches"], env)]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--shell-bin", default=str(REPO / "target/debug/shell"))
    parser.add_argument("--out", type=Path, default=REPO / "target/latency-bench")
    parser.add_argument("--switches", type=int, default=100,
                        help="workspace switches to time; at 100 the p99 is not the single slowest")
    parser.add_argument("--switch-pause", type=float, default=0.3,
                        help="seconds between a switch's report and the next chord")
    parser.add_argument("--pointer-count", type=int, default=600, help="pointer moves to time")
    parser.add_argument("--pointer-hz", type=float, default=20.0)
    parser.add_argument("--min-pointer-ratio", type=float, default=0.8,
                        help="fail when fewer timed frames than this share of the moves")
    parser.add_argument("--refresh-hz", type=int, default=60)
    parser.add_argument("--width", type=int, default=1920)
    parser.add_argument("--height", type=int, default=1080)
    parser.add_argument("--startup-timeout", type=float, default=60.0)
    parser.add_argument("--env-json", type=Path, help="record environment (tools/perf/record.py env)")
    parser.add_argument("--records", type=Path, help="append the records to this JSON-lines file")
    args = parser.parse_args(argv)
    if args.switches < 1 or args.pointer_count < 1 or args.pointer_hz <= 0:
        parser.error("--switches, --pointer-count and --pointer-hz must be positive")
    if (args.env_json is None) != (args.records is None):
        parser.error("--env-json and --records go together")
    try:
        result = measure(args)
        if args.records is not None:
            for item in records(result, record.load_env(args.env_json), args):
                record.write(args.records, item)
                print(item.to_json())
        else:
            for name, values in (("pointer input to frame (ms)", [v / 1000 for v in result["pointer_us"]]),
                                 ("switch first frame (ms)", [s["first_shown_us"] / 1000 for s in result["switches"]]),
                                 ("switch settled (ms)", [s["settled_shown_us"] / 1000 for s in result["switches"]]),
                                 ("switch dropped frames", [float(s["dropped"]) for s in result["switches"]])):
                ordered = sorted(values)
                print(f"{name}: n={len(ordered)} p50={record.nearest_rank(ordered, 50):.2f} "
                      f"p99={record.nearest_rank(ordered, 99):.2f} max={ordered[-1]:.2f}")
    except (RuntimeError, ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"latency_bench.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
