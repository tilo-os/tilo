#!/usr/bin/env python3
"""Open an app, time it to its first correct frame, close it, time it to exit.

Drives Tilo apps, host Wayland apps and host X11 apps through the same
action surface an agent uses (`applications` / `host-applications`), in an
isolated headless session on a private D-Bus, and reports per app:

  first_surface   the toplevel exists (placeholder may be up)
  first_frame     the client committed a buffer the shell captured
  correct         presented, and the buffer matches the tile it was laid into
  flashes         buffer/tile disagreements seen after the first correct frame
  close_to_exit   window closed -> every process the launch started is gone
  leftovers       processes still alive 10 s after the close, with state

A PNG is written at the first frame and once settled, for each app.

Usage:
  ./app-lifecycle-timing.py [--size 5120x2880@2] TARGET...
  TARGET is PROVIDER:ID, e.g. applications:so.tilo.file-explorer or
  host-applications:Alacritty
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
POLL = 0.02
LINGER = 10.0


def now_ms(t0: float) -> int:
    return int((time.monotonic() - t0) * 1000)


class Session:
    def __init__(self, root: str, size: str):
        self.dir = root
        self.socket = os.path.join(root, "control.sock")
        self.runtime = os.path.join(root, "run")
        os.makedirs(self.runtime, mode=0o700, exist_ok=True)
        env = {
            **os.environ,
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(root, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(root, "data"),
            "XDG_CACHE_HOME": os.path.join(root, "cache"),
            "XDG_STATE_HOME": os.path.join(root, "state"),
            "XDG_CONFIG_HOME": os.path.join(root, "config"),
            "XDG_RUNTIME_DIR": self.runtime,
            "TILO_IDENTITY_ROOT": os.path.join(root, "identity"),
            "TILO_PEER_KEY_PATH": os.path.join(root, "identity", "device", "keypair.bin"),
            "TILO_USER_ID": "lifecycle",
            "TILO_HEADLESS_SIZE": size,
            "TILO_IDLE_BLANK_SECS": "0",
            "TILO_IDLE_LOCK_SECS": "0",
            "TILO_IDLE_SUSPEND_SECS": "0",
            "RUST_LOG": os.environ.get("RUST_LOG", "info"),
        }
        for var in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS",
                    "XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"):
            env.pop(var, None)
        env["PATH"] = f"{SHELL.parent}:{env.get('PATH', '')}"
        self.env = env
        self.log_path = os.path.join(root, "shell.log")
        self.log = open(self.log_path, "w")
        # What onboarding leaves behind: native apps refuse to start without it.
        subprocess.run([str(SHELL), "--firstboot", "--provision-identity"], env=env,
                       stdout=self.log, stderr=subprocess.STDOUT, check=True)
        # A private bus: a host app must never reach the real desktop's.
        self.proc = subprocess.Popen(
            ["dbus-run-session", "--", str(SHELL), "--backend", "headless"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(), start_new_session=True,
        )

    def wait_up(self, timeout: float = 120.0) -> bool:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if os.path.exists(self.socket):
                try:
                    self.call({"op": "foreign_frames"})
                    return True
                except OSError:
                    pass
            if self.proc.poll() is not None:
                return False
            time.sleep(0.2)
        return False

    def call(self, payload: dict, timeout: float = 60.0) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.socket)
        try:
            s.sendall((json.dumps(payload) + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(1 << 20)
                if not chunk:
                    break
                buf += chunk
        finally:
            s.close()
        return json.loads(buf.decode())

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def actions(self, query: str = "") -> list[dict]:
        r = self.call({"op": "actions_enumerate", "query": query})
        return r.get("answer", {}).get("actions", [])

    def publish_display_to_bus(self) -> None:
        """What a Tilo login does, confined to this session's own bus."""
        sockets = sorted(p for p in os.listdir(self.runtime)
                         if p.startswith("wayland-") and not p.endswith(".lock"))
        bus = session_bus_address(self.proc.pid)
        if sockets and bus:
            subprocess.run(
                ["dbus-update-activation-environment",
                 f"WAYLAND_DISPLAY={sockets[0]}", f"XDG_RUNTIME_DIR={self.runtime}"],
                env={**self.env, "DBUS_SESSION_BUS_ADDRESS": bus}, check=False)

    def session_pids(self) -> dict[int, str]:
        """Every live process of this user running under this session's
        runtime dir, except the compositor, the bus and what the bus
        activated (portals belong to the session, not to the app whose
        launch first asked for them)."""
        skip = {self.proc.pid} | descendants_named(
            self.proc.pid, {"shell", "dbus-daemon", "dbus-broker", "dbus-broker-lau"})
        out = {}
        for pid in all_pids():
            if pid in skip or pid == os.getpid() or bus_activated(pid):
                continue
            if read_environ(pid).get("XDG_RUNTIME_DIR") == self.runtime:
                out[pid] = comm(pid)
        return out

    def stop(self):
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)
            self.proc.wait(timeout=8)
        except Exception:
            try:
                os.killpg(self.proc.pid, signal.SIGKILL)
            except Exception:
                pass
        for pid in self.session_pids():
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        self.log.close()


