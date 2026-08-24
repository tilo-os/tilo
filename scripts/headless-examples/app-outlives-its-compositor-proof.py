#!/usr/bin/env python3
"""An application whose compositor goes away shuts down; it does not abort.

Applications were dying by `SIGABRT` during a live session, and the cores
say why:

    thread 'main' panicked at library/std/src/io/stdio.rs:1166:9:
    failed printing to stderr: Broken pipe (os error 32)

Two things had to be true at once, and both were.

The compositor launches an application with its stdout and stderr on pipes
and holds the reading ends. When the compositor exits, those ends close, and
`eprintln!` answers a failed write with a panic — which `panic = "abort"`
answers with `SIGABRT`. Separately, winit answers the dropped Wayland
connection by ending the event loop with the connection's `errno` as an exit
code, and the toolkit called `.unwrap()` on that. Either one alone was fatal.

This drives the whole thing the way it actually happens: a real compositor,
a real application connected to it over a real Wayland socket, its stdout
and stderr arranged the way the compositor's launcher arranges them. Then
the compositor is killed, and the only question asked is how the
application ended.

The application's log is a file here because that is what the launcher now
hands it, and for the same reason: a pipe's reading end dies with the
compositor, and an application writing to a pipe with no reader is one
`println!` from `SIGABRT` — including a `println!` in a library it merely
links. That an application's own diagnostics survive a broken pipe is
pinned separately, by `ui`'s `diagnostics_outlive_a_closed_log_pipe`.

`SIGABRT` here is the bug. A clean exit is the fix. A timeout means the
application never noticed its display had gone, which is its own failure.

Usage:
  ./app-outlives-its-compositor-proof.py [--keep]

Exits non-zero if the application does not shut down cleanly.
"""
from __future__ import annotations

import argparse
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]


def binary(name: str) -> pathlib.Path:
    """Locate a built binary, honouring CARGO_TARGET_DIR.

    Concurrent work in this tree gives each builder its own target
    directory, so the default `target/debug` is often not where the thing
    you just built landed.
    """
    roots = []
    if os.environ.get("CARGO_TARGET_DIR"):
        roots.append(pathlib.Path(os.environ["CARGO_TARGET_DIR"]))
    roots.append(REPO / "target")
    for root in roots:
        for profile in ("debug", "demo", "release"):
            path = root / profile / name
            if path.exists():
                return path
    sys.exit(
        f"no `{name}` binary found under {[str(r) for r in roots]}.\n"
        f"build it first:  cargo build -p shell --bin shell -p calculator"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the session directory behind for inspection")
    args = ap.parse_args()

    shell_bin = binary("shell")
    app_bin = binary("calculator")

    # Short root: a unix socket path has a hard ~108-byte limit, and a deep
    # temp directory silently pushes the compositor's socket past it.
    session = tempfile.mkdtemp(prefix="qs-", dir="/tmp")
    runtime = pathlib.Path(session) / "run"
    runtime.mkdir(parents=True, exist_ok=True)
    runtime.chmod(0o700)

    env = dict(os.environ)
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["XDG_DATA_HOME"] = str(pathlib.Path(session) / "data")
    env["XDG_CONFIG_HOME"] = str(pathlib.Path(session) / "config")
    env.pop("WAYLAND_DISPLAY", None)

    compositor_log = open(pathlib.Path(session) / "compositor.log", "w")
    compositor = subprocess.Popen(
        [str(shell_bin), "--mode", "native", "--backend", "headless"],
        cwd=REPO, env=env, stdout=compositor_log,
        stderr=subprocess.STDOUT,
        preexec_fn=die_with_parent(enabled=not args.keep),
    )
    track(compositor)

    # The compositor writes the socket name it bound, with no trailing
    # newline, so the file's contents are exactly WAYLAND_DISPLAY.
    display_file = runtime / "qos-session" / "wayland-display"
    deadline = time.time() + 90
    while time.time() < deadline:
        if compositor.poll() is not None:
            print(f"compositor exited before binding a socket; see {session}/compositor.log",
                  file=sys.stderr)
            return 1
        if display_file.exists():
            break
        time.sleep(0.1)
    else:
        print(f"compositor never bound a socket; see {session}/compositor.log",
              file=sys.stderr)
        compositor.kill()
        return 1

    display = display_file.read_text().strip()
    print(f"compositor up on WAYLAND_DISPLAY={display}")

    app_env = dict(env)
    app_env["WAYLAND_DISPLAY"] = display
    app_log_path = pathlib.Path(session) / "app.log"
    app_log = open(app_log_path, "w")
    app = subprocess.Popen(
        [str(app_bin)],
        cwd=REPO, env=app_env,
        stdout=app_log, stderr=subprocess.STDOUT,
        preexec_fn=die_with_parent(enabled=not args.keep),
    )
    track(app)

    # Give the application time to connect, get its surface configured and
    # paint. If it has not settled before the compositor goes, the test is
    # about startup rather than shutdown.
    time.sleep(6.0)
    if app.poll() is not None:
        print(f"application exited before the compositor did: {app.returncode}",
              file=sys.stderr)
        compositor.kill()
        return 1
    print("application running and connected")

    # The display goes away, which is what the person did when they
    # could not get out of a stuck window.
    compositor.send_signal(signal.SIGTERM)
    try:
        compositor.wait(timeout=20)
    except subprocess.TimeoutExpired:
        compositor.kill()
        compositor.wait(timeout=10)
    print("compositor gone")

    try:
        code = app.wait(timeout=30)
    except subprocess.TimeoutExpired:
        app.kill()
        print("FAIL: the application never noticed its display had gone",
              file=sys.stderr)
        return 1

    def show_log():
        app_log.close()
        text = app_log_path.read_text(errors="replace").strip()
        print("---- the application's log ----", file=sys.stderr)
        print(text[-3000:] or "(empty)", file=sys.stderr)

    if code < 0:
        name = signal.Signals(-code).name
        print(f"FAIL: the application was killed by {name} "
              f"(this is the crash: a dying session must not abort its apps)",
              file=sys.stderr)
        show_log()
        return 1

    if code != 0:
        print(f"FAIL: the application exited {code}; losing a display is an "
              f"ordinary shutdown and should be reported as one", file=sys.stderr)
        show_log()
        return 1

    print("PASS: the application shut down cleanly when its compositor went away")
    if not args.keep:
        import shutil
        shutil.rmtree(session, ignore_errors=True)
    else:
        print(f"session kept at {session}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
