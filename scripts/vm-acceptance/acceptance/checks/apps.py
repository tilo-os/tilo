"""Essential 3: open and close Tilo apps, a host Wayland app and an X11 app."""

from __future__ import annotations

import re
import time
from dataclasses import dataclass

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, wait_until

AREA = "apps"

CLOSE_GRACE_S = 20
# A first frame whose luminance spread stays at or below this is a flat fill.
FLAT_STDDEV = 0.02
# A client can present a clear before its first real buffer: how long to wait for content.
FIRST_FRAME_WAIT_S = 5.0
LAUNCHER = "tilo-launcher"

# pid, the stat fields after the command name, the cgroup and the command line of
# every process the slot's user owns.
_PROC_SCRIPT = ("for d in /proc/[0-9]*; do u=$(stat -c %u $d 2>/dev/null); "
                "[ \"$u\" = \"$(id -u)\" ] || continue; "
                "st=$(sed 's/.*) //' $d/stat 2>/dev/null) || continue; "
                "cg=$(tr '\\n' ' ' < $d/cgroup 2>/dev/null); "
                "printf '%s\\t%s\\t%s\\t%s\\n' ${d#/proc/} \"$st\" \"$cg\" "
                "\"$(tr '\\0' ' ' < $d/cmdline 2>/dev/null | cut -c1-1024)\"; done")


@dataclass(frozen=True)
class Proc:
    cmd: str
    ppid: int
    sid: int
    cgroup: str = ""

    @property
    def exe(self) -> str:
        return self.cmd.split()[0].rsplit("/", 1)[-1] if self.cmd else ""


@dataclass(frozen=True)
class AppLaunch:
    """What marks a process as the app's: the launcher processes the open started,
    the sessions processes of that tree lead (a host launch `setsid`s its app, Wine's
    prefix joins it) and the launcher's `app-tilo-<id>-<pid>.scope`."""
    roots: frozenset
    sessions: frozenset

    def owns(self, pid: int, proc: Proc, procs: dict) -> bool:
        if proc.sid in self.sessions or _in_launch_scope(proc.cgroup, self.roots):
            return True
        seen = set()
        while pid not in seen and pid > 1:
            if pid in self.roots:
                return True
            seen.add(pid)
            parent = procs.get(pid)
            if parent is None:
                return False
            pid = parent.ppid
        return False

    def members(self, procs: dict) -> dict:
        return {p: c for p, c in procs.items() if self.owns(p, c, procs)}


def parse_procs(text: str) -> dict[int, Proc]:
    """Rows of `_PROC_SCRIPT`; kernel threads and vanished processes have no command."""
    procs = {}
    for line in text.splitlines():
        fields = line.split("\t", 3)
        if len(fields) != 4 or not fields[0].isdigit() or not fields[3].strip():
            continue
        stat = fields[1].split()
        if len(stat) < 4 or not stat[1].isdigit() or not stat[3].isdigit():
            continue
        procs[int(fields[0])] = Proc(fields[3].strip(), int(stat[1]), int(stat[3]), fields[2].strip())
    return procs


def _in_launch_scope(cgroup: str, roots) -> bool:
    for path in cgroup.split():
        unit = path.rstrip("/").rsplit("/", 1)[-1]
        if unit.startswith("app-tilo-") and unit.endswith(".scope"):
            pid = unit[:-len(".scope")].rsplit("-", 1)[-1]
            if pid.isdigit() and int(pid) in roots:
                return True
    return False


def app_launch(procs: dict, before, app_id: str) -> AppLaunch:
    """The launch an open started: launchers for `app_id` (`--app-id`, `--desktop-id`)
    that were not running before it, and the sessions led by a process of their tree.
    Session services the app merely woke up (D-Bus-activated portals, socket-activated
    Tilo services, the per-workspace Xwayland, another launcher's service) are children
    of the user manager or the compositor, so they never count."""
    roots = frozenset(p for p, c in procs.items()
                      if p not in before and c.exe == LAUNCHER and app_id in c.cmd.split())
    tree = AppLaunch(roots, frozenset()).members(procs)
    return AppLaunch(roots, frozenset(p for p, c in tree.items() if c.sid == p))


