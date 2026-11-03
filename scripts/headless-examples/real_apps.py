#!/usr/bin/env python3
"""A headless Tilo session with the tree's own apps, started the way the image starts them.

This is the one way a check gets a desktop where the real apps run: the session
`dist/dev/nested-session.sh` starts, with the tree's apps installed as signed
packages and launched through the session's own `tilo-launcher` and sandbox.
Nothing here starts an app binary directly, and nothing here touches the live
desktop: the session renders on lavapipe alone (`--software`), on a private
runtime directory and a throwaway identity.

The build (`dist/dev/nested-session-build.sh`) makes the session, its programs,
the apps and the runtime library they link; it is made once and every start
reuses it (`--no-build`). With TILO_NESTED_SESSION_BUILD_EXEC set, that build
runs whole through that command prefix (a tool that runs it in a builder's copy
of the worktree and brings `target/nested-session` back), so a machine without
the toolchain or the RAM for it still gets a session.

As a library, `Session` is the session and the tools that act in it:
`Session.start()`, `session.invoke("applications", "so.tilo.notes")` to launch
an app through the desktop, `session.windows()`, `session.snapshot(path)`,
`session.click_named(...)`, `session.type(...)`. `tilolang-e2e.py` and
`canvas-e2e.py` build on it.

As a command:

  # start a session, open Notes through the desktop, write the window bar's note title to a PNG
  python3 scripts/headless-examples/real_apps.py --start --open notes --shot /tmp/notes.png

  # leave it running and drive it with tilo-control from anywhere
  python3 scripts/headless-examples/real_apps.py --start --keep --state /tmp/my-session

`--start` prints its ready JSON (control socket, runtime directory, state
directory, the session script's pid) once the desktop answers with a frame, so
a caller waits for that line instead of guessing. Stop a kept session with
`kill -TERM -<pid>`, which stops its apps, services and bus too.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
# The apps every session installs unless it names its own: what a check of a
# desktop needs to see a person's work in it.
APPS = "notes file-explorer chat settings terminal"
# Where the nested-session build lands (dist/dev/nested-session-build.sh).
BUILD = Path(os.environ.get("TILO_NESTED_SESSION_BUILD") or REPO / "target/nested-session")


class Failed(Exception):
    """A step of the harness did not get what it waited for."""


def app_id(name: str) -> str:
    """The application id of the tree's app `name` (its directory under apps/)."""
    if name == "files":
        name = "file-explorer"
    return f"so.tilo.{name}"


