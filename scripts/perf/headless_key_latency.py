#!/usr/bin/env python3
"""Key-to-present latency of typing on the Rust session, headless.

The session times every key from the instant it hands it to the window's client
until the frame that shows the client's answer reaches the display
(`crates/session/frame-metrics/src/key_latency.rs`): each answered key is one
`KEY-LATENCY app=<id> key_to_commit_us=... key_to_submit_us=...
key_to_present_us=...` line in the session log. This script types into a window,
reads those lines, and reports the distribution. The legs are the same three the
old desktop logged, so a number here is comparable with an older one:

    key -> client commit -> compositor submit -> vblank that shows it

What it leaves out is the kernel's input stack and the panel itself: the keys
enter through the session's control socket, so this is the compositor, the
client and the GPU.

Two windows to type into:

- **an installed app** (`--app so.tilo.notes`, or a tilolang app's id): opened
  through the desktop's own launcher, as a person opens it. Its own editor
  answers each key with a commit, which is what a person feels. The run clicks
  into the window first (`--click-at`) so the editor has the caret, and refuses
  to type when a key changes nothing the app draws.
- **the synthetic client** (the default): `crates/session/session/examples/
  latency_client.rs`, a window that answers every key with exactly one commit
  and draws nothing else, so the compositor's share of the latency stands alone.

The session is the tree's headless desktop (`scripts/headless-examples/
real_apps.py`): the session `dist/dev/nested-session.sh` starts, its apps
installed as signed packages, rendering on lavapipe, on a throwaway identity and
a private runtime directory. Nothing here touches the desktop this runs from.

    # once per tree, or pass --build here
    dist/dev/nested-session-build.sh                     # through a remote exec
    cargo build --release -p tilo-session --example latency_client
    cargo build --release -p click-test-client --bin tilo-loadclient

    # the synthetic client, 300 keys
    scripts/perf/headless_key_latency.py --label client --client-bin target/release/examples/latency_client

    # a native app and a tilolang app, through the desktop
    scripts/perf/headless_key_latency.py --label native-notes --app so.tilo.notes
    scripts/perf/headless_key_latency.py --label tilolang-notes --app so.tilo.lang.notes

    scripts/perf/headless_key_latency.py --compare target/key-latency/*.json
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
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
# The window of the synthetic client: what its READY line says its size is.
CLIENT_APP = "so.tilo.latency-client"
STAGES = ("key_to_commit_us", "key_to_submit_us", "key_to_present_us")
KEY_LINE = re.compile(
    r"KEY-LATENCY app=(?P<app>\S*) key_to_commit_us=(?P<key_to_commit_us>\d+) "
    r"key_to_submit_us=(?P<key_to_submit_us>\d+) key_to_present_us=(?P<key_to_present_us>\d+)")
UNANSWERED_LINE = re.compile(r"KEY-LATENCY app=(?P<app>\S*) unanswered=1")
# The synthetic client's own clock, for the legs the compositor cannot see: when its commit went
# out, and when the compositor showed it.
CLIENT_COMMIT_LINE = re.compile(r"CLIENT-KEY n=(?P<n>\d+) key_to_commit_us=(?P<us>\d+)")
CLIENT_FRAME_LINE = re.compile(r"CLIENT-FRAME n=(?P<n>\d+) key_to_frame_us=(?P<us>\d+)")


def real_apps():
    """The headless session with the tree's apps, as every check starts it."""
    spec = importlib.util.spec_from_file_location(
        "real_apps", REPO / "scripts/headless-examples/real_apps.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def percentile(ordered: list[float], p: float) -> float:
    return ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)]


def distribution(values_us: list[float]) -> dict:
    ordered = sorted(value / 1000 for value in values_us)
    return {"n": len(ordered), "p50_ms": percentile(ordered, 50),
            "p90_ms": percentile(ordered, 90), "p99_ms": percentile(ordered, 99),
            "max_ms": ordered[-1]}