def all_pids() -> list[int]:
    return [int(p) for p in os.listdir("/proc") if p.isdigit()]


def read_environ(pid: int) -> dict[str, str]:
    try:
        raw = pathlib.Path(f"/proc/{pid}/environ").read_bytes()
    except OSError:
        return {}
    env = {}
    for item in raw.split(b"\0"):
        k, sep, v = item.partition(b"=")
        if sep:
            env[k.decode(errors="replace")] = v.decode(errors="replace")
    return env


def comm(pid: int) -> str:
    try:
        return pathlib.Path(f"/proc/{pid}/comm").read_text().strip()
    except OSError:
        return "?"


def ppid(pid: int) -> int:
    try:
        stat = pathlib.Path(f"/proc/{pid}/stat").read_text()
        return int(stat.rsplit(")", 1)[1].split()[1])
    except (OSError, IndexError, ValueError):
        return 0


def bus_activated(pid: int) -> bool:
    """Whether the bus started `pid`. The daemon double-forks activated
    services, so ancestry cannot tell; it does mark their environment."""
    return "DBUS_STARTER_ADDRESS" in read_environ(pid)


def descendants_named(root: int, names: set[str] | None) -> set[int]:
    """Descendants of `root` whose comm is in `names` (all, for None)."""
    kids: dict[int, list[int]] = {}
    for pid in all_pids():
        kids.setdefault(ppid(pid), []).append(pid)
    out, stack = set(), [root]
    while stack:
        for child in kids.get(stack.pop(), []):
            stack.append(child)
            if names is None or comm(child) in names:
                out.add(child)
    return out


def thread_states(pid: int) -> list[str]:
    out = []
    try:
        tids = sorted(int(t) for t in os.listdir(f"/proc/{pid}/task"))
    except OSError:
        return out
    for tid in tids:
        try:
            name = pathlib.Path(f"/proc/{pid}/task/{tid}/comm").read_text().strip()
            wchan = pathlib.Path(f"/proc/{pid}/task/{tid}/wchan").read_text().strip()
        except OSError:
            continue
        out.append(f"{name}:{wchan}")
    return out


def session_bus_address(root: int) -> str | None:
    for pid in [root, *descendants_named(root, {"shell"})]:
        addr = read_environ(pid).get("DBUS_SESSION_BUS_ADDRESS")
        if addr:
            return addr
    return None


def proc_state(pid: int) -> str | None:
    try:
        status = pathlib.Path(f"/proc/{pid}/status").read_text()
        wchan = pathlib.Path(f"/proc/{pid}/wchan").read_text().strip()
    except OSError:
        return None
    st = re.search(r"State:\s+(\S)", status)
    if st and st.group(1) in ("Z", "X"):
        return None
    rss = re.search(r"VmRSS:\s+(\d+)", status)
    threads = re.search(r"Threads:\s+(\d+)", status)
    return (f"{comm(pid)} state={st.group(1) if st else '?'} wchan={wchan} "
            f"rss={int(rss.group(1)) // 1024 if rss else 0}MB "
            f"threads={threads.group(1) if threads else '?'}")


def close_rows(session: Session) -> set[str]:
    return {a["id"] for a in session.actions("")
            if a.get("provider") == "desktop-windows" and a.get("id", "").startswith("tab.close:")}


def notices(session: Session) -> set[str]:
    dump = json.dumps(session.call({"op": "state_dump"}))
    return set(re.findall(r"Couldn(?:'|\\u2019)t [^\"\\]{0,80}", dump))


def size_matches(frame: dict, scale: float) -> bool:
    rect, buf = frame.get("rect"), frame.get("buffer")
    if not rect or not buf:
        return False
    return (abs(buf["w"] - rect["w"] * scale) <= 2 * scale
            and abs(buf["h"] - rect["h"] * scale) <= 2 * scale)


