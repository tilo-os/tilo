#!/usr/bin/env python3
"""Terminal in a nested session runs a command in its shell.

Starts dist/dev/nested-session.sh headless with Terminal installed, from the
last build (TILO_NESTED_SESSION_BUILD on t), and through the session's
control socket and the actions people and agents use:

  open    Terminal opens from the launcher and its window stays: the ush
          supervisor answers it (without one Terminal exits with 70)
  run     Terminal's run types `echo ok` into its shell, and Terminal's read
          shows a line `ok` on its screen

A check that cannot observe its subject fails; nothing is skipped. The
session's logs, state dumps, its window list and a snapshot are kept in
--out; its state directory (with the env-runtime, hundreds of MiB) is not.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
TERMINAL = "so.tilo.terminal"
START = 900.0
OPEN = 120.0
# Long enough for the first environment: the supervisor imports the
# env-runtime from the system store on its first use.
RUN = 300.0


def pair_check():
    spec = importlib.util.spec_from_file_location("nested_pair_check", REPO / "dist/dev/nested-pair-check.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PAIR = pair_check()
Failed, Session, wait_for, ok = PAIR.Failed, PAIR.Session, PAIR.wait_for, PAIR.ok


def control_of(state: pathlib.Path) -> str:
    """The control socket nested-session.sh gives a state directory."""
    import hashlib
    digest = hashlib.sha256(str(state).encode()).hexdigest()[:12]
    return f"{os.environ['XDG_RUNTIME_DIR']}/tilo-nested-{digest}/control.sock"


def terminal_tabs(session: Session) -> set:
    tree = session.state()["workspace_tree"]
    out = set()
    for w in tree.get("workspaces", []):
        if str(w.get("room")) != session.room:
            continue
        for strip in w.get("strips", []):
            for group in strip.get("window_groups", []):
                for tab in group.get("tabs", []):
                    if any(isinstance(body, dict) and body.get("app_id") == TERMINAL
                           for body in (tab.get("tile") or {}).values()):
                        out.add(tab["id"])
    return out


def terminal(session: Session, operation: str, args: dict | None = None, timeout: float = OPEN):
    """Invoke Terminal's `operation` in the session's room, looked up again
    while Terminal republishes its surface."""
    def done():
        row = next((row for row in session.catalogue() if row.get("application") == TERMINAL
                    and row.get("room") == session.room and row.get("operation") == operation), None)
        if row is None:
            return None
        value = session.invoke(row, args)
        return None if value is PAIR.Stale else (value,)
    try:
        return wait_for(f"Terminal's {operation}", done, timeout)[0]
    except Failed as failure:
        offered = sorted({str(row.get("operation")) for row in session.catalogue()
                          if row.get("application") == TERMINAL})
        if operation in offered:
            raise Failed(f"Terminal's {operation} in room {session.room}: {failure}") from failure
        raise Failed(f"Terminal offers no {operation} in room {session.room}; it offers {offered}") from failure


def native(state: pathlib.Path) -> bool:
    """Whether nested-session.sh confirmed the Home workspace native: the
    marker it leaves in the state directory once the shell says so."""
    return (state / "native-workspace").is_file()


def tail(path: pathlib.Path, lines: int = 60):
    try:
        kept = path.read_text(errors="replace").splitlines()[-lines:]
    except OSError as error:
        print(f"--- {path}: {error}")
        return
    print(f"--- {path} (last {len(kept)} lines)")
    for line in kept:
        print(f"    {line}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=pathlib.Path, help="where logs, the state dump and snapshots go")
    args = parser.parse_args()
    out = (args.out or REPO / "target/nested-terminal-check" / time.strftime("%Y%m%d-%H%M%S")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    # Outside --out: t keeps a failure's outputs only up to its evidence limit.
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="nested-terminal-", dir=os.environ.get("TMPDIR")))
    state = scratch / "session"
    failed: list[str] = []

    def check(name: str, passed: bool, detail: str):
        print(f"{'ok  ' if passed else 'FAIL'} {name}: {detail}", flush=True)
        if not passed:
            failed.append(name)

    began = time.monotonic()
    log = open(out / "session.log", "a")
    process = subprocess.Popen(
        [str(SESSION), "--state", str(state), "--headless", "--apps", "terminal", "--reset-identity", "--no-build"],
        stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        env=dict(os.environ, XKB_DEFAULT_LAYOUT="us"))
    session = None
    try:
        control = control_of(state)

        def up():
            if process.poll() is not None:
                raise Failed(f"the session exited with {process.returncode} (see {out / 'session.log'})")
            if not os.path.exists(control):
                return None
            try:
                tree = ok(control, {"op": "state_dump"})["state"]["workspace_tree"]
            except (Failed, OSError, ValueError):
                return None
            active = next((w for w in tree.get("workspaces", []) if w.get("id") == tree.get("active")), None)
            return Session("a", control, str(active["room"])) if active and active.get("room") else None
        session = wait_for("the session to answer", up, START)
        print(f"the session is up in {time.monotonic() - began:.0f} s, room {session.room}", flush=True)
        wait_for("the Home workspace to be native", lambda: native(state), START)
        print(f"the Home workspace is native {time.monotonic() - began:.0f} s in", flush=True)

        try:
            row = session.action(lambda row: row.get("provider") == "applications" and row.get("id") == TERMINAL,
                                 "Terminal to open")
            print(f"opening Terminal: {json.dumps(row)[:300]} answered "
                  f"{json.dumps(session.invoke(row), default=str)[:300]}", flush=True)
            tab = wait_for("a Terminal window", lambda: next(iter(terminal_tabs(session)), None), OPEN)
            # Terminal exits at once when the launcher finds no ush socket;
            # its window must still be there once its shell had time to start.
            time.sleep(5)
            check("open", tab in terminal_tabs(session), f"Terminal opened in tab {tab} and stayed")
        except Failed as failure:
            check("open", False, str(failure))

        if not failed:
            try:
                terminal(session, "run", {"command": "echo ok"}, RUN)
                started = time.monotonic()

                def shown():
                    value = terminal(session, "read")
                    text = value.get("text", "") if isinstance(value, dict) else ""
                    return text if any(line.strip() == "ok" for line in text.splitlines()) else None
                text = wait_for("Terminal to show `ok`", shown, RUN)
                check("run", True, f"Terminal shows `ok` {time.monotonic() - started:.1f} s after `echo ok`: "
                                   f"{text.strip().splitlines()[-3:]}")
            except Failed as failure:
                screen = None
                try:
                    screen = terminal(session, "read")
                except Failed:
                    pass
                check("run", False, f"{failure}; the screen reads {json.dumps(screen)[:600]}")
    except Failed as failure:
        check("session", False, str(failure))
    finally:
        if session is not None:
            session.snapshot(out, "end")
            try:
                (out / "windows.json").write_text(json.dumps(ok(session.control, {"op": "windows"}), indent=1))
            except (Failed, OSError, ValueError) as error:
                print(f"   (no window list: {error})")
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=90)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
        logs = state / "logs"
        if logs.is_dir():
            shutil.copytree(logs, out / "logs", dirs_exist_ok=True)
        if failed:
            tail(out / "session.log")
            for name in ("native.log", "ush.log", "shell.log"):
                tail(logs / name, 200)
        shutil.rmtree(scratch, ignore_errors=True)
    print(f"took {time.monotonic() - began:.0f} s", flush=True)
    print("RESULT ok" if not failed else f"RESULT failed: {', '.join(failed)}", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