def run(argv, **kwargs) -> str:
    result = subprocess.run(argv, text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise Failed(
            f"{' '.join(map(str, argv))} failed ({result.returncode}): "
            f"{result.stderr.strip()[-2000:]}"
        )
    return result.stdout


def until(what: str, check, deadline: float = 60.0, step: float = 0.25):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(step)
    raise Failed(f"timed out waiting for {what}")


def control_binary() -> Path:
    """`tilo-control` of the build the session runs from."""
    return BUILD / "bin/tilo-control"


def build(*, apps: str = APPS, remote: str | None = None) -> None:
    """Build the session, its programs and `apps` into BUILD."""
    command = [str(REPO / "dist/dev/nested-session-build.sh")]
    argv = [*shlex.split(remote), *command] if remote else command
    env = {**os.environ, "TILO_NESTED_APPS": apps}
    print(f"==> building the session and its apps into {BUILD}", file=sys.stderr)
    result = subprocess.run(argv, cwd=REPO, env=env)
    if result.returncode != 0:
        raise Failed(f"the nested-session build failed ({result.returncode})")


def prepare(state: Path, *, apps: str = APPS, built: bool = False, remote=None) -> None:
    """Install `apps` and their runtime into `state`, ready for a session."""
    if built:
        build(apps=apps, remote=remote)
    if not (BUILD / "bin/tilo-session").is_file():
        raise Failed(
            f"no session build in {BUILD}: run with --build, or "
            "dist/dev/nested-session-build.sh (through a remote exec when this "
            "machine has no toolchain)"
        )
    run([str(SESSION), "--no-build", "--prepare-only", "--apps", apps, "--state", str(state)],
        cwd=REPO)


class Session:
    """One headless session on a state directory, and the tools that act in it."""

    def __init__(self, state: Path, ready: dict | None = None):
        self.state = Path(state).resolve()
        self.proc: subprocess.Popen | None = None
        self.ready = ready
        self.tag = ""

    # -- lifecycle --

    def start(self, *, apps: str = APPS, fresh: bool = True, out: Path | None = None,
              tag: str = "session", tapps: list[Path] | None = None,
              env: dict | None = None) -> "Session":
        """Start the session on its state. `fresh` forgets the saved desktop, so
        the session opens the way a first login does; without it the
        workspaces, windows and their layout come back as they were left."""
        if self.proc is not None:
            raise Failed("the session is running already")
        out = Path(out).resolve() if out else self.state
        out.mkdir(parents=True, exist_ok=True)
        self.tag = tag
        ready = out / f"ready-{tag}.json"
        ready.unlink(missing_ok=True)
        argv = [str(SESSION), "--no-build", "--headless", "--software", "--apps", apps,
                "--state", str(self.state), "--ready", str(ready)]
        if fresh:
            argv.append("--fresh-desktop")
        for tapp in tapps or []:
            argv += ["--install-tapp", str(tapp)]
        print(f"==> starting the headless session ({tag}, lavapipe only)", file=sys.stderr)
        log = open(out / f"session-{tag}.log", "w")
        # The session script makes its own private runtime directory inside
        # `XDG_RUNTIME_DIR`, which a container leaves unset: give it one of
        # the state's own, so nothing here needs a logged-in seat.
        host_runtime = Path(os.environ.get("XDG_RUNTIME_DIR") or self.state / "host-runtime")
        if not host_runtime.is_dir():
            host_runtime.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.proc = subprocess.Popen(
            argv, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
            env={**os.environ, "XDG_RUNTIME_DIR": str(host_runtime),
                 "RUST_LOG": os.environ.get(
                     "RUST_LOG", "warn,tilo::launch_latency=info"), **(env or {})},
        )
        until("the session to start",
              lambda: ready.is_file() or self.proc.poll() is not None, 300, 0.25)
        if self.proc.poll() is not None:
            code = self.proc.returncode
            self.proc = None
            raise Failed(f"the session ended ({code}); see {out / f'session-{tag}.log'}")
        self.ready = json.loads(ready.read_text())
        # A viewer is looking: headless frames are rasterized, and a snapshot
        # then covers what the session painted instead of an empty texture.
        self.control({"op": "watch", "watching": True})
        # The session answers once it has drawn its own desktop; apps start
        # from there.
        until("the session to answer",
              lambda: (self.state / "session-ready").is_file(), 120, 0.2)
        return self

    def stop(self) -> None:
        """Stop the session and everything it started: the script led each of
        its children's process groups, and its own cleanup stops them."""
        proc, self.proc = self.proc, None
        if proc is None:
            return
        if proc.poll() is None:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(60)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(10)
        self.ready = None

    def restart(self) -> "Session":
        self.stop()
        return self.start(fresh=False, tag=f"{self.tag}-again")

    def running(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def __enter__(self) -> "Session":
        return self

    def __exit__(self, *_error) -> None:
        self.stop()

    # -- the tools that act in it --

    def env(self) -> dict:
        """The environment `tilo-control` and `tilo-install` act on this session in."""
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
            TILO_TEST_CONTROL=self.control_socket(),
            XDG_RUNTIME_DIR=self.runtime_directory(),
        )
        env.pop("TILO_APPS_SYSTEM_DIR", None)
        return env

    def control_socket(self) -> str:
        if self.ready is not None:
            return self.ready["control"]
        return str(self.state / "runtime/control.sock")

    def runtime_directory(self) -> str:
        if self.ready is not None:
            return self.ready["runtime"]
        return str(self.state / "runtime")

    def control(self, payload: dict | str, timeout: float = 30.0) -> dict:
        """One request on the test control socket and its reply, whose one key
        names its shape (`ok`, `state`, `answer`, `foreign_frames`, ...); an
        `err` reply raises. `payload` is a dict or its JSON text. See
        crates/session/session/src/test_control.rs for the ops."""
        if isinstance(payload, str):
            payload = json.loads(payload)
        if self.ready is None:
            raise Failed("the session is not running")
        wait = payload.get("wait_ms", 0) / 1000.0
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout + wait)
            s.connect(self.control_socket())
            s.sendall(json.dumps(payload).encode() + b"\n")
            buf = b""
            while b"\n" not in buf:
                chunk = s.recv(1 << 20)
                if not chunk:
                    raise Failed(f"the control socket closed mid-reply to {payload.get('op')}")
                buf += chunk
        reply = json.loads(buf.partition(b"\n")[0])
        if "err" in reply:
            raise Failed(f"{payload.get('op')} refused: {json.dumps(reply['err'])[:2000]}")
        return reply

    def control_cmd(self, *args: str) -> str:
        """`tilo-control` of the build this session runs from, in the session's environment."""
        return run([control_binary(), *args], env=self.env())

    def control_json(self, *args: str):
        return json.loads(self.control_cmd(*args))

    def actions(self, query: str = "") -> list:
        """The actions the desktop lists for `query`, those listed for agents
        alone included ("Show this window"), which a person's palette leaves out."""
        found, cursor = [], None
        while True:
            request = {"op": "actions_enumerate", "query": query, "include_programmatic": True,
                       "wait_ms": 5000, "limit": 1000}
            if cursor:
                request["cursor"] = cursor
            answer = self.control(request)["answer"]
            found += answer["actions"]
            cursor = answer.get("next_cursor")
            if not cursor:
                return found

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        """Invoke an action by its address and answer its settled receipt."""
        payload = {"op": "actions_invoke", "provider": provider, "id": ident, "wait_ms": 20000}
        if args:
            payload["args"] = args
        return self.control(payload)["answer"]

    def windows(self) -> list:
        return self.control({"op": "windows"})["answer"]["windows"]

    def state_dump(self) -> dict:
        return self.control({"op": "state_dump"})["state"]

    def foreign_frames(self) -> list:
        return self.control({"op": "foreign_frames"})["foreign_frames"]["frames"]

    def launch(self, app: str, deadline: float = 120.0) -> dict:
        """Open `app` (its directory under apps/, or its application id) through
        the desktop's own launcher, and answer its window once it is up."""
        ident = app if "." in app else app_id(app)
        action = until(f"{ident} among the desktop's applications", lambda: next(
            (action for action in self.actions(ident)
             if action.get("provider") == "applications" and action.get("id") == ident),
            None), 60, 0.5)
        self.invoke("applications", ident)
        found = until(f"the window of {ident}", lambda: next(
            (w for w in self.windows() if w.get("app_id") == ident), None), deadline)
        return {**found, "title": found.get("title") or action.get("title")}

    def presented(self, app: str, deadline: float = 60.0) -> bool:
        """Whether `app` has presented a frame of its own: its window paints."""
        ident = app if "." in app else app_id(app)
        return until(f"a presented frame of {ident}", lambda: any(
            f.get("app_id") == ident and f.get("presented") and f.get("pixels_ready")
            for f in self.foreign_frames()), deadline, 0.25)

    def click_at(self, x: float, y: float) -> dict:
        return self.control({"op": "click_at", "x": x, "y": y})

    def right_click_at(self, x: float, y: float) -> None:
        self.control({"op": "pointer_move", "x": x, "y": y})
        self.control({"op": "pointer_button", "pressed": True, "button": "right"})
        self.control({"op": "pointer_button", "pressed": False, "button": "right"})

    def click_named(self, name: str) -> dict:
        return self.control({"op": "click_named", "name": name})

    def targets(self) -> list:
        return self.control({"op": "list_targets"})["targets"]

    def type(self, text: str) -> dict:
        return self.control({"op": "type", "text": text})

    def key(self, key: str) -> dict:
        return self.control({"op": "key", "key": key})

    def chord(self, modifier: str, key: str) -> None:
        """`modifier` held while `key` is typed, as Ctrl+N is."""
        self.control({"op": "key_down", "key": modifier})
        try:
            self.key(key)
        finally:
            self.control({"op": "key_up", "key": modifier})

    def snapshot(self, path: Path) -> Path:
        path = Path(path).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        self.control({"op": "snapshot", "path": str(path)})
        until(f"the snapshot {path.name}",
              lambda: path.is_file() and path.stat().st_size > 0, 30, 0.1)
        return path

    def log(self) -> str:
        return (self.state / "logs/session.log").read_text(errors="replace")