def run_one(session: Session, target: str, shots: str, scale: float) -> dict:
    provider, _, ident = target.partition(":")
    before_surfaces = {f["surface_id"] for f in session.frames()}
    before_rows = close_rows(session)
    before_pids = set(session.session_pids())
    before_notices = notices(session)
    rep: dict = {"target": target}
    t0 = time.monotonic()
    resp = session.call({"op": "actions_invoke", "provider": provider, "id": ident})
    rep["invoke_ms"] = now_ms(t0)
    if "err" in resp:
        rep["error"] = resp["err"]
        return rep
    surface, history, flashes, first = None, [], 0, False
    deadline = t0 + 45
    while time.monotonic() < deadline:
        new = [f for f in session.frames() if f["surface_id"] not in before_surfaces]
        if new:
            if surface is None:
                surface = new[0]["surface_id"]
                rep["first_surface_ms"] = now_ms(t0)
                rep["app_id"] = new[0].get("app_id")
            f = next((x for x in new if x["surface_id"] == surface), new[0])
            sig = (f.get("frame_seq", 0) > 0, f.get("presented"), f.get("opening"),
                   f.get("buffer"), f.get("rect"))
            if not history or history[-1][1:] != sig:
                history.append((now_ms(t0), *sig))
            if f.get("frame_seq", 0) > 0 and not first:
                first = True
                rep["first_frame_ms"] = now_ms(t0)
                session.call({"op": "snapshot", "path": os.path.join(shots, f"{ident}-first.png")})
            ok = f.get("presented") and size_matches(f, scale)
            if first and ok:
                rep.setdefault("correct_ms", now_ms(t0))
            elif "correct_ms" in rep and not ok:
                flashes += 1
            if "correct_ms" in rep and now_ms(t0) - rep["correct_ms"] > 1500:
                break
        time.sleep(POLL)
    rep["history"] = history
    rep["flashes_after_correct"] = flashes
    rep["surfaces"] = len([f for f in session.frames() if f["surface_id"] not in before_surfaces])
    session.call({"op": "snapshot", "path": os.path.join(shots, f"{ident}-settled.png")})
    launched = {pid: n for pid, n in session.session_pids().items() if pid not in before_pids}
    rep["processes"] = {pid: proc_state(pid) for pid in launched}
    opened = notices(session) - before_notices
    if opened:
        rep["notices_on_open"] = sorted(opened)
    rows = close_rows(session) - before_rows
    if not rows:
        rep["close"] = "no close action for the new window"
        return rep
    t1 = time.monotonic()
    for row in sorted(rows):
        session.call({"op": "actions_invoke", "provider": "desktop-windows", "id": row})
    while time.monotonic() - t1 < LINGER:
        if not any(proc_state(pid) for pid in launched):
            rep["close_to_exit_ms"] = int((time.monotonic() - t1) * 1000)
            break
        time.sleep(POLL)
    else:
        rep["leftovers"] = {pid: s for pid in launched if (s := proc_state(pid))}
        rep["leftover_threads"] = {
            f"{child} {comm(child)}": thread_states(child)
            for pid in rep["leftovers"]
            for child in sorted(descendants_named(pid, None))
        }
        if shutil.which("eu-stack"):
            for pid in rep["leftovers"]:
                for child in sorted(descendants_named(pid, None)) or [pid]:
                    if comm(child) in ("bwrap", "xdg-dbus-proxy"):
                        continue
                    path = os.path.join(shots, f"{ident}-{child}.stack")
                    with open(path, "w") as out:
                        subprocess.run(["eu-stack", "-p", str(child)], stdout=out,
                                       stderr=subprocess.STDOUT, timeout=60, check=False)
                    rep.setdefault("stacks", []).append(path)
        for pid in rep["leftovers"]:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
    time.sleep(0.5)
    late = notices(session) - before_notices - opened
    if late:
        rep["notices_after_close"] = sorted(late)
    return rep


def main() -> int:
    global SHELL
    ap = argparse.ArgumentParser()
    ap.add_argument("--size", default="1920x1080", help="WxH[@SCALE], e.g. 5120x2880@2")
    ap.add_argument("--desktop", action="append", default=[], metavar="ID=EXEC",
                    help="install a session-local host entry, e.g. glxgears=glxgears")
    ap.add_argument("--list", action="store_true", help="print what the session offers and stop")
    ap.add_argument("--shell", type=pathlib.Path, default=SHELL,
                    help="a compositor that honours TILO_TEST_CONTROL (debug or dev-overrides); "
                         "tilo-launcher is taken from beside it")
    ap.add_argument("targets", nargs="*")
    args = ap.parse_args()
    SHELL = args.shell.resolve()
    if not SHELL.exists():
        print(f"missing {SHELL}; cargo build -p shell --bin shell -p launcher --bin tilo-launcher")
        return 2
    root = tempfile.mkdtemp(prefix="/tmp/qlt-")
    shots = os.path.join(root, "shots")
    os.makedirs(shots)
    apps_dir = os.path.join(root, "s", "data", "applications")
    os.makedirs(apps_dir)
    for spec in args.desktop:
        ident, _, exe = spec.partition("=")
        with open(os.path.join(apps_dir, f"{ident}.desktop"), "w") as f:
            f.write(f"[Desktop Entry]\nType=Application\nName={ident}\nExec={exe}\n")
    session = Session(os.path.join(root, "s"), args.size)
    scale = float(args.size.partition("@")[2].split("~")[0] or 1)
    reports = []
    try:
        if not session.wait_up():
            print(open(session.log_path).read()[-4000:])
            return 1
        session.publish_display_to_bus()
        for _ in range(40):
            apps = [a for a in session.actions("")
                    if a.get("provider") in ("applications", "host-applications")]
            if apps:
                break
            time.sleep(0.5)
        if args.list:
            for a in apps:
                print(a.get("provider"), a.get("id"), "|", a.get("title"))
        for target in args.targets:
            rep = run_one(session, target, shots, scale)
            reports.append(rep)
            print(json.dumps(rep, default=str), flush=True)
    finally:
        session.stop()
        print(f"session: {root}  log: {session.log_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