def lingering(procs: dict, launch: AppLaunch, owned: dict) -> dict:
    """App processes still running: anything the launch still owns, and any process seen
    as the app's while it was open (same pid and command) that is still alive."""
    left = launch.members(procs)
    left.update({p: procs[p] for p, c in owned.items() if p in procs and procs[p].cmd == c.cmd})
    return left


def first_textured(sample, timeout: float, interval: float = 0.5,
                   clock=time.monotonic, sleep=time.sleep):
    """Sample a window's pixels until they are not a flat fill or `timeout` passes.
    Returns the first textured sample, else the last one, and how many were taken."""
    deadline = clock() + timeout
    attempts = 0
    while True:
        stats = sample()
        attempts += 1
        if stats["stddev"] > FLAT_STDDEV or clock() >= deadline:
            return stats, attempts
        sleep(interval)


def _procs(ctx) -> dict[int, Proc]:
    return parse_procs(ctx.vm.exec(_PROC_SCRIPT))


def _notifications(st) -> list:
    return st["chrome"].get("notifications") or []


def _is_undo_close(toast: dict, title: str, tab_count: int = 1) -> bool:
    """A transient undo toast for a captured title, with any reopen binding.

    Match the English/German shell messages, including the unbound hint. A
    title substring or a shortcut alone cannot identify the close's toast.
    """
    if (not title or not title.strip() or toast.get("severity") != "Info"
            or toast.get("persistent")):
        return False
    english = f"Closed {tab_count} tabs" if tab_count > 1 else f"Closed {title}"
    german = f"{tab_count} Tabs geschlossen" if tab_count > 1 else f"{title} geschlossen"
    messages = ((english, r"Select to undo(?:, or press .+ later)?"),
                (german, r"Zum Rückgängigmachen auswählen(?: oder später .+ drücken)?"))
    return any(toast.get("title") == heading and re.fullmatch(hint, toast.get("body") or "")
               is not None for heading, hint in messages)