def start_session(state: Path, out: Path, tapps: list[Path] | None = None, tag: str = "session",
                  apps: str = APPS, fresh: bool = True, env: dict | None = None):
    """Start a session and answer `(proc, Session)`; `stop_session` stops the one
    it started."""
    session = Session(state)
    session.start(apps=apps, fresh=fresh, out=out, tag=tag, tapps=tapps, env=env)
    return session.proc, session


def stop_session(proc) -> None:
    """Stop the session `start_session` started, and everything it started."""
    if proc is not None and proc.poll() is None:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(60)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(10)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="A headless Tilo session with the tree's apps, started as the image starts them.",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--start", action="store_true", help="start a session")
    parser.add_argument(
        "--state", type=Path,
        default=Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "tilo-real-apps",
        help="state directory (default: $XDG_CACHE_HOME/tilo-real-apps)")
    parser.add_argument("--out", type=Path, help="where logs, the ready JSON and --shot go")
    parser.add_argument("--apps", default=APPS, help=f"apps to install (default: {APPS!r})")
    parser.add_argument("--open", action="append", default=[], metavar="APP",
                        help="open an app through the desktop once the session is up (repeatable)")
    parser.add_argument("--shot", type=Path, help="snapshot the desktop to this PNG")
    parser.add_argument("--fresh", action=argparse.BooleanOptionalAction, default=True,
                        help="forget the saved desktop, as a first login does (default: yes)")
    parser.add_argument("--keep", action="store_true", help="leave the session running")
    parser.add_argument("--build", action="store_true", help="build the session and its apps")
    parser.add_argument("--remote-exec", default=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"),
                        help="run the build through this command prefix (default: "
                             "$TILO_NESTED_SESSION_BUILD_EXEC)")
    options = parser.parse_args()
    if not options.start and not options.shot and not options.open:
        parser.error("nothing to do: pass --start (and --open or --shot to show something)")

    out = options.out or options.state
    try:
        prepare(options.state, apps=options.apps, built=options.build, remote=options.remote_exec)
        session = Session(options.state)
        session.start(apps=options.apps, fresh=options.fresh, out=out)
        for app in options.open:
            window = session.launch(app)
            session.presented(app)
            print(json.dumps({"opened": app, "window": window.get("title")}), file=sys.stderr)
        if options.shot:
            session.snapshot(options.shot)
            print(json.dumps({"shot": str(options.shot)}), file=sys.stderr)
    except Failed as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(json.dumps({**session.ready, "state": str(session.state),
                      "pid": session.proc.pid if session.proc else 0}))
    if not options.keep:
        session.stop()
        return 0
    # Kept: the caller owns the session now, and stops its process group.
    try:
        if session.proc is not None:
            session.proc.wait()
    except KeyboardInterrupt:
        pass
    finally:
        session.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
