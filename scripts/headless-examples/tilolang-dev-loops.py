#!/usr/bin/env python3
"""Measure tilolang's two development loops in a headless nested session.

The app loop: `tilolang dev` follows a copy of Notes in a session it attaches to. Each run
saves a change to the note editor's placeholder and waits until the running app shows it:

  app         from the save to the app's own first frame with the new build (the line
              `tilolang: <app> draws build N at <unix ms>` the app writes when it draws it)
  presented   from the save to the first frame of the new build the compositor presented, the
              first `tilo-display-frame` row the app logs after its draw

Before the first run a note is created and opened, so the detail, the list's selection and the
window's state are there to keep; after each run the app must have taken the build in place
(`runs the new build, with its state`) and the note must still be open.

The session is headless and renders with lavapipe only (`--software`); its state lives in
OUT/state and the session is stopped at the end. Prints the medians of the runs and writes every
sample to OUT/loops.json.

With `--session READY` it attaches to a session that is already running (its `--ready` file)
instead of starting one, and leaves it running.

  scripts/headless-examples/tilolang-dev-loops.py [--runs 5] [--session READY]
      [--no-build] [--busy N]
"""

import argparse
import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path

from real_apps import address, catalogue, invocation, receipt

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
BIN = REPO / "target/nested-session/bin"
APP = "so.tilo.notes"
# The string each package under test shows, so a save paints something new: Notes' body
# placeholder in a session.
PLACEHOLDER = {"notes": r'placeholder: "Write[^"]*"',
               "tasks": r'placeholder: "New task(, [^"]*)?"'}
# The app's own line when it draws a new build, and the row it logs when the compositor presents
# a frame (`TILO_PROFILE_PRESENTATION=1`).
DRAW = re.compile(r"tilolang: (\S+) draws build (\d+) at (\d+) ms, (\d+) ms after it arrived")
PRESENTED = re.compile(r"tilo-display-frame \{.*\}")
BUILT = re.compile(r"built in (\d+) ms, (\w+) in (\d+) ms")


class Failed(Exception):
    pass