def _lifecycle(ctx, res, app_id: str, provider: str, label: str, expect_foreign: bool,
               match=None):
    """Open, verify first frame / placement / focus, close, verify exit and no toasts."""
    ctx.reset()
    st0 = ctx.state()
    toasts0 = len(_notifications(st0))
    focused_display = next(d["id"] for d in st0["chrome"]["displays"] if d.get("focused"))
    before = _procs(ctx)
    ctx.facts[f"groups_before:{label}"] = {g["id"] for g in core.all_groups(st0)}
    group, t_group = ctx.open_app(app_id, provider)
    gid = group["id"]
    res.metrics["seconds_to_window"] = t_group
    started = time.monotonic()
    frame = None
    if expect_foreign:
        def presented():
            for f in ctx.vm.foreign_frames():
                if (match or (lambda f: f["app_id"] == app_id))(f) and f.get("presented") and f.get("frame_seq", 0) > 0:
                    return f
            return None
        frame = wait_until(presented, 30, 0.5)
        if frame is None:
            ctx.shot(f"{label}-no-frame")
            frames = [(f["app_id"], f.get("frame_seq"), f.get("last_error")) for f in ctx.vm.foreign_frames()]
            raise CheckFailed(f"{label} never presented a first frame within 30 s (foreign frames: {frames})")
        res.metrics["seconds_to_first_frame"] = round(t_group + time.monotonic() - started, 2)
    ctx.settle(1.5)
    st = ctx.state()
    shot = ctx.shot(f"{label}-open")
    g = core.group_by_id(st, gid)
    expect(g is not None, f"{label}'s window group vanished after opening")
    res.metrics["tab_title"] = g["tabs"][0].get("title")
    res.metrics["tile"] = core.tab_app(g["tabs"][0])
    expect(core.active_group_id(st) == gid, f"{label} took focus (focused group is {core.active_group_id(st)})")
    disp = core.display_of_group(st, gid)
    expect(disp == focused_display, f"{label} opened on the focused output {focused_display}, not {disp}")
    if frame is not None:
        latest = {"frame": frame, "fresh": True}

        def sample():
            # The first sample reads the shot above; later ones retake it and follow the
            # surface in case its first buffers changed size.
            if not latest["fresh"]:
                latest["frame"] = presented() or latest["frame"]
                ctx.vm.shot(shot)
            latest["fresh"] = False
            r = latest["frame"]["rect"]
            return core.region_stats(core.load_image(shot), (r["x"], r["y"], r["w"], r["h"]))

        stats, attempts = first_textured(sample, FIRST_FRAME_WAIT_S)
        r = latest["frame"]["rect"]
        rect = (r["x"], r["y"], r["w"], r["h"])
        drect = core.display_rects(st)[disp]
        res.metrics["surface_rect"] = rect
        res.metrics["surface_pixels"] = stats
        res.metrics["first_frame_samples"] = attempts
        expect(core.rect_inside(rect, drect, slack=2), f"{label}'s surface {rect} lies inside its output {drect}")
        expect(stats["stddev"] > FLAT_STDDEV,
               f"{label}'s first frame is a flat fill (stddev {stats['stddev']}) "
               f"for {FIRST_FRAME_WAIT_S:g} s ({attempts} samples)")
    launch, app_procs, now = _app_procs(ctx, before, app_id)
    res.metrics["launchers"] = sorted(launch.roots)
    res.metrics["processes"] = sorted(set(c.exe for c in app_procs.values()))
    others = [c.cmd for p, c in now.items() if p not in before and c.exe == LAUNCHER]
    expect(launch.roots, f"no {LAUNCHER} process for {app_id} started with {label}, so its processes "
           f"cannot be told apart (new launchers: {others[:3]})")
    ctx.facts[f"launch:{label}"] = launch
    ctx.facts[f"open:{label}"] = gid
    return gid, app_procs, toasts0


def _app_procs(ctx, before: dict, app_id: str):
    """The open's launch and its processes that are still there 2 s after the first look."""
    first = _procs(ctx)
    time.sleep(2.0)
    now = _procs(ctx)
    launch = app_launch(now, before, app_id)
    return launch, {p: c for p, c in launch.members(now).items()
                    if p in first and first[p].cmd == c.cmd}, now


def close_group(ctx, gid, before_close=None) -> None:
    """Close one window group through its close action.

    Closing one window of an app can take its others with it (Wine File's
    windows all end with its process), so a group already gone is closed, not
    an unknown action."""
    st = ctx.state()
    if before_close is not None:
        if before_close(st) is False:
            return
    if core.group_by_id(st, gid) is None:
        return
    try:
        ctx.vm.invoke("desktop-windows", f"group.close:{gid}")
    except VmError as exc:
        if "unknown_action" not in str(exc) or core.group_by_id(ctx.state(), gid) is not None:
            raise


def _process_app_ids(proc: Proc) -> set[str]:
    """Executable/launcher identities, including Wine's Windows executable paths."""
    args = proc.cmd.split()
    names = {proc.exe.replace("\\", "/").rsplit("/", 1)[-1].lower()}
    for flag in ("--app-id", "--desktop-id"):
        if flag in args and args.index(flag) + 1 < len(args):
            names.add(args[args.index(flag) + 1].lower())
    return names - {""}


