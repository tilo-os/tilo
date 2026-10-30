#!/usr/bin/env python3
"""tilolang apps end to end, as real and independent Tilo apps.

Builds the notes, chat, todo and today packages with `tilolang build`, installs
them into a headless nested Tilo session (dist/dev/nested-session.sh) that
renders on lavapipe and never opens this machine's GPU (`--software`), and
checks through the semantic control surface (tilo-control) what an installed
app gets on a real system:

  registry   each package is an application under its own id, name and category
  launch     each runs its own process from the tilolang runtime bundle, in its
             own sandbox (user, mount and pid namespaces of its own), and the
             compositor admits its window under its own app id: a sandboxed
             client reaches the compositor only through verified launch custody
  gpu        the shell, its render service and every app load no Vulkan driver
             but lavapipe
  actions    each app publishes its actions; a "New ..." action changes the
             app's record (its history gains the change)
  hosted-here the session's own workspace is hosted by this device: no app waits
             on a host, each shows its own state as first opened (first-*.png)
  snapshot   a PNG of the nested session per app, in OUT
  native     native Notes (so.tilo.notes), installed beside them, launches and
             presents a frame (native-notes.png)
  activation "Show this window" answers for tilolang Notes as for native Notes
  read       the door's `read` of tilolang Notes lists the room's records, and
             the notebook's read lists its notes by the ids a read takes
  idle       each app untouched for 10 s, native Notes included: one that commits no
             frame wakes no thread at all; one that animates commits one frame per
             pose and no more
  routes     the broker's route access, as the attested launch of an app: the
             collaboration probe (crates/dev/collab-probe) is installed as
             Today, whose surface declares that it reads Notes' kinds, and as
             Todo, which declares no read. Today joins Notes' route read-only
             and every write there is refused; Todo cannot join it at all.
             This runs in a second start of the same throwaway session, with
             the probe packages installed in place of Today and Todo.

Usage:
  scripts/headless-examples/tilolang-e2e.py [--no-build] [--out DIR] [--keep]

Without --no-build it builds the nested session remotely (nested-session.sh:
with cargo), and the tilolang tool and the probe with cargo. Each run
uses a fresh throwaway session state in OUT/state and removes its compositor,
runtime directory and processes at the end; --keep leaves the session running
(stop it with `kill $(cat OUT/state/session.pid)`). The verdict is OUT/summary.json; the
exit status is non-zero when a check fails. Never touches the live desktop.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
BIN = REPO / "target/nested-session/bin"
APPS = ["notes", "chat", "tasks", "today"]
# The native app installed beside them, to hold the tilolang apps to what it does.
NATIVE = "so.tilo.notes"
PROBE_RUNTIME = "0.1.0+probe"
# The routes the probe tries as each app: Today declares that it reads Notes'
# kinds; Todo declares no read.
PROBE_ROUTES = {
    "today": ["so.tilo.lang.notes", "so.tilo.lang.today"],
    "todo": ["so.tilo.lang.notes"],
}


def app_id(name: str) -> str:
    return f"so.tilo.lang.{name}"


class Failed(Exception):
    pass


def run(argv, **kwargs) -> str:
    result = subprocess.run(argv, text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise Failed(f"{' '.join(map(str, argv))} failed ({result.returncode}): {result.stderr.strip()[-2000:]}")
    return result.stdout


def until(what: str, check, deadline: float = 60.0, step: float = 0.25):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(step)
    raise Failed(f"timed out waiting for {what}")


class Session:
    """One headless nested session and the environment its tools act in."""

    def __init__(self, state: Path, ready: dict):
        self.pid = 0
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

    def control_cmd(self, *args: str) -> str:
        return run([BIN / "tilo-control", *args], env=self.env())

    def control_json(self, *args: str):
        return json.loads(self.control_cmd(*args))

    def actions(self, query: str = "") -> list:
        # As an agent or a script lists them: with the actions listed for agents alone
        # ("Show this window"), which a person's palette leaves out.
        return self.control_json("actions", "list", query, "--json", "--programmatic").get("actions", [])

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        argv = ["actions", "invoke", provider, ident, "--wait"]
        if args is not None:
            argv += ["--args-json", json.dumps(args)]
        return self.control_json(*argv)

    def windows(self) -> list:
        return self.control_json("windows", "--json")

    def log(self) -> str:
        return (self.state / "logs/shell.log").read_text(errors="replace")

# --- /proc --------------------------------------------------------------------

def children(pid: int) -> list[int]:
    found = []
    for task in Path(f"/proc/{pid}/task").glob("*"):
        try:
            found += [int(c) for c in (task / "children").read_text().split()]
        except OSError:
            pass
    return found


def descendants(pid: int) -> list[int]:
    out, todo = [], [pid]
    while todo:
        for child in children(todo.pop()):
            out.append(child)
            todo.append(child)
    return out


def environ(pid: int) -> dict:
    try:
        raw = Path(f"/proc/{pid}/environ").read_bytes()
    except OSError:
        return {}
    pairs = (item.split(b"=", 1) for item in raw.split(b"\0") if b"=" in item)
    return {k.decode(errors="replace"): v.decode(errors="replace") for k, v in pairs}


def exe(pid: int) -> str:
    try:
        return os.readlink(f"/proc/{pid}/exe")
    except OSError:
        return ""


def cmdline(pid: int) -> list[str]:
    try:
        return Path(f"/proc/{pid}/cmdline").read_bytes().decode(errors="replace").split("\0")
    except OSError:
        return []


def namespaces(pid: int) -> dict:
    out = {}
    for ns in ("user", "mnt", "pid"):
        try:
            out[ns] = os.readlink(f"/proc/{pid}/ns/{ns}")
        except OSError:
            out[ns] = None
    return out


def vulkan_drivers(pid: int) -> list[str] | None:
    """The Vulkan drivers mapped into `pid`, or None when its maps are not
    readable (a non-dumpable process; `sudo -n` is tried once)."""
    try:
        maps = Path(f"/proc/{pid}/maps").read_text()
    except OSError:
        result = subprocess.run(["sudo", "-n", "cat", f"/proc/{pid}/maps"], text=True, capture_output=True)
        if result.returncode != 0:
            return None
        maps = result.stdout
    return sorted({m for m in re.findall(r"libvulkan_[a-z_]+\.so", maps)})


def switches(pid: int) -> dict[str, int]:
    """Context switches of each thread of `pid`, by `tid comm`."""
    out = {}
    for task in Path(f"/proc/{pid}/task").glob("*"):
        try:
            comm = (task / "comm").read_text().strip()
            total = sum(
                int(line.split()[1])
                for line in (task / "status").read_text().splitlines()
                if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"))
            )
            out[f"{task.name} {comm}"] = total
        except OSError:
            pass
    return out


def anon_kib(pid: int) -> int | None:
    """The anonymous memory `pid` holds, in KiB."""
    try:
        status = Path(f"/proc/{pid}/status").read_text()
    except OSError:
        return None
    for line in status.splitlines():
        if line.startswith("RssAnon:"):
            return int(line.split()[1])
    return None


def host_pids(session_pid: int) -> list[int]:
    """The tilolang hosts of the session."""
    return [p for p in descendants(session_pid) if exe(p).endswith("/bin/tilolang-host")]


# --- checks ---------------------------------------------------------------------

def check_registry(session: Session, names=APPS) -> dict:
    # A cold query starts the index's refresh and may answer before it has
    # every application (docs/headless-testing.md): ask again until it does.
    found = {}
    for name in names:
        hits = until(f"{app_id(name)} among the session's applications", lambda: [
            a for a in session.actions(app_id(name))
            if a.get("provider") == "applications" and a.get("id") == app_id(name)
        ], 30, 0.5)
        found[app_id(name)] = {"title": hits[0]["title"], "category": hits[0].get("subtitle")}
    return found


def launch_all(session: Session) -> dict:
    for name in APPS:
        session.invoke("applications", app_id(name))
    def admitted():
        windows = {w.get("app_id"): w for w in session.windows()}
        return windows if all(app_id(n) in windows for n in APPS) else None
    windows = until("every app's window", admitted)
    def presented():
        frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
        shown = {f["app_id"] for f in frames if f["presented"] and f["pixels_ready"]}
        return all(app_id(n) in shown for n in APPS)
    until("a presented frame of every app", presented, 30)
    return {
        app_id(n): {"group": windows[app_id(n)]["group"], "tab": windows[app_id(n)]["tab"]}
        for n in APPS
    }


def check_processes(session: Session, session_pid: int) -> dict:
    tree = descendants(session_pid)
    shell = next(p for p in tree if exe(p).endswith("/bin/shell") and "__render-service" not in cmdline(p))
    render = [p for p in tree if "__render-service" in cmdline(p)]
    shell_ns = namespaces(shell)
    out = {"shell": {"pid": shell, "vulkan": vulkan_drivers(shell)}, "apps": {}}
    out["render_service"] = [{"pid": p, "vulkan": vulkan_drivers(p)} for p in render]
    for name in APPS:
        procs = [p for p in tree if environ(p).get("TILO_PACKAGE_ID") == app_id(name)
                 and exe(p).endswith("/bin/tilolang-app")]
        if len(procs) != 1:
            raise Failed(f"{app_id(name)} runs in {len(procs)} tilolang runtime processes")
        pid = procs[0]
        env = environ(pid)
        ns = namespaces(pid)
        own = {k: ns[k] is not None and ns[k] != shell_ns[k] for k in ns}
        if not all(own.values()):
            raise Failed(f"{app_id(name)} shares a namespace with the shell: {ns} vs {shell_ns}")
        out["apps"][app_id(name)] = {
            "pid": pid,
            "exe": exe(pid),
            "module": env.get("TILO_APP_MODULE"),
            "icon": env.get("TILO_APP_ICON"),
            "own_namespaces": own,
            "vulkan": vulkan_drivers(pid),
        }
    for who in [out["shell"], *out["render_service"], *out["apps"].values()]:
        drivers = who["vulkan"]
        if drivers and any(d != "libvulkan_lvp.so" for d in drivers):
            raise Failed(f"pid {who['pid']} loaded a GPU's Vulkan driver: {drivers}")
    return out


def own_action(session: Session, name: str, title: str, kind: str):
    for action in session.actions(title):
        if (action.get("application") == app_id(name) and action.get("title") == title
                and action.get("operation_kind") == kind and action["id"].startswith(app_id(name))):
            return action
    return None


def history_total(session: Session, name: str) -> int:
    history = until(f"{app_id(name)}'s history action", lambda: own_action(session, name, "History", "View"))
    answer = session.invoke("app-actions", history["id"])
    try:
        return answer["receipt"]["update"]["output"]["value"]["total"]
    except (KeyError, TypeError):
        raise Failed(f"{app_id(name)}'s history answered without a total: {json.dumps(answer)}") from None


def check_actions(session: Session) -> dict:
    out = {}
    news = {"notes": "New note", "chat": "New chat", "todo": "New item"}
    for name in APPS:
        published = until(
            f"{app_id(name)}'s actions",
            lambda: [a for a in session.actions(app_id(name)) if a.get("application") == app_id(name)],
        )
        out[app_id(name)] = {"published": len(published)}
    for name, title in news.items():
        action = until(f"{app_id(name)}'s {title!r}", lambda: own_action(session, name, title, "Document"))
        before = history_total(session, name)
        state = session.invoke("app-actions", action["id"])["receipt"]["update"]["state"]
        after = until(f"{title!r} in {app_id(name)}'s history",
                      lambda: (lambda t: t if t > before else None)(history_total(session, name)), 15)
        out[app_id(name)].update(invoked=title, state=state, history=[before, after])
    return out


def value_of(answer: dict):
    try:
        return answer["receipt"]["update"]["output"]["value"]
    except (KeyError, TypeError):
        raise Failed(f"an answer without a value: {json.dumps(answer)[:2000]}") from None


def app_action(session: Session, app: str, title: str):
    """The first action called `title` that the window of `app` publishes. Its application is
    `app` exactly: so.tilo.notes never matches so.tilo.lang.notes. (An app out of focus lists
    its actions under directory ids, which do not start with the app's id.)"""
    for action in session.actions(title):
        if action.get("application") == app and action.get("title") == title:
            return action
    return None


def check_native(session: Session, out: Path) -> dict:
    """Native Notes launches beside the tilolang apps and presents a frame."""
    session.invoke("applications", NATIVE)
    windows = until(f"{NATIVE}'s window",
                    lambda: {w.get("app_id"): w for w in session.windows() if w.get("app_id") == NATIVE})
    def presented():
        frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
        return any(f["app_id"] == NATIVE and f["presented"] and f["pixels_ready"] for f in frames)
    until(f"a presented frame of {NATIVE}", presented, 30)
    # Connected: its window publishes its actions.
    until(f"{NATIVE}'s actions", lambda: app_action(session, NATIVE, "Show this window"), 60)
    w = windows[NATIVE]
    session.invoke("desktop-windows", f"tab.focus:{w['group']}:{w['tab']}")
    time.sleep(0.5)
    path = out / "native-notes.png"
    session.control_cmd("snapshot", str(path))
    return {"group": w["group"], "tab": w["tab"], "snapshot": str(path)}


def check_activation(session: Session) -> dict:
    """"Show this window" answers for a tilolang app as it does for the native app."""
    out = {}
    for app in (NATIVE, app_id("notes")):
        action = until(f"{app}'s 'Show this window'", lambda: app_action(session, app, "Show this window"))
        out[app] = value_of(session.invoke("app-actions", action["id"]))
    if out[app_id("notes")] != out[NATIVE]:
        raise Failed(f"'Show this window' answers differently from native Notes: {out}")
    return out


def check_read(session: Session) -> dict:
    """An agent or a script reads what a tilolang app holds through the door's `read`, as it
    reads a native app's content: the room's records, then one of them by its id."""
    action = until("tilolang Notes' 'Read the content'",
                   lambda: app_action(session, app_id("notes"), "Read the content"))
    room = value_of(session.invoke("app-actions", action["id"]))
    # Notes' root record, whatever its type is called: the one record of the app's own kinds.
    prefix = f"tl.{app_id('notes')}."
    notebook = next((r for r in room.get("records", []) if r.get("kind", "").startswith(prefix)), None)
    if notebook is None:
        raise Failed(f"the room's read lists no record of Notes: {json.dumps(room)[:2000]}")
    kind = notebook["kind"]
    read = value_of(session.invoke("app-actions", action["id"], {"item": notebook["item"]}))
    # The collection of notes, whatever the root calls it: its field listing Notes' notes.
    notes = next((f for f in read.get("fields", {}).values() if isinstance(f, dict)
                  and any(i.get("type") == "Note" for i in f.get("items", []))), {})
    if read.get("kind") != kind or not notes.get("items"):
        raise Failed(f"Notes' root record's read holds no notes: {json.dumps(read)[:2000]}")
    native = until("native Notes' 'Read the content'", lambda: app_action(session, NATIVE, "Read the content"))
    return {
        "records": len(room["records"]),
        "notebook": {"label": read.get("label"), "notes": notes.get("count")},
        "native": sorted(value_of(session.invoke("app-actions", native["id"]))),
    }


def snapshots(session: Session, windows: dict, out: Path, prefix: str = "") -> dict:
    shots = {}
    for name in APPS:
        w = windows[app_id(name)]
        session.invoke("desktop-windows", f"tab.focus:{w['group']}:{w['tab']}")
        time.sleep(0.5)
        path = out / f"{prefix}{name}.png"
        session.control_cmd("snapshot", str(path))
        shots[app_id(name)] = str(path)
    return shots


def native_pid(session_pid: int) -> int | None:
    """The process of native Notes, when it runs."""
    return next((p for p in descendants(session_pid)
                 if environ(p).get("TILO_PACKAGE_ID") == NATIVE and Path(exe(p)).name == "notes"), None)


def check_idle(session: Session, processes: dict, session_pid: int, seconds: float = 10.0) -> dict:
    """Every app, native Notes among them, left untouched for 10 s. An app that commits no frame
    is settled, and every thread of it sleeps: zero context switches. An app that commits frames
    is animating, which in this session is only the connecting spinner of an app whose room never
    syncs and that holds nothing to show (Today; native Notes): one frame per pose of the spinner
    and no more, with its context switches per frame recorded."""
    time.sleep(3)
    pids = {app: info["pid"] for app, info in processes["apps"].items()}
    native = native_pid(session_pid)
    if native is None:
        raise Failed("native Notes is not running to be held to the same idle")
    pids[NATIVE] = native
    before = {app: switches(pid) for app, pid in pids.items()}
    seqs = {app: frame_seq(session, app) for app in pids}
    start = time.monotonic()
    time.sleep(seconds)
    elapsed = time.monotonic() - start
    poses = elapsed * INDICATOR_POSES_PER_SECOND
    out, problems = {}, []
    for app, pid in pids.items():
        after = switches(pid)
        frames = frame_seq(session, app) - seqs[app]
        woke = {t: after[t] - before[app].get(t, 0) for t in after if after[t] != before[app].get(t, 0)}
        total = sum(woke.values())
        out[app] = {"frames": frames, "switches": total, "threads": woke}
        if frames == 0 and woke:
            problems.append(f"{app} drew nothing and woke: {woke}")
        elif frames > poses + 1:
            problems.append(f"{app} committed {frames} frames in {elapsed:.1f} s, more than its "
                            f"spinner's {poses:.0f} poses")
        if frames:
            out[app]["switches_per_frame"] = round(total / frames, 1)
    if problems:
        raise Failed(f"{'; '.join(problems)} ({json.dumps(out)})")
    return out


def frame_seq(session: Session, app: str) -> int:
    frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
    return max((f.get("frame_seq") or 0 for f in frames if f["app_id"] == app), default=0)


# The poses a second of a looping indicator (ui_core::animation::INDICATOR_STEP).
INDICATOR_POSES_PER_SECOND = 30


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def log_lines(session: Session, pattern: str) -> list[dict]:
    """The shell log's tracing lines matching `pattern`, each as its `key=value` fields."""
    out = []
    for line in ANSI.sub("", session.log()).splitlines():
        if re.search(pattern, line):
            out.append(dict(re.findall(r"(\w+)=(\S+)", line)))
    return out


def presented(session: Session, app: str) -> bool:
    frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
    return any(f["app_id"] == app and f["presented"] and f["pixels_ready"] for f in frames)


def check_hosted_here(session: Session, windows: dict, out: Path) -> dict:
    """The session's own workspace is hosted by this device: its apps wait on no other host
    (the room syncs with nothing, or with this device's own workspace host, which is there
    already), and each shows its own state at once."""
    room = session.active_room()
    told = session.room_sync(room)
    if told["state"] not in (None, "synced"):
        raise Failed(f"the session's own workspace waits on a host: {told}")
    return {"room": room, "state": told["state"],
            "snapshots": snapshots(session, windows, out, "first-")}


def probe_packages(state: Path, out: Path) -> list[Path]:
    """The probe as Today (whose surface declares that it reads Notes' kinds)
    and as Todo (which declares no read): each package is the app's own, with
    the probe as its entrypoint and the routes to try in its payload. Bakes the
    probe's runtime bundle into the session, signed by its runtime publisher."""
    probe = REPO / "target/debug/tilo-collab-probe"
    if not probe.is_file():
        raise Failed(f"no probe at {probe} (cargo build -p collab-probe)")
    bundle = state / "runtimes" / PROBE_RUNTIME
    shutil.rmtree(bundle, ignore_errors=True)
    run([BIN / "tilo-runtime-bundle", "build", "--version", PROBE_RUNTIME, "--out", bundle,
         "--include-bin", probe, "--identity", state / "keys/publisher.json"])
    identity = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "tilolang/publisher.json"
    tapps = []
    for name, routes in PROBE_ROUTES.items():
        staged = out / f"probe-{name}"
        shutil.rmtree(staged, ignore_errors=True)
        shutil.copytree(out / "build" / name / "payload", staged / "payload")
        (staged / "payload/probe-routes").write_text("\n".join(routes) + "\n")
        manifest = (out / "build" / name / "manifest.toml").read_text()
        # The probe runs as an app launched with that app's own custody, as any app's process
        # is: its entrypoint names the probe's runtime, not tilolang's, so the session starts it
        # as an app process rather than handing a tilolang host its instance.
        manifest, n = re.subn(r'(?m)^optional-runtimes = \["([^"]*)"\]',
                              f'optional-runtimes = ["\\1", "probe/{PROBE_RUNTIME}"]', manifest)
        manifest, m = re.subn(r'(?ms)^\[entrypoint\]\nruntime = "[^"]*"\nbin = "[^"]*"',
                              f'[entrypoint]\nruntime = "probe/{PROBE_RUNTIME}"\nbin = "bin/tilo-collab-probe"',
                              manifest)
        if n != 1 or m != 1:
            raise Failed(f"cannot point {name}'s manifest at the probe")
        (staged / "manifest.toml").write_text(manifest)
        tapp = staged / f"{name}.tapp"
        run([BIN / "tilo-app-package", "pack", "--manifest", staged / "manifest.toml",
             "--payload", staged / "payload", "--identity", identity, "--output", tapp])
        tapps.append(tapp)
    return tapps


def check_routes(session: Session) -> dict:
    """Launch the probe as Today and as Todo and read what the broker let
    each do on the routes it tried."""
    # As in the first start, the applications are listed before any opens:
    # an app opened in the session's first moments, before its broker has
    # bound, is refused ("native app authorship broker unavailable").
    check_registry(session, [*PROBE_ROUTES, "notes"])
    for name in PROBE_ROUTES:
        session.invoke("applications", app_id(name))
    pattern = re.compile(r"\[app (\S+) stdout\] probe: ROUTE (\S+) (.*)")
    def answered():
        lines = [m.groups() for m in pattern.finditer(session.log())]
        return lines if len(lines) >= sum(map(len, PROBE_ROUTES.values())) else None
    lines = until("the probes' route answers", answered, 60)
    result = {f"{who} -> {route}": answer for who, route, answer in lines}
    today = result.get(f"{app_id('today')} -> {app_id('notes')}", "")
    todo = result.get(f"{app_id('todo')} -> {app_id('notes')}", "")
    if not (today.startswith("JOINED") and "WRITE-REFUSED" in today and "changes nothing" in today):
        raise Failed(f"Today's declared read of Notes was not a read-only join: {today!r}")
    if not todo.startswith("JOIN-REFUSED"):
        raise Failed(f"Todo, which declares no read of Notes, joined its route: {todo!r}")
    return result


def start_session(state: Path, out: Path, tapps: list[Path], tag: str, native: str = "",
                  fresh: bool = True):
    ready = out / f"ready-{tag}.json"
    argv = [SESSION, "--no-build", "--headless", "--software", "--apps", native,
            "--state", state, "--ready", ready]
    if fresh:
        argv.append("--fresh-desktop")
    for tapp in tapps:
        argv += ["--install-tapp", tapp]
    print(f"==> starting the headless session ({tag}, lavapipe only)", file=sys.stderr)
    log = open(out / f"session-{tag}.log", "w")
    proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                            env={**os.environ, "RUST_LOG": os.environ.get(
        "RUST_LOG", "warn,tilo::launch_latency=info,tilo::room_sync=info")})
    until("the session", lambda: ready.is_file() or proc.poll() is not None, 300, 0.5)
    if proc.poll() is not None:
        raise Failed(f"the session ended ({proc.returncode}); see {out / f'session-{tag}.log'}")
    return proc, Session(state, json.loads(ready.read_text()))