def parse_samples(text: str, app: str) -> list[dict]:
    return [{stage: int(match[stage]) for stage in STAGES}
            for match in KEY_LINE.finditer(text) if match["app"] == app]


def summarize(rows: list[dict]) -> dict:
    out = {stage: distribution([row[stage] for row in rows]) for stage in STAGES}
    # The legs between the compositor's own marks, key by key.
    out["commit_to_submit_us"] = distribution(
        [row["key_to_submit_us"] - row["key_to_commit_us"] for row in rows])
    out["submit_to_present_us"] = distribution(
        [row["key_to_present_us"] - row["key_to_submit_us"] for row in rows])
    return out


def wait_line(proc: subprocess.Popen, timeout: float) -> str:
    """The client's first stdout line, bounded: it prints `READY <w>x<h>` once its window is drawn."""
    deadline = time.monotonic() + timeout
    data = b""
    reader = proc.stdout
    if reader is None:
        raise RuntimeError("the latency client has no stdout")
    fd = reader.fileno()
    with selectors.DefaultSelector() as selector:
        selector.register(fd, selectors.EVENT_READ)
        while b"\n" not in data:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise RuntimeError("the latency client never reported READY")
            try:
                chunk = reader.read1(4096)
            except BlockingIOError:
                continue
            if not chunk:
                raise RuntimeError(f"the latency client exited before READY: {data!r}")
            data += chunk
    line = data.split(b"\n", 1)[0].decode(errors="replace")
    if not line.startswith("READY "):
        raise RuntimeError(f"the latency client's first line is not READY: {line!r}")
    return line


def dismiss_first_login(session, timeout: float = 5.0) -> None:
    """Complete the disposable session's first-login flow, pressing its own controls."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        names = {target.get("name") for target in session.targets()
                 if isinstance(target, dict) and isinstance(target.get("name"), str)}
        onboarding = sorted(name for name in names if name.startswith("onboarding."))
        if not onboarding:
            return
        for name in ("onboarding.begin.button", "onboarding.understood.row",
                     "onboarding.create.button", "onboarding.backup.skip-acknowledged.row",
                     "onboarding.backup.continue.button", "onboarding.tour.skip.button",
                     "onboarding.finish.button"):
            if name in names:
                session.click_named(name)
                break
        else:
            return
        time.sleep(0.2)


def wayland_display(session, timeout: float = 10.0) -> str:
    """The socket the session's clients connect to, named as a client finds it."""
    runtime = Path(session.runtime_directory())
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        sockets = sorted(path.name for path in runtime.glob("wayland-*")
                         if path.is_socket())
        if sockets:
            return sockets[0]
        time.sleep(0.05)
    raise RuntimeError(f"the session bound no Wayland socket in {runtime}")