def _close(ctx, res, label, gid, app_procs, toasts0):
    # The app can raise a dialog between our first snapshot and a close's guard.
    # Discover it from each fresh snapshot, without taking unrelated windows.
    before = ctx.facts.get(f"groups_before:{label}", set())
    launch = ctx.facts[f"launch:{label}"]
    queue = [gid]
    closed = {}
    invoked = set()
    examined = set(before)

    def observe(st, closing=None):
        groups = core.all_groups(st)
        candidates = [g for g in groups if g["id"] != gid and g["id"] not in invoked and
                      (g["id"] in queue or g["id"] not in examined)]
        eligible = set()
        if candidates:
            procs = _procs(ctx)
            owned = launch.members(procs)
            own_ids = set().union(*(_process_app_ids(p) for p in owned.values()))
            other_ids = set().union(*(_process_app_ids(p) for pid, p in procs.items() if pid not in owned))
            exclusive_ids = own_ids - other_ids
            for group in candidates:
                examined.add(group["id"])
                apps = [(core.tab_app(tab) or "").lower() for tab in group["tabs"]]
                if apps and all(app in exclusive_ids for app in apps):
                    eligible.add(group["id"])
                    if group["id"] not in queue:
                        queue.append(group["id"])
        # Capture all pending titles before invoking a close: it can end dialogs too.
        for group in groups:
            group_id = group["id"]
            if group_id in invoked:
                continue
            if group_id == gid or group_id in eligible:
                closed[group_id] = [tab.get("title") or "" for tab in group["tabs"]]
            elif group_id in queue:
                # A pending group can acquire an unrelated tab or lose exclusive
                # attribution. Do not close it or accept its later notifications.
                closed.pop(group_id, None)
        res.metrics["closed_titles"] = [title for titles in closed.values() for title in titles]
        res.metrics["extra_windows"] = sum(group_id != gid for group_id in closed)
        return closing is None or closing == gid or closing in eligible

    observe(ctx.state())
    deadline = time.monotonic() + 10
    for other in queue:
        expect(time.monotonic() < deadline, f"new app windows kept appearing while closing {label} for 10 s")
        def guard(st):
            accepted = observe(st, other)
            if accepted and core.group_by_id(st, other) is not None:
                # Foreign process exit and compositor group removal can happen
                # separately. Once issued, retain this close's captured metadata.
                invoked.add(other)
            return accepted

        close_group(ctx, other, guard)
        observe(ctx.state())
    ctx.wait_state(lambda s: core.group_by_id(s, gid) is None, 10, f"{label}'s window gone after close")
    left = wait_until(lambda: not lingering(_procs(ctx), launch, app_procs) or None, CLOSE_GRACE_S, 1.0)
    still = {} if left else lingering(_procs(ctx), launch, app_procs)
    res.metrics["lingering_after_close"] = [c.cmd for c in still.values()]
    st = ctx.state()
    toasts = _notifications(st)[toasts0:]
    def expected_undo(toast):
        return any(titles and _is_undo_close(toast, titles[0], len(titles)) for titles in closed.values())

    res.metrics["undo_toasts"] = [t for t in toasts if expected_undo(t)]
    false = [t for t in toasts if not expected_undo(t)]
    res.metrics["toasts"] = false
    expect(not still,
           f"{label}'s processes still running {CLOSE_GRACE_S} s after close: "
           f"{res.metrics['lingering_after_close'][:4]}")
    expect(not false, f"unexpected notifications while opening/closing {label}: {false}")


def _app_check(app_id, label, foreign=True):
    def body(ctx, res):
        gid, procs, toasts0 = _lifecycle(ctx, res, app_id, "applications", label, foreign)
        if foreign:
            expect(procs, f"no process of {label} observed while its window was open")
        _close(ctx, res, label, gid, procs, toasts0)
    return body


SHELL_READY_S = core.TERMINAL_SHELL_S


