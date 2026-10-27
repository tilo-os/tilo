#!/usr/bin/env python3
"""Typing latency of tilolang Todo and native Notes, stage by stage, headless.

Starts a headless nested session (dist/dev/nested-session.sh, lavapipe only)
in its own Home workspace with no community site, so no app waits for a room
to sync and each shows what it holds. The shell runs a simulated 60 Hz display
(TILO_HEADLESS_VBLANK_HZ), so frames are paced and retired as on a panel, and
every app writes one `INPUT-FRAME` line per key (RUST_LOG
tilo::input_frame=debug, ui_core::input_frame).

For each app it opens a text field (Todo: a new item's title, clicked into;
Notes: a new note, whose editor opens focused) and types keys through the
compositor's input funnel (`seat_key`), one at a time, waiting for each key's
answer, then reports per key:

  compositor  KEY-LATENCY: key -> app commit -> compositor submit -> present
  app         INPUT-FRAME: handling, wait for the frame, the frame (and every
              named scope it ran), the render thread's present, and whether
              the frame repainted its whole window and why
  delivery    key-to-commit less the app's dispatch-to-present: the key's way
              from the compositor to the app

Software rendering inflates render times: key->commit is the app's share,
commit->present the compositor's, reported apart. The run fails when a key's
frame repainted the whole window (a keystroke changes a field and what shows
it), when the compositor repainted the whole output while keys were typed, or
when the app wrote no record for a key.

Usage:
  scripts/headless-examples/typing-latency.py [--keys 40] [--out DIR] [--keep]

Needs target/nested-session (dist/dev/nested-session.sh builds it) and
target/debug/tilolang, which builds the Todo package for the session's runtime
(or pass one with --tapp).
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
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
BIN = REPO / "target/nested-session/bin"
KEY = re.compile(r"KEY-LATENCY app=(?P<app>\S*) key_to_commit_us=(?P<commit>\d+) "
                 r"key_to_submit_us=(?P<submit>\d+) key_to_present_us=(?P<present>\d+)")
# Where Todo's title field is once its item is open, alone on a 1280x800 desktop.
TODO_TITLE = (600, 152)
# What each frame the compositor rendered repainted (CPU-FRAME, headless).
CPU_FRAME = re.compile(r"CPU-FRAME serial=\d+ .*?\bdamage_px=(\d+)")
# The headless output, 1280x800 (dist/dev/nested-session.sh).
OUTPUT_PX = 1280 * 800
FRAME = re.compile(r"\[app (?P<app>\S+) std(?:out|err)\] .*?INPUT-FRAME (?P<rest>.*)$", re.M)


def run(argv, env=None) -> str:
    result = subprocess.run(argv, text=True, capture_output=True, env=env)
    if result.returncode != 0:
        raise SystemExit(f"{' '.join(map(str, argv))} failed: {result.stderr.strip()[-2000:]}")
    return result.stdout


class Session:
    def __init__(self, control: str, state: Path):
        self.control = control
        self.state = state

    def env(self) -> dict:
        env = dict(os.environ)
        env.update(TILO_TEST_CONTROL=self.control, XDG_RUNTIME_DIR=str(Path(self.control).parent))
        return env

    def ctl(self, *args: str) -> str:
        return run([BIN / "tilo-control", *args], env=self.env())

    def raw(self, request: dict) -> dict:
        return json.loads(self.ctl("raw", json.dumps(request)))

    def actions(self, query: str = "") -> list:
        return json.loads(self.ctl("actions", "list", query, "--json")).get("actions", [])

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        argv = ["actions", "invoke", provider, ident, "--wait"]
        if args is not None:
            argv += ["--args-json", json.dumps(args)]
        return json.loads(self.ctl(*argv))

    def log(self) -> str:
        return (self.state / "logs/shell.log").read_text(errors="replace")


def until(what, check, deadline=60.0, step=0.25):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(step)
    raise SystemExit(f"timed out waiting for {what}")


def open_app(session: Session, app: str) -> None:
    until(f"{app} among the applications", lambda: any(
        a.get("provider") == "applications" and a.get("id") == app for a in session.actions(app)), 60, 0.5)
    # The applications index refreshes in the background: an entry it just
    # listed may answer once it has settled.
    def opened():
        try:
            session.invoke("applications", app)
            return True
        except SystemExit:
            return False
    until(f"{app} to open", opened, 30, 1.0)
    def presented():
        frames = session.raw({"op": "foreign_frames"})["foreign_frames"]["frames"]
        return any(f["app_id"] == app and f["presented"] and f["pixels_ready"] for f in frames)
    until(f"a presented frame of {app}", presented, 60)


def own_action(session: Session, app: str, title: str):
    for action in session.actions(title):
        if action.get("application") == app and action.get("title") == title:
            return action
    return None


def type_keys(session: Session, app: str, keys: int, settle: float) -> None:
    letters = "abcdefghijklmnopqrstuvwxyz"
    for index in range(keys):
        before = len([m for m in KEY.finditer(session.log()) if m["app"] == app])
        session.raw({"op": "seat_key", "key": letters[index % len(letters)]})
        until("the key's answer", lambda: len(
            [m for m in KEY.finditer(session.log()) if m["app"] == app]) > before, 10, 0.02)
        time.sleep(settle)


def pct(values: list[float], p: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, min(len(ordered) - 1, round(len(ordered) * p / 100) - 1))]


def summarize(log: str, app: str, skip: int) -> dict:
    """Each stage's distribution over the keys after the first `skip`; an app's
    stages in microseconds before `dist` turns them into milliseconds."""
    keys = [m for m in KEY.finditer(log) if m["app"] == app][skip:]
    frames = [m["rest"] for m in FRAME.finditer(log) if m["app"] == app][skip:]
    out: dict = {"keys": len(keys), "frames": len(frames)}
    # The compositor's frames while the keys were typed: none repaints the
    # whole output for a keystroke, or for anything else on the desktop.
    if keys:
        span = log[keys[0].start():keys[-1].end()]
        damage = [int(px) for px in CPU_FRAME.findall(span)]
        out["compositor_frames"] = len(damage)
        out["compositor_full_frames"] = sum(1 for px in damage if px >= OUTPUT_PX)
    def dist(values):
        return {"p50_ms": round(statistics.median(values) / 1000, 2),
                "p90_ms": round(pct(values, 90) / 1000, 2), "max_ms": round(max(values) / 1000, 2)} if values else None
    if keys:
        commit = [int(m["commit"]) for m in keys]
        submit = [int(m["submit"]) for m in keys]
        present = [int(m["present"]) for m in keys]
        out["compositor"] = {
            "key_to_commit": dist(commit),
            "commit_to_submit": dist([s - c for s, c in zip(submit, commit)]),
            "submit_to_present": dist([p - s for p, s in zip(present, submit)]),
            "key_to_present": dist(present),
        }
    stages: dict[str, list[float]] = {}
    for rest in frames:
        for field in rest.split():
            name, _, value = field.partition("=")
            number = value.split("/")[0]
            if number.isdigit():
                stages.setdefault(name, []).append(int(number))
    out["app"] = {name: dist(values) for name, values in sorted(stages.items())}
    # A keystroke changes a field and what shows it: no key's frame repaints
    # the whole window.
    out["full_repaints"] = [re.search(r"repaint=full:(\S*)", rest)[1]
                            for rest in frames if "repaint=full" in rest]
    # One record each per key: the compositor's key-to-commit less the app's
    # dispatch-to-present is the key's way from the compositor to the app.
    if keys and len(frames) == len(keys):
        totals = [int(re.search(r"total_us=(\d+)", rest)[1]) for rest in frames]
        out["delivery"] = dist([max(0, int(m["commit"]) - total) for m, total in zip(keys, totals)])
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tapp", type=Path, help="the Todo package (default: built here with "
                        "target/debug/tilolang for this session's runtime)")
    parser.add_argument("--keys", type=int, default=40)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--settle", type=float, default=0.15, help="seconds between keys")
    parser.add_argument("--out", type=Path, default=REPO / "target/typing-latency")
    parser.add_argument("--apps", default="todo,notes")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    state = out / "state"
    ready = out / "ready.json"
    ready.unlink(missing_ok=True)
    env = {**os.environ, "TILO_HEADLESS_VBLANK_HZ": "60",
           "RUST_LOG": os.environ.get("RUST_LOG", "warn,tilo::input_frame=debug")}
    env.setdefault("TMPDIR", str(Path.home() / "tmp"))
    # The session's own Home workspace and no community site: no room waits
    # for a host, so each app shows what it holds and takes typing.
    community = out / "community.toml"
    community.write_text('site = ""\n')
    env["TILO_COMMUNITY_CONFIG"] = str(community)
    log = open(out / "session.log", "w")
    # Notes installed first: an install collects runtimes no package names
    # yet, so the start below bakes the tilolang runtime again beside an
    # installed Notes that installs nothing more.
    subprocess.run([SESSION, "--no-build", "--prepare-only", "--apps", "notes", "--state", str(state)],
                   stdout=log, stderr=subprocess.STDOUT, env=env, check=True)
    if args.tapp is None:
        runtime = (state / "runtimes/.version").read_text().strip()
        run([REPO / "target/debug/tilolang", "build", REPO / "apps/tl/todo", "--search", REPO / "apps/tl",
             "--out", out / "build", "--tilo-runtime", f"tilo-runtime/{runtime}"])
        args.tapp = out / "build/todo.tapp"
    proc = subprocess.Popen([SESSION, "--no-build", "--headless", "--software", "--fresh-desktop",
                             "--apps", "notes", "--install-tapp", str(args.tapp.resolve()),
                             "--state", str(state), "--ready", str(ready)],
                            stdout=log, stderr=subprocess.STDOUT, start_new_session=True, env=env)
    report: dict = {}
    failures: list[str] = []
    try:
        until("the session", lambda: ready.is_file() or proc.poll() is not None, 600, 0.5)
        if proc.poll() is not None:
            raise SystemExit(f"the session ended; see {out / 'session.log'}")
        session = Session(json.loads(ready.read_text())["control"], state)
        # Todo first, alone on the desktop: its list fills the left, the item
        # it opens the right. Its title field is focused by a click, as a person
        # does; Notes' new note opens with its editor focused.
        for name in args.apps.split(","):
            app = {"todo": "so.tilo.lang.todo", "notes": "so.tilo.notes"}[name]
            print(f"==> {app}", file=sys.stderr)
            open_app(session, app)
            if name == "todo":
                action = until("Todo's 'New item'", lambda: own_action(session, app, "New item"), 60)
                session.invoke("app-actions", action["id"])
                time.sleep(1)
                session.raw({"op": "click_at", "x": 300, "y": 98})
                time.sleep(1)
                session.raw({"op": "click_at", "x": TODO_TITLE[0], "y": TODO_TITLE[1]})
            else:
                action = until("Notes' 'New note'", lambda: own_action(session, app, "New note"), 60)
                session.invoke("app-actions", action["id"], {"title": "Typing"})
            time.sleep(2)
            session.ctl("snapshot", str(out / f"{name}-before.png"))
            type_keys(session, app, args.warmup + args.keys, args.settle)
            session.ctl("snapshot", str(out / f"{name}-after.png"))
            report[app] = summarize(session.log(), app, args.warmup)
            print(json.dumps({app: report[app]}, indent=2))
            if report[app]["frames"] < args.keys:
                failures.append(f"{app}: {report[app]['frames']} INPUT-FRAME records for "
                                f"{args.keys} keys (RUST_LOG must enable tilo::input_frame=debug)")
            if report[app].get("compositor_full_frames"):
                failures.append(f"{app}: the compositor repainted the whole output "
                                f"{report[app]['compositor_full_frames']} times while keys were typed")
            if report[app]["full_repaints"]:
                failures.append(f"{app}: keys repainted the whole window: "
                                f"{report[app]['full_repaints']}")
    finally:
        (out / "report.json").write_text(json.dumps(report, indent=2))
        if not args.keep and proc.poll() is None:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(60)
    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