def start_client(args: argparse.Namespace, session, env: dict) -> subprocess.Popen:
    """The synthetic window that commits once per key, on the session's own socket."""
    client = Path(args.client_bin)
    if not client.is_file():
        sys.exit(f"missing {client}: build it with\n"
                 "  cargo build --release -p tilo-session --example latency_client")
    environment = {**env, "XDG_RUNTIME_DIR": session.runtime_directory(),
                   "WAYLAND_DISPLAY": wayland_display(session)}
    print(f"[key-latency] client on {environment['WAYLAND_DISPLAY']}: {client}", flush=True)
    return subprocess.Popen(
        [str(client), CLIENT_APP, "--damage", args.damage, "--buffers", args.buffers,
         "--scale", str(args.client_scale)],
        env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def placed_window(session, app: str, timeout: float) -> dict:
    """The app's window once the desk has drawn it somewhere: a row says where it is only after
    the window mapped, so a click has a rectangle to land in."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for row in session.windows():
            if row.get("app_id") == app and row.get("rect"):
                return row
        time.sleep(0.1)
    raise RuntimeError(f"{app} showed no window to type into")


def app_frames(session, app: str) -> int:
    """How many frames the app's window has committed: a rise is a key it answered."""
    return max((int(row.get("frames") or 0) for row in session.windows()
                if row.get("app_id") == app), default=0)


def type_keys(session, args: argparse.Namespace, warmup: int, keys: int) -> None:
    """Press `keys` keys as a person does: at even places on the keyboard, at random phases of the
    display's refresh, so no position on the refresh grid is favoured."""
    rng = random.Random(args.seed)
    letters = "abcdefghijklmnopqrstuvwxyz"
    for index in range(warmup + keys):
        session.key(letters[index % len(letters)])
        time.sleep(args.interval_ms / 1000 + rng.random() / args.refresh_hz)


def stop_client(proc: subprocess.Popen) -> str:
    """Stop the synthetic client and answer what it printed (nothing, if it printed nothing)."""
    if proc.poll() is None:
        proc.terminate()
    try:
        out, err = proc.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        out, err = proc.communicate(timeout=5)
    return (out or b"").decode(errors="replace") + (err or b"").decode(errors="replace")


def measure(args: argparse.Namespace) -> dict:
    apps_module = real_apps()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    apps_module.prepare(args.state, apps=args.apps, built=args.build,
                        remote=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))
    env = {"TILO_HEADLESS_SIZE": args.size}
    if args.scale:
        env["TILO_HEADLESS_SCALE"] = str(args.scale)
    session = apps_module.Session(args.state)
    client = None
    app = args.app or CLIENT_APP
    try:
        session.start(apps=args.apps, fresh=True, out=out, tag=args.label, env=env,
                      tapps=args.tapp or None)
        dismiss_first_login(session)
        if args.app:
            window = session.launch(args.app, deadline=args.app_timeout)
            session.answering(args.app, deadline=args.app_timeout)
            print(f"[key-latency] {args.app} window {window.get('title')!r}", flush=True)
            # The window has the keyboard, but its editor has to have the caret before a key
            # changes anything it draws: a click where the person types puts it there.
            across, down = (float(part) for part in args.click_at.split(","))
            placed = placed_window(session, args.app, args.app_timeout)
            left, top, width, height = (int(value) for value in placed["rect"])
            session.click_at(left + int(width * across), top + int(height * down))
            time.sleep(0.5)
            frames_before = app_frames(session, args.app)
            session.key("a")
            time.sleep(0.5)
            if app_frames(session, args.app) <= frames_before:
                sys.exit(f"[key-latency] a key changed nothing {args.app} drew: its editor has "
                         f"no caret at --click-at {args.click_at}; name another point of the "
                         "window, as fractions of its width and height")
        else:
            client = start_client(args, session, os.environ)
            ready = wait_line(client, args.app_timeout)
            print(f"[key-latency] client window {ready.split(' ', 1)[1]}", flush=True)
        time.sleep(args.settle)
        type_keys(session, args, args.warmup, args.keys)
        # The last key's frame reaches its vblank a refresh after it was committed.
        time.sleep(1.0)
    finally:
        client_output = stop_client(client) if client is not None else ""
        session_log = session.log()
        session.stop()

    rows = parse_samples(session_log, app)[args.warmup:]
    unanswered = sum(1 for match in UNANSWERED_LINE.finditer(session_log) if match["app"] == app)
    if len(rows) < args.keys * 0.8:
        sys.exit(f"[key-latency] only {len(rows)} of {args.keys} keys were answered and timed "
                 f"by {app}; the session log's last lines:\n"
                 + "\n".join(session_log.splitlines()[-25:]))
    result = {"label": args.label, "app": app, "size": args.size,
              "keys_sent": args.keys, "unanswered": unanswered,
              "damage": args.damage, "buffers": args.buffers,
              **summarize(rows)}
    # The synthetic client times its own two legs: its commit and the frame the compositor showed,
    # on its own clock. It has no view of the session's marks, so these stand beside them.
    client_commit = [int(match["us"]) for match in CLIENT_COMMIT_LINE.finditer(client_output)]
    client_frame = [int(match["us"]) for match in CLIENT_FRAME_LINE.finditer(client_output)]
    if client_commit:
        result["client_key_to_commit_us"] = distribution(client_commit[args.warmup:])
    if client_frame:
        result["client_key_to_frame_us"] = distribution(client_frame[args.warmup:])
    (out / f"{args.label}.json").write_text(json.dumps(result, indent=2))
    return result