@check("A01", AREA, "Terminal opens with a live shell and closes cleanly")
def terminal(ctx, res):
    shells = core.terminal_shells(ctx)
    gid, procs, toasts0 = _lifecycle(ctx, res, "so.tilo.terminal", "applications", "A01-terminal", False)
    ready = core.wait_new_terminal_shell(ctx, shells, SHELL_READY_S)
    res.metrics["seconds_to_shell"] = ready
    probe = f"acc-term-{int(time.time())}"
    ctx.vm.control("type", f"echo ok > ~/{probe}")
    ctx.vm.control("key", "Enter")
    found = wait_until(lambda: core.terminal_probe(ctx, probe), 8, 1)
    ctx.shot("A01-terminal-command")
    res.metrics["shell_ran_command"] = bool(found)
    if ready is None or not found:
        ctx.write("A01-ush.txt", core.ush_diagnostics(ctx))
    expect(ready is not None, f"no shell ran in Terminal's environment within {SHELL_READY_S} s "
                              "of its window opening (A01-ush.txt shows the shell runtime)")
    expect(found, "a command typed into Terminal (control-socket type + Enter) never ran")
    _close(ctx, res, "A01-terminal", gid, procs, toasts0)
    core.assert_terminal_shell_performance(ready)


check("A02", AREA, "Files opens, draws its first frame, and exits on close")(
    _app_check("so.tilo.file-explorer", "A02-files"))
check("A03", AREA, "Settings opens, draws its first frame, and exits on close")(
    _app_check("so.tilo.settings", "A03-settings"))
check("A04", AREA, "Notes opens, draws its first frame, and exits on close")(
    _app_check("so.tilo.notes", "A04-notes"))
check("A05", AREA, "Store opens, draws its first frame, and exits on close")(
    _app_check("so.tilo.store", "A05-store"))


@check("A06", AREA, "A host Wayland app (foot) opens in a window and exits on close")
def host_wayland(ctx, res):
    ids = [a["id"] for a in ctx.vm.actions("foot") if a["provider"] == "host-applications"]
    if "foot" not in ids:
        raise Unobservable(f"no host Wayland app to open: `foot` is not in host-applications ({ids})")
    gid, procs, toasts0 = _lifecycle(ctx, res, "foot", "host-applications", "A06-foot", True,
                                     match=lambda f: "foot" in (f.get("app_id") or ""))
    expect(any("foot" in c.cmd for c in procs.values()),
           f"no foot process observed ({[c.cmd for c in procs.values()][:4]})")
    _close(ctx, res, "A06-foot", gid, procs, toasts0)


@check("A07", AREA, "An X11 app opens through Xwayland and exits on close")
def x11(ctx, res):
    cands = {a["id"]: a for a in ctx.vm.actions("xterm") + ctx.vm.actions("xeyes")
             if a["provider"] == "host-applications"}
    if not cands and not ctx.facts.get("apps_installed"):
        try:
            ctx.vm.run("slot", "apps", "install", "--set", "verification", timeout=900)
            ctx.facts["apps_installed"] = True
            time.sleep(3)
        except Exception as exc:  # noqa: BLE001
            res.metrics["verification_install_error"] = str(exc).splitlines()[:3]
        cands = {a["id"]: a for a in ctx.vm.actions("xterm") + ctx.vm.actions("xeyes")
                 if a["provider"] == "host-applications"}
    app = next((i for i in ("xterm", "org.xfce.xterm", "xeyes") if i in cands), None) or next(iter(cands), None)
    if app is None:
        wine = [a["id"] for a in ctx.vm.actions("Wine File") if a["provider"] == "host-applications"]
        app = wine[0] if wine else None
    if app is None:
        raise Unobservable("no X11 app available: xterm/xeyes not installable and no Wine File entry")
    res.metrics["x11_app"] = app
    key = app.split("-")[-1].lower()
    gid, procs, toasts0 = _lifecycle(ctx, res, app, "host-applications", "A07-x11", True,
                                     match=lambda f: key in (f.get("app_id") or "").lower()
                                     or "x11" in (f.get("app_id") or "").lower()
                                     or f.get("app_id") not in (None, "", "so.tilo.settings"))
    xw = [c.cmd for c in _procs(ctx).values() if "Xwayland" in c.cmd or "xwayland" in c.cmd]
    res.metrics["xwayland"] = xw[:2]
    expect(xw, "no Xwayland server is running while an X11 app is open")
    _close(ctx, res, "A07-x11", gid, procs, toasts0)