def run(argv, **kwargs) -> str:
    result = subprocess.run([str(a) for a in argv], text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise Failed(f"{' '.join(map(str, argv))} failed ({result.returncode}): {result.stderr.strip()[-2000:]}")
    return result.stdout


def until(what: str, check, deadline: float = 60.0, step: float = 0.01):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(step)
    raise Failed(f"timed out waiting for {what}")


class Session:
    def __init__(self, state: Path, ready: dict):
        self.state = state
        self.control = ready["control"]
        self.runtime = ready["runtime"]

    def env(self) -> dict:
        state = self.state
        env = dict(os.environ)
        env.update(
            HOME=str(state / "home"),
            XDG_CONFIG_HOME=str(state / "config"),
            XDG_DATA_HOME=str(state / "data"),
            XDG_STATE_HOME=str(state / "state"),
            XDG_CACHE_HOME=str(state / "cache"),
            TILO_IDENTITY_ROOT=str(state / "identity"),
            TILO_APPS_DATA_DIR=str(state / "apps"),
            TILO_RUNTIME_BUNDLE_DIR=str(state / "apps/runtimes"),
            TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=str(state / "keys/runtime-publishers.txt"),
            TILO_TEST_CONTROL=self.control,
            XDG_RUNTIME_DIR=self.runtime,
        )
        env.pop("TILO_APPS_SYSTEM_DIR", None)
        return env

    def control_json(self, *args: str):
        return json.loads(run([BIN / "tilo-control", *args], env=self.env()))

    def frame(self, message: dict) -> dict:
        return self.control_json("raw", json.dumps(message))

    def actions(self, query: str) -> list:
        return catalogue(self.frame, query)

    def invoke(self, row: dict):
        return receipt(self.frame(invocation(row["provider"], *address(row), None, 10000)))

    def log(self) -> str:
        return (self.state / "logs/session.log").read_text(errors="replace")


class Log:
    """A growing log file, read from where the last read stopped."""

    def __init__(self, path: Path):
        self.path = path
        self.offset = path.stat().st_size if path.exists() else 0

    def new(self) -> str:
        if not self.path.exists():
            return ""
        with self.path.open(errors="replace") as file:
            file.seek(self.offset)
            text = file.read()
        self.offset += len(text.encode())
        return text


class Tail:
    """The complete lines a growing file gained since the last read."""

    def __init__(self, path: Path):
        self.path = path
        self.offset = 0
        self.partial = b""

    def lines(self) -> list[str]:
        if not self.path.exists():
            return []
        with self.path.open("rb") as file:
            file.seek(self.offset)
            data = self.partial + file.read()
            self.offset = file.tell()
        self.partial = b""
        cut = data.rfind(b"\n")
        if cut < 0:
            self.partial = data
            return []
        self.partial = data[cut + 1:]
        return data[:cut].decode(errors="replace").split("\n")


def edit_placeholder(source: Path, text: str, package: str = "notes"):
    body = source.read_text()
    new, n = re.subn(PLACEHOLDER[package], f'placeholder: "{text}"', body)
    if n != 1:
        raise Failed(f"cannot find {package}'s placeholder")
    # Replace the file whole, as an editor's save does: a writer that truncates it lets the
    # watcher read half a package.
    saving = source.with_name(source.name + ".save")
    saving.write_text(new)
    os.replace(saving, source)


def wait_for_build(reader, saved: float, timeout: float = 60.0) -> tuple[dict, list[str]]:
    """The app's first frame of the build a save makes, and the first frame it presented after.

    `reader` hands over the lines the app's log gained since the last call.
    """
    drawn = None
    seen = []

    def watch():
        nonlocal drawn
        for line in reader():
            seen.append(line)
            found = DRAW.search(line)
            if drawn is None and found and int(found.group(3)) >= saved * 1000:
                drawn = found
                continue
            if drawn is not None and PRESENTED.search(line):
                return time.time()
        return None

    presented = until("the app's first presented frame of the new build", watch, timeout, 0.001)
    return {
        "build": int(drawn.group(2)),
        "draw_ms": round(int(drawn.group(3)) - saved * 1000, 1),
        "arrival_ms": int(drawn.group(4)),
        "presented_ms": round((presented - saved) * 1000, 1),
    }, seen


def app_loop(dev_log: Log, shell_log: Log, source: Path, runs: int) -> list[dict]:
    samples = []
    for index in range(runs):
        text = f"Write something, take {index + 1}"
        saved = time.time()
        edit_placeholder(source, text)
        sample, seen = wait_for_build(lambda: shell_log.new().splitlines(), saved)
        # `tilolang dev` says what the app does with the build once tilo-install has returned,
        # which can be after the app already drew it.
        said = []
        def told():
            said.extend(seen)
            said.append(dev_log.new())
            text = "".join(said)
            if "starts over" in text or "last good build" in text:
                raise Failed(f"the build was not taken in place: {text[-1000:]}")
            return "takes it with its state" in text
        until("`tilolang dev` to say the app takes the build in place", told, 30)
        samples.append(sample)
        print(f"    app loop {index + 1}: app {sample['draw_ms']:.0f} ms, "
              f"presented {sample['presented_ms']:.0f} ms", file=sys.stderr)
        time.sleep(0.5)
    return samples


# The tool, as the nested session's programs are built (dist/dev/nested-session-build.sh).
PROFILE_ARGS = ["--profile", "nested",
                "--config", 'profile.nested.inherits="release"',
                "--config", "profile.nested.lto=false",
                "--config", "profile.nested.codegen-units=16",
                "--config", "profile.nested.incremental=true"]


def start_session(state: Path, out: Path):
    unattach(state)
    ready = out / "ready.json"
    ready.unlink(missing_ok=True)
    argv = [SESSION, "--no-build", "--headless", "--software", "--fresh-desktop", "--apps", "",
            "--state", state, "--ready", ready]
    log = open(out / "session.log", "w")
    # The apps log the frame the compositor presents (see `wait_for_build`).
    env = dict(os.environ, TILO_PROFILE_PRESENTATION="1")
    proc = subprocess.Popen([str(a) for a in argv], stdout=log, stderr=subprocess.STDOUT,
                            env=env, start_new_session=True)
    until("the session", lambda: ready.is_file() or proc.poll() is not None, 300, 0.5)
    if proc.poll() is not None:
        raise Failed(f"the session ended ({proc.returncode}); see {out / 'session.log'}")
    return proc, Session(state, json.loads(ready.read_text())), ready


def stop(proc):
    if proc is not None and proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(60)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)