def table(results: list[dict]) -> str:
    """One row per run and stage: p50 / p90 / p99 in milliseconds."""
    stages = [("key_to_commit_us", "key->commit"), ("commit_to_submit_us", "commit->submit"),
              ("submit_to_present_us", "submit->present"), ("key_to_present_us", "key->present"),
              ("client_key_to_commit_us", "client key->commit"),
              ("client_key_to_frame_us", "client key->frame")]
    lines = ["| run | stage | n | p50 | p90 | p99 |", "|---|---|---|---|---|---|"]
    for result in results:
        for key, name in stages:
            found = result.get(key)
            if found:
                lines.append(f"| {result['label']} | {name} | {found['n']} "
                             f"| {found['p50_ms']:.1f} | {found['p90_ms']:.1f} "
                             f"| {found['p99_ms']:.1f} |")
    lines.append("")
    lines.extend(f"{result['label']}: {result['app']} on a {result['size']} display, "
                 f"{result['keys_sent']} keys sent, {result.get('unanswered', 0)} unanswered"
                 for result in results)
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", help="type into this installed app instead of the synthetic client")
    parser.add_argument("--apps", default="notes", help="apps the session installs (default: notes)")
    parser.add_argument("--tapp", type=Path, action="append", metavar="PATH",
                        help="a signed app package the session installs besides them, as a tilolang "
                             "app is (repeatable)")
    parser.add_argument("--build", action="store_true",
                        help="build the session and its apps first (through $TILO_NESTED_SESSION_BUILD_EXEC)")
    parser.add_argument("--client-bin", default=str(REPO / "target/release/examples/latency_client"))
    parser.add_argument("--buffers", choices=("shm", "dmabuf"), default="shm",
                        help="the synthetic client's buffers: wl_shm, or LINEAR dma-bufs")
    parser.add_argument("--damage", choices=("full", "line"), default="full",
                        help="what the synthetic client damages per key")
    parser.add_argument("--client-scale", type=int, default=1,
                        help="buffer pixels per surface pixel the synthetic client draws")
    parser.add_argument("--state", type=Path,
                        default=Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
                        / "tilo-key-latency",
                        help="the session's state directory (default: $XDG_CACHE_HOME/tilo-key-latency)")
    parser.add_argument("--out", type=Path, default=REPO / "target/key-latency")
    parser.add_argument("--label", default="run")
    parser.add_argument("--size", default="1280x800", help="the headless display, WxH")
    parser.add_argument("--scale", default=None, help="the headless display's density")
    parser.add_argument("--keys", type=int, default=300, help="keys timed")
    parser.add_argument("--warmup", type=int, default=20)
    parser.add_argument("--interval-ms", type=float, default=100.0)
    parser.add_argument("--refresh-hz", type=float, default=60.0,
                        help="the display's rate, for the random phase of each key")
    parser.add_argument("--settle", type=float, default=2.0)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--app-timeout", type=float, default=120.0)
    parser.add_argument("--click-at", default="0.5,0.5", metavar="ACROSS,DOWN",
                        help="where in the app's window to click before typing, as fractions of "
                             "its width and height, so the editor has the caret (default 0.5,0.5)")
    parser.add_argument("--compare", nargs="+", type=Path, metavar="RESULT.json",
                        help="print a table of earlier results and exit")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.compare:
        print(table([json.loads(path.read_text()) for path in args.compare]))
        return 0
    result = measure(args)
    print(table([result]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