def stop_session(proc) -> None:
    if proc is not None and proc.poll() is None:
        os.kill(proc.pid, signal.SIGTERM)
        proc.wait(60)


def run_checks(summary: dict, checks) -> bool:
    for name, check in checks:
        print(f"==> {name}", file=sys.stderr)
        try:
            summary["checks"][name] = check()
            print(json.dumps({name: summary["checks"][name]}, indent=2))
        except Failed as error:
            summary["failed"].append(name)
            summary["checks"][name] = {"failed": str(error)}
            print(f"FAIL {name}: {error}", file=sys.stderr)
            if name in ("launch", "processes"):
                return False
    return True


def apps_phases(summary: dict, state: Path, out: Path, running: dict) -> bool:
    """The apps in a session of their own, then the route checks in a second start of it."""
    proc, session = start_session(state, out, [out / "build" / f"{n}.tapp" for n in APPS], "apps",
                                  native="notes")
    running["proc"] = proc
    pid = proc.pid
    ok = run_checks(summary, [
        ("registry", lambda: check_registry(session)),
        ("launch", lambda: launch_all(session)),
        ("processes", lambda: check_processes(session, pid)),
        # Before anything is in them: the session's own workspace is this device's to
        # host, so no app waits on a host and each shows its own state.
        ("hosted-here", lambda: check_hosted_here(session, summary["checks"]["launch"], out)),
        ("actions", lambda: check_actions(session)),
        ("snapshots", lambda: snapshots(session, summary["checks"]["launch"], out)),
        ("native", lambda: check_native(session, out)),
        ("activation", lambda: check_activation(session)),
        ("read", lambda: check_read(session)),
        ("idle", lambda: check_idle(session, summary["checks"]["processes"], pid)),
    ])
    stop_session(proc)
    running["proc"] = None
    # The second start writes its own shell log over this one.
    shutil.copy(state / "logs/shell.log", out / "shell-apps.log")
    if not ok:
        return False
    # Route access, with the probe installed as Today and as Todo. The probes first: until
    # a package names the probe's runtime, the collection after each install reclaims it.
    tapps = [*probe_packages(state, out), out / "build/notes.tapp"]
    proc, session = start_session(state, out, tapps, "routes")
    running["proc"] = proc
    run_checks(summary, [("routes", lambda: check_routes(session))])
    stop_session(proc)
    running["proc"] = None
    shutil.copy(state / "logs/shell.log", out / "shell-routes.log")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path, default=REPO / "target/tilolang-e2e")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    state = out / "state"
    tool = REPO / "target/debug/tilolang"
    os.environ.setdefault("TMPDIR", str(Path.home() / "tmp"))
    Path(os.environ["TMPDIR"]).mkdir(parents=True, exist_ok=True)

    build = [] if not args.no_build else ["--no-build"]
    print("==> preparing the session", file=sys.stderr)
    run([SESSION, *build, "--prepare-only", "--apps", "notes", "--state", state])
    if not args.no_build:
        print("==> building the tilolang tool and the probe", file=sys.stderr)
        run(["cargo", "build", "-p", "tilolang-cli", "--bin", "tilolang",
             "-p", "collab-probe", "--bin", "tilo-collab-probe",
             "-p", "tilolang-host", "--example", "components"], cwd=REPO)
    runtime = (state / "runtimes/.version").read_text().strip()
    print("==> tilolang build", file=sys.stderr)
    run([tool, "build", REPO / "apps/tl/today", "--search", REPO / "apps/tl", "--out", out / "build",
         "--tilo-runtime", f"tilo-runtime/{runtime}"])

    summary: dict = {"checks": {}, "failed": []}
    # The session running now, stopped at the end whatever happens.
    running: dict = {"proc": None}
    try:
        apps_phases(summary, state, out, running)
    except Failed as error:
        summary["failed"].append("session")
        summary["error"] = str(error)
        print(f"FAIL {error}", file=sys.stderr)
    finally:
        (out / "summary.json").write_text(json.dumps(summary, indent=2))
        if not args.keep:
            stop_session(running["proc"])
    print(f"==> {'FAILED: ' + ', '.join(summary['failed']) if summary['failed'] else 'every check passed'}"
          f" (summary: {out / 'summary.json'})", file=sys.stderr)
    return 1 if summary["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
