#!/usr/bin/env python3
"""Measure tilolang's two development loops in a headless nested session.

The app loop: `tilolang dev` follows a copy of Notes in a session it attaches to. Each run
saves a change to the note editor's placeholder and waits until the running app shows it:

  app         from the save to the app's own first frame with the new build (the line
              `tilolang: <app> draws build N at <unix ms>` the app writes when it draws it)
  compositor  from the save to the first new frame of the app the nested compositor received
              after that (its `frame_seq`), so it includes the app's own rendering on lavapipe

Before the first run a note is created and opened, so the detail, the list's selection and the
window's state are there to keep; after each run the app must have taken the build in place
(`runs the new build, with its state`) and the note must still be open.

The runtime loop: `tilolang dev --build-runtime` runs the apps on a tilolang runtime it builds
from this checkout (through `cargo`, on the build server) and builds again on every change of
the runtime's sources. Each run edits a line of the runtime's source and waits until the app was
restarted on the new runtime and presented a frame, end to end.

The session is headless and renders with lavapipe only (`--software`); its state lives in
OUT/state and the session is stopped at the end. Prints the medians of the runs and writes every
sample to OUT/loops.json.

With `--session READY` it attaches to a session that is already running (its `--ready` file)
instead of starting one, and leaves it running. `--app-only` runs the app loop on the built
runtime without timing the runtime loop; `--skip-runtime` uses the session's own runtime.

  scripts/headless-examples/tilolang-dev-loops.py [--runs 5] [--session READY]
      [--app-only | --skip-runtime]
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

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
BIN = REPO / "target/nested-session/bin"
APP = "so.tilo.lang.notes"
# The line of the runtime's source each runtime run edits: a comment, so the build is a real
# incremental rebuild of the crate with nothing else changed.
RUNTIME_SOURCE = REPO / "apps/tilolang/src/updates.rs"
RUNTIME_MARK = "// tilolang-dev-loops: runtime run"


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
            TILO_RUNTIME_BUNDLE_DIR=str(state / "runtimes"),
            TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=str(state / "keys/runtime-publishers.txt"),
            TILO_TEST_CONTROL=self.control,
            XDG_RUNTIME_DIR=self.runtime,
        )
        env.pop("TILO_APPS_SYSTEM_DIR", None)
        return env

    def control_json(self, *args: str):
        return json.loads(run([BIN / "tilo-control", *args], env=self.env()))

    def frame(self, app: str):
        frames = self.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
        return next((f for f in frames if f["app_id"] == app and f["presented"] and f["pixels_ready"]), None)

    def actions(self, query: str) -> list:
        return self.control_json("actions", "list", query, "--json").get("actions", [])

    def invoke(self, provider: str, ident: str):
        return self.control_json("actions", "invoke", provider, ident, "--wait")

    def log(self) -> str:
        return (self.state / "logs/shell.log").read_text(errors="replace")


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


def tilolang_app_pids(session_pid: int) -> set[int]:
    """The tilolang-app processes under the session."""
    found = set()
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if os.readlink(proc / "exe").endswith("/tilolang-app") and descends(int(proc.name), session_pid):
                found.add(int(proc.name))
        except OSError:
            continue
    return found


def descends(pid: int, ancestor: int) -> bool:
    for _ in range(64):
        if pid == ancestor:
            return True
        try:
            stat = (Path("/proc") / str(pid) / "stat").read_text()
        except OSError:
            return False
        pid = int(stat.rsplit(")", 1)[1].split()[1])
        if pid <= 1:
            return False
    return False


def edit_placeholder(source: Path, text: str):
    body = source.read_text()
    new, n = re.subn(r'placeholder: "Write[^"]*"', f'placeholder: "{text}"', body)
    if n != 1:
        raise Failed("cannot find the note editor's placeholder")
    source.write_text(new)


def app_loop(session: Session, dev_log: Log, shell_log: Log, source: Path, runs: int) -> list[dict]:
    samples = []
    for index in range(runs):
        before = session.frame(APP)
        seq = before["frame_seq"] if before else -1
        text = f"Write something, take {index + 1}"
        saved = time.time()
        edit_placeholder(source, text)
        drawn = until("the app's frame of the new build", lambda: re.search(
            rf"tilolang: {re.escape(APP)} draws build (\d+) at (\d+) ms", shell_log.new() or ""), 30)
        app_ms = int(drawn.group(2)) - saved * 1000
        def received():
            frame = session.frame(APP)
            return frame if frame and frame["frame_seq"] != seq and time.time() * 1000 >= int(drawn.group(2)) else None
        until("the compositor's frame after it", received, 30)
        compositor_ms = (time.time() - saved) * 1000
        # `tilolang dev` says what the app does with the build once tilo-install has returned,
        # which can be after the app already drew it.
        said = []
        def told():
            said.append(dev_log.new())
            text = "".join(said)
            if "starts over" in text or "last good build" in text:
                raise Failed(f"the build was not taken in place: {text[-1000:]}")
            return "takes it with its state" in text
        until("`tilolang dev` to say the app takes the build in place", told, 30)
        samples.append({"app_ms": round(app_ms, 1), "compositor_ms": round(compositor_ms, 1),
                        "build": int(drawn.group(1))})
        print(f"    app loop {index + 1}: app {app_ms:.0f} ms, compositor {compositor_ms:.0f} ms", file=sys.stderr)
        time.sleep(0.5)
    return samples


# The tool, as the nested session's programs are built (dist/dev/nested-session-build.sh).
PROFILE_ARGS = ["--profile", "nested",
                "--config", 'profile.nested.inherits="release"',
                "--config", "profile.nested.lto=false",
                "--config", "profile.nested.codegen-units=16",
                "--config", "profile.nested.incremental=true"]


def runtime_loop(session: Session, session_pid: int, dev_log: Log, runs: int) -> list[dict]:
    original = RUNTIME_SOURCE.read_text()
    samples = []
    try:
        for index in range(runs):
            old = tilolang_app_pids(session_pid)
            started = time.time()
            RUNTIME_SOURCE.write_text(original.rstrip("\n") + f"\n{RUNTIME_MARK} {index + 1} at {started}\n")
            # `tilolang dev` builds the runtime, re-bakes and reinstalls it and restarts the app.
            said = []
            def built_s():
                said.append(dev_log.new())
                found = re.search(r"the runtime built in ([0-9.]+) s", "".join(said))
                return float(found.group(1)) if found else None
            build_s = until("the runtime's build", built_s, 1800, 0.25)
            new = until("the app on the new runtime", lambda: (tilolang_app_pids(session_pid) - old) or None, 120)
            until("a frame of the restarted app", lambda: session.frame(APP), 60)
            total = time.time() - started
            text = "".join(said) + dev_log.new()
            baked = re.findall(r"baked in (\d+) ms, installed in (\d+) ms", text)
            samples.append({"total_s": round(total, 2), "build_s": build_s,
                            "after_build_s": round(total - build_s, 2),
                            "bake_ms": int(baked[-1][0]) if baked else None,
                            "bake_install_ms": int(baked[-1][1]) if baked else None,
                            "pids": sorted(new)})
            print(f"    runtime loop {index + 1}: {total:.1f} s (build {build_s:.1f} s)", file=sys.stderr)
    finally:
        RUNTIME_SOURCE.write_text(original)
    return samples


def start_session(state: Path, out: Path):
    ready = out / "ready.json"
    ready.unlink(missing_ok=True)
    argv = [SESSION, "--no-build", "--headless", "--software", "--fresh-desktop", "--apps", "",
            "--state", state, "--ready", ready]
    log = open(out / "session.log", "w")
    proc = subprocess.Popen([str(a) for a in argv], stdout=log, stderr=subprocess.STDOUT,
                            start_new_session=True)
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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--skip-runtime", action="store_true")
    parser.add_argument("--app-only", action="store_true")
    parser.add_argument("--session", type=Path)
    parser.add_argument("--out", type=Path, default=REPO / "target/tilolang-dev-loops")
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out / "src", ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    state = out / "state"
    tool = REPO / "target/nested/tilolang"
    if not tool.is_file():
        raise Failed(f"no {tool} (cargo build {' '.join(PROFILE_ARGS)} -p tilolang-cli)")
    # A copy of Notes to edit: the repository's sources stay as they are.
    shutil.copytree(REPO / "apps/tl/notes", out / "src/notes", ignore=shutil.ignore_patterns("target"))
    source = out / "src/notes/app.tl"

    result = {}
    session_proc = dev = None
    try:
        if args.session:
            ready = args.session.resolve()
            attached = json.loads(ready.read_text())
            state = Path(attached["state"])
            session = Session(state, attached)
            session_pid = int((state / "session.pid").read_text())
        else:
            run([SESSION, "--no-build", "--prepare-only", "--apps", "", "--state", state])
            session_proc, session, ready = start_session(state, out)
            session_pid = session_proc.pid
        shell_log = Log(state / "logs/shell.log")
        dev_log_path = out / "dev.log"
        dev_argv = [tool, "dev", out / "src/notes", "--session", ready, "--repo", REPO]
        if not args.skip_runtime:
            dev_argv += ["--build-runtime"]
        dev = subprocess.Popen([str(a) for a in dev_argv], stdout=subprocess.DEVNULL,
                               stderr=open(dev_log_path, "w"), start_new_session=True)
        dev_log = Log(dev_log_path)
        dev_log.offset = 0
        def opened():
            if dev.poll() is not None:
                raise Failed(f"tilolang dev ended: {dev_log_path.read_text()[-2000:]}")
            return "is open;" in dev_log_path.read_text()
        until("`tilolang dev` to open the app", opened, 1800, 0.25)
        until("the app's window", lambda: session.frame(APP), 120, 0.25)
        new_note = until("Notes' New note", lambda: next((a for a in session.actions("New note")
                                                          if a.get("title") == "New note"), None), 30, 0.25)
        session.invoke(new_note["provider"], new_note["id"])
        time.sleep(1.0)
        dev_log.new()
        shell_log.new()
        print("==> app loop", file=sys.stderr)
        result["app"] = app_loop(session, dev_log, shell_log, source, args.runs)
        if not (args.skip_runtime or args.app_only):
            print("==> runtime loop", file=sys.stderr)
            result["runtime"] = runtime_loop(session, session_pid, dev_log, args.runs)
    finally:
        stop(dev)
        stop(session_proc)
        (out / "loops.json").write_text(json.dumps(result, indent=2))
    app = result.get("app", [])
    if app:
        print(f"app loop, median of {len(app)}: app frame {statistics.median(s['app_ms'] for s in app):.0f} ms, "
              f"compositor frame {statistics.median(s['compositor_ms'] for s in app):.0f} ms")
    rt = result.get("runtime", [])
    if rt:
        print(f"runtime loop, median of {len(rt)}: "
              f"{statistics.median(s['total_s'] for s in rt):.1f} s end to end, "
              f"build {statistics.median(s['build_s'] for s in rt):.1f} s, "
              f"after the build {statistics.median(s['after_build_s'] for s in rt):.1f} s")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAIL {error}", file=sys.stderr)
        sys.exit(1)
