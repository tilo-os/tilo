#!/usr/bin/env python3
"""Key-to-photon latency of the compositor, headless, on the machine's real GPU.

Runs `shell --backend headless` with a render thread and a simulated display
(`TILO_HEADLESS_VBLANK_HZ`): every frame is rendered by the real GPU into an
offscreen target, its flip is queued behind the previous one and retires on a
vblank grid at the panel's rate, as the DRM presenter does. A synthetic
`wl_shm` client (`crates/session/shell/examples/latency_client.rs`) redraws its
whole window on every key and damages either all of it (`--damage full`) or
only the text line the key typed on (`--damage line`). Keys are injected through the control socket at
random phases of the refresh interval, through the compositor's input funnel as
a physical keyboard's keys arrive (`--input seat`) or straight to the focused
client (`--input direct`). The compositor's own `KEY-LATENCY`
lines give each key's journey:

    key -> client commit -> compositor submit -> vblank that shows it

and `CPU-FRAME` lines give the GPU time of each frame. What it leaves out is
the panel itself and libinput, so it measures the compositor and the GPU, not
the whole keyboard-to-photon path.

    cargo build --release -p shell --bin shell --features dev-overrides
    cargo build --release -p shell --example latency_client
    scripts/perf/headless_key_latency.py --label after --out target/key-latency
    scripts/perf/headless_key_latency.py --compare before.json after.json

To type into an installed Tilo app instead of the synthetic client, pass
`--app APP_ID` (it is launched through the search palette, as a person does)
and the launcher options of `scripts/perf/frame-loop-bench.py`.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import random
import re
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CLIENT_APP_ID = "so.tilo.latency-client"
STAGES = ("key_to_commit_us", "key_to_submit_us", "key_to_present_us")
KEY_LINE = re.compile(
    r"KEY-LATENCY app=(?P<app>\S*) key_to_commit_us=(?P<key_to_commit_us>\d+) "
    r"key_to_submit_us=(?P<key_to_submit_us>\d+) key_to_present_us=(?P<key_to_present_us>\d+)")
GPU_LINE = re.compile(r"CPU-FRAME serial=\d+ .*?\bgpu_us=(\d+)")
UPLOAD_LINE = re.compile(r"CPU-FRAME serial=\d+ .*?\bshm_upload_bytes=(\d+)")
# What each rendered frame repainted (damage) and rasterised, and whether it
# rendered the whole output.
PAINT_LINE = re.compile(
    r"CPU-FRAME serial=\d+ .*?\bdamage_px=(\d+) raster_px=(\d+) dense=([01])")
# Frames the simulated display showed as the client's own buffer, rendering nothing.
SCANOUT_LINE = re.compile(r"CPU-FRAME serial=\d+ .*?\bscanout=1\b")


def _bench():
    spec = importlib.util.spec_from_file_location(
        "frame_loop_bench", REPO / "scripts/perf/frame-loop-bench.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def percentile(ordered: list[float], p: float) -> float:
    return ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)]


def distribution(values_us: list[int]) -> dict:
    if not values_us:
        raise ValueError("no samples")
    ordered = sorted(v / 1000 for v in values_us)
    return {"n": len(ordered), "p50_ms": percentile(ordered, 50),
            "p90_ms": percentile(ordered, 90), "p99_ms": percentile(ordered, 99),
            "max_ms": ordered[-1]}


def pixel_distribution(values: list[int]) -> dict:
    ordered = sorted(values)
    return {"n": len(ordered), "p50": percentile(ordered, 50), "p90": percentile(ordered, 90),
            "p99": percentile(ordered, 99), "max": ordered[-1]}


def parse_samples(text: str, app: str) -> list[dict]:
    rows = []
    for match in KEY_LINE.finditer(text):
        if match["app"] == app:
            rows.append({stage: int(match[stage]) for stage in STAGES})
    return rows


def summarize(rows: list[dict], gpu_us: list[int]) -> dict:
    out = {stage: distribution([row[stage] for row in rows]) for stage in STAGES}
    # The legs between the compositor's own marks, key by key.
    out["commit_to_submit_us"] = distribution(
        [row["key_to_submit_us"] - row["key_to_commit_us"] for row in rows])
    out["submit_to_present_us"] = distribution(
        [row["key_to_present_us"] - row["key_to_submit_us"] for row in rows])
    if gpu_us:
        out["gpu_frame_us"] = distribution(gpu_us)
    return out


def session_env(root: Path, args: argparse.Namespace, ctl: Path) -> dict[str, str]:
    """The private session's environment; its per-tick frame trace (paint
    reasons and the clients that committed) lands next to the log."""
    env = dict(os.environ)
    for name in ("WAYLAND_DISPLAY", "DISPLAY", "TILO_RUN_SECONDS"):
        env.pop(name, None)
    run = root / "run"
    env.update({
        "TILO_TEST_CONTROL": str(ctl),
        # One panel at its scale and refresh (`--outputs`, e.g. the laptop's
        # `2944x1840@2~90`), or a bare size at scale 1.
        **({"TILO_HEADLESS_OUTPUTS": args.outputs} if args.outputs
           else {"TILO_HEADLESS_SIZE": f"{args.width}x{args.height}"}),
        "TILO_HEADLESS_VBLANK_HZ": str(args.hz),
        "TILO_PROFILE_PRESENTATION": "1",
        "TILO_FRAME_TRACE": str((args.out / f"{args.label}.trace.csv").resolve()),
        "HOME": str(root / "home"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "state"),
        # Private, so the session's Wayland socket and its display file can
        # never replace the ones of a desktop running on this machine.
        "XDG_RUNTIME_DIR": str(run),
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path={run}/no-bus",
        "TILO_STATE_PATH": str(root / "config" / "state.json"),
        "TILO_IDENTITY_ROOT": str(root / "identity-root"),
        "TILO_PEER_KEY_PATH": str(root / "keys" / "peer-key"),
    })
    return env


def wait_client_ready(proc: subprocess.Popen, timeout: float) -> str:
    """Bound the complete readiness line, including a client that writes only a prefix."""
    deadline = time.monotonic() + timeout
    data = b""
    with selectors.DefaultSelector() as selector:
        selector.register(proc.stdout, selectors.EVENT_READ)
        while b"\n" not in data:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise RuntimeError("latency client readiness timed out")
            chunk = os.read(proc.stdout.fileno(), 4096)
            if not chunk:
                raise RuntimeError(f"latency client exited before readiness: {data!r}")
            data += chunk
    fields = data.split(b"\n", 1)[0].split()
    if (len(fields) != 2 or fields[0] != b"READY"
            or re.fullmatch(rb"[1-9][0-9]*x[1-9][0-9]*", fields[1]) is None):
        raise RuntimeError(f"invalid latency client readiness: {data!r}")
    return fields[1].decode("ascii")


def stop_client(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    finally:
        if proc.stdout is not None:
            proc.stdout.close()


def measure(args: argparse.Namespace) -> dict:
    bench = _bench()
    shell, client = Path(args.shell_bin), Path(args.client_bin)
    for binary in ((shell,) if args.app else (shell, client)):
        if not binary.is_file():
            sys.exit(f"missing binary {binary}: see the build lines in --help")
    args.out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="tilo-key-latency-"))
    (root / "run").mkdir(mode=0o700)
    # The session trusts settings only below an existing login home.
    (root / "home").mkdir(mode=0o700)
    ctl_path = root / "run" / "control.sock"
    env = session_env(root, args, ctl_path)
    # The session's settings directory, as an installed session has it.
    settings = root / "config" / "tilo" / "session" / "settings.json"
    settings.parent.mkdir(parents=True)
    if args.hide_top_bar:
        # The desktop's bar over the windows everywhere (Settings,
        # Appearance, Desktop), not only over a maximized window.
        settings.write_text(json.dumps({"desktop_controls": {"hide_top_bar": True}}))
    log = args.out / f"{args.label}.log"
    proc = client_proc = control = None
    try:
        provision = subprocess.run(
            [str(shell), "--provision-identity", "--disable-peer-sync"], env=env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            timeout=args.startup_timeout, check=False)
        if provision.returncode != 0:
            sys.exit(f"could not provision the disposable identity:\n{provision.stdout}")
        with log.open("wb") as sink:
            proc = subprocess.Popen(
                [str(shell), "--mode", "native", "--backend", "headless", "--disable-peer-sync"],
                env=env, stdout=sink, stderr=subprocess.STDOUT, start_new_session=True)
            if not bench.wait_for_socket(ctl_path, args.startup_timeout):
                sys.exit("the control socket never appeared:\n"
                         + log.read_text(errors="replace")[-3000:])
            wayland = bench.wait_for_wayland_socket(log, 10.0)
            if wayland is None:
                sys.exit("the compositor named no Wayland socket")
            control = bench.Control(ctl_path)
            bench.dismiss_first_login(control)
            app = CLIENT_APP_ID
            if args.app:
                app = args.app
                ok, _, why = bench.launch_and_wait_for_app(control, app, args.app_timeout)
                if not ok:
                    sys.exit(f"{app} did not open: {why}")
            else:
                client_proc = subprocess.Popen(
                    [str(client), app, "--damage", args.damage, "--buffers", args.buffers,
                     "--scale", str(args.client_scale)],
                    env={**env, "WAYLAND_DISPLAY": wayland},
                    stdout=subprocess.PIPE, text=True)
                window = wait_client_ready(client_proc, args.startup_timeout)
                print(f"[key-latency] client window {window}", flush=True)
            if args.maximize:
                # Super+Up, as a person maximizes the window they type in.
                for request in ({"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": True},
                                {"op": "key", "key": "ArrowUp"},
                                {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False}):
                    reply = control.send(request)
                    if "err" in reply:
                        sys.exit(f"maximize refused: {reply}")
            time.sleep(args.settle)
            rng = random.Random(args.seed)
            keys = "abcdefghijklmnopqrstuvwxyz"
            for index in range(args.warmup + args.keys):
                op = "seat_key" if args.input == "seat" else "key"
                reply = control.send({"op": op, "key": keys[index % len(keys)]})
                if "err" in reply:
                    sys.exit(f"key refused: {reply}")
                # Random phase against the display so no refresh position is favoured.
                time.sleep(args.interval_ms / 1000 + rng.random() / args.hz)
            time.sleep(1.0)
            if proc.poll() is not None:
                raise RuntimeError(f"compositor exited during measurement: {proc.returncode}")
            if client_proc is not None and client_proc.poll() is not None:
                raise RuntimeError(f"latency client exited during measurement: {client_proc.returncode}")
    finally:
        if control is not None:
            control.close()
        try:
            if client_proc is not None:
                stop_client(client_proc)
        finally:
            if proc is not None:
                bench.stop_process_group(proc)
            shutil.rmtree(root, ignore_errors=True)
    text = log.read_text(errors="replace")
    rows = parse_samples(text, app)[args.warmup:]
    if len(rows) < args.keys * 0.8:
        sys.exit(f"only {len(rows)} of {args.keys} keys were answered and timed; see {log}")
    gpu = [int(m[1]) for m in GPU_LINE.finditer(text)]
    # Frames rendered while typing: between the first and the last answered key, so the
    # window's opening animation does not count against the keys.
    key_lines = [m.start() for m in KEY_LINE.finditer(text) if m["app"] == app]
    typing_frames = sum(1 for m in GPU_LINE.finditer(text)
                        if key_lines and key_lines[0] < m.start() < key_lines[-1])
    painted = [(int(m[1]), int(m[2]), m[3] == "1") for m in PAINT_LINE.finditer(text)]
    paint = {}
    # A frame proven unchanged keeps the image and repaints nothing.
    rendered = [frame for frame in painted if frame[0] > 0]
    if rendered:
        paint = {"dense_frames": sum(dense for _, _, dense in rendered),
                 "kept_frames": len(painted) - len(rendered),
                 "damage_px": pixel_distribution([d for d, _, _ in rendered]),
                 "raster_px": pixel_distribution([r for _, r, _ in rendered])}
    paint["scanout_frames"] = sum(1 for _ in SCANOUT_LINE.finditer(text))
    if "kept_frames" in paint:
        paint["kept_frames"] -= paint["scanout_frames"]
    uploads = [int(m[1]) for m in UPLOAD_LINE.finditer(text)]
    if uploads:
        paint["shm_upload_bytes"] = pixel_distribution(uploads)
    result = {"label": args.label, "app": app,
              "size": args.outputs or f"{args.width}x{args.height}",
              "maximized": args.maximize, "hide_top_bar": args.hide_top_bar,
              "hz": args.hz, "keys_sent": args.keys, "damage": args.damage, "input": args.input,
              "buffers": args.buffers, **paint,
              # Rendered frames per key sent: a key answered by one commit should cost one.
              "frames_per_key": round(len(gpu) / (args.keys + args.warmup), 2),
              "typing_frames_per_key": round(typing_frames / max(1, len(key_lines) - 1), 2),
              **summarize(rows, gpu)}
    (args.out / f"{args.label}.json").write_text(json.dumps(result, indent=2))
    return result


def table(results: list[dict]) -> str:
    """One row per run and stage: p50 / p90 / p99 in milliseconds."""
    stages = [("key_to_commit_us", "key->commit"), ("commit_to_submit_us", "commit->submit"),
              ("submit_to_present_us", "submit->present"), ("key_to_present_us", "key->present"),
              ("gpu_frame_us", "GPU frame")]
    lines = ["| run | stage | n | p50 | p90 | p99 |", "|---|---|---|---|---|---|"]
    for r in results:
        for key, name in stages:
            d = r.get(key)
            if d:
                lines.append(f"| {r['label']} | {name} | {d['n']} | {d['p50_ms']:.1f} "
                             f"| {d['p90_ms']:.1f} | {d['p99_ms']:.1f} |")
    lines.append("")
    lines.extend(f"{r['label']}: {r['frames_per_key']} rendered frames per key sent, "
                 f"{r.get('typing_frames_per_key', '?')} per key while typing"
                 for r in results if "frames_per_key" in r)
    for r in results:
        if "raster_px" in r:
            frames = r["raster_px"]["n"]
            lines.append(
                f"{r['label']}: {r['dense_frames']}/{frames} rendered frames drew the whole "
                f"output, {r.get('kept_frames', 0)} more kept the image unchanged; "
                f"damage p50 {r['damage_px']['p50']} px, "
                f"rasterised p50 {r['raster_px']['p50']} px p90 {r['raster_px']['p90']} px")
        if r.get("scanout_frames"):
            lines.append(f"{r['label']}: {r['scanout_frames']} frames showed the client's own "
                         "buffer (direct scanout), rendering nothing")
        if "shm_upload_bytes" in r:
            up = r["shm_upload_bytes"]
            lines.append(f"{r['label']}: wl_shm bytes uploaded per frame p50 {up['p50']} "
                         f"p90 {up['p90']} max {up['max']}")
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    """The command line, shared with the tests that drive `measure` directly."""
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--shell-bin", default=str(REPO / "target/release/shell"))
    parser.add_argument("--client-bin", default=str(REPO / "target/release/examples/latency_client"))
    parser.add_argument("--out", type=Path, default=REPO / "target/key-latency")
    parser.add_argument("--label", default="run")
    parser.add_argument("--width", type=int, default=2944)
    parser.add_argument("--height", type=int, default=1840)
    parser.add_argument("--hz", type=float, default=60.0, help="the simulated panel's refresh rate")
    parser.add_argument("--outputs", help="a headless output spec instead of --width/--height, "
                        "e.g. 2944x1840@2~90 for the laptop panel at 200 %% and 90 Hz")
    parser.add_argument("--client-scale", type=int, default=1,
                        help="buffer pixels per surface pixel the synthetic client draws "
                        "(2 on a 200 %% panel, so its buffer matches the display)")
    parser.add_argument("--hide-top-bar", action="store_true",
                        help="the person's setting that hides the desktop's bar until the "
                        "pointer is near it, so a maximized window covers the display")
    parser.add_argument("--maximize", action="store_true",
                        help="maximize the typed-into window (Super+Up) before timing")
    parser.add_argument("--keys", type=int, default=300, help="keys timed")
    parser.add_argument("--warmup", type=int, default=20)
    parser.add_argument("--interval-ms", type=float, default=100.0)
    parser.add_argument("--settle", type=float, default=2.0)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--input", choices=("seat", "direct"), default="seat",
                        help="seat: keys enter the compositor's input funnel as a physical "
                             "keyboard's do; direct: straight to the focused client")
    parser.add_argument("--buffers", choices=("shm", "dmabuf"), default="shm",
                        help="the synthetic client's buffers: wl_shm, or LINEAR dma-bufs")
    parser.add_argument("--damage", choices=("full", "line"), default="full",
                        help="what the synthetic client damages per key")
    parser.add_argument("--app", help="type into this installed app instead of the synthetic client")
    parser.add_argument("--app-timeout", type=float, default=60.0)
    parser.add_argument("--startup-timeout", type=float, default=120.0)
    parser.add_argument("--compare", nargs="+", type=Path, metavar="RESULT.json",
                        help="print a table of earlier results and exit")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.compare:
        print(table([json.loads(p.read_text()) for p in args.compare]))
        return 0
    result = measure(args)
    print(table([result]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