def unattach(state: Path):
    """Leave no stale mount of a session that died, so the same state starts again."""
    # The launcher picks the runtime directory (a login's XDG_RUNTIME_DIR, or one of its own
    # where there is none); asking it keeps one derivation of that path.
    runtime = Path(subprocess.run([str(SESSION), "--print-runtime", "--state", str(state)],
                                  capture_output=True, text=True, check=True).stdout.strip())
    for line in Path("/proc/self/mounts").read_text().splitlines():
        mount = line.split()[1]
        if mount.startswith(str(runtime) + "/"):
            subprocess.run(["fusermount3", "-u", mount], capture_output=True)
    shutil.rmtree(runtime, ignore_errors=True)


def start_busy(count: int) -> list:
    return [subprocess.Popen(["yes"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(count)]


def stop_all(processes: list):
    for proc in processes:
        proc.terminate()
    for proc in processes:
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--session", type=Path)
    parser.add_argument("--out", type=Path, default=REPO / "target/tilolang-dev-loops")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--busy", type=int, default=0)
    parser.add_argument("--tool", type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out / "src", ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    state = out / "state"
    tool = (args.tool or REPO / "target/nested/tilolang").resolve()
    if not args.no_build:
        # The tool of this checkout: one built before would time the loops of older code.
        run(["cargo", "build", *PROFILE_ARGS, "-p", "tilolang-cli", "--bin", "tilolang"], cwd=REPO)
    if not tool.is_file():
        raise Failed(f"no {tool} (cargo build {' '.join(PROFILE_ARGS)} -p tilolang-cli)")
    # A copy of the package to edit: the repository's sources stay as they are.
    package = "notes"
    shutil.copytree(REPO / "apps/tl" / package, out / "src" / package,
                    ignore=shutil.ignore_patterns("target"))
    source = out / "src" / package / "app.tl"

    result = {}
    session_proc = dev = None
    busy = start_busy(args.busy)
    try:
        if args.session:
            ready = args.session.resolve()
            attached = json.loads(ready.read_text())
            state = Path(attached["state"])
            session = Session(state, attached)
        else:
            run([SESSION, "--no-build", "--prepare-only", "--apps", "", "--state", state])
            session_proc, session, ready = start_session(state, out)
        shell_log = Log(state / "logs/session.log")
        dev_log_path = out / "dev.log"
        dev_argv = [tool, "dev", out / "src/notes", "--session", ready, "--repo", REPO]
        dev = subprocess.Popen([str(a) for a in dev_argv], stdout=subprocess.DEVNULL,
                               stderr=open(dev_log_path, "w"), start_new_session=True)
        dev_log = Log(dev_log_path)
        dev_log.offset = 0
        def opened():
            if dev.poll() is not None:
                raise Failed(f"tilolang dev ended: {dev_log_path.read_text()[-2000:]}")
            return "is open;" in dev_log_path.read_text()
        until("`tilolang dev` to open the app", opened, 1800, 0.25)
        shell_log.new()
        until("the app's window",
              lambda: any(PRESENTED.search(line) for line in shell_log.new().splitlines()), 120, 0.25)
        new_note = until("Notes' New note", lambda: next((a for a in session.actions("New note")
                                                          if (a.get("action") or {}).get("action_label") == "New note"),
                                                         None), 30, 0.25)
        session.invoke(new_note)
        time.sleep(1.0)
        dev_log.new()
        shell_log.new()
        print("==> app loop", file=sys.stderr)
        result["app"] = app_loop(dev_log, shell_log, source, args.runs)
    finally:
        stop(dev)
        stop(session_proc)
        stop_all(busy)
        (out / "loops.json").write_text(json.dumps(result, indent=2))
    app = result.get("app", [])
    if app:
        print(f"app loop, median of {len(app)}: draws {statistics.median(s['draw_ms'] for s in app):.0f} ms, "
              f"presented {statistics.median(s['presented_ms'] for s in app):.0f} ms")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAIL {error}", file=sys.stderr)
        sys.exit(1)
