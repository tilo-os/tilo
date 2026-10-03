"""Essential 3: open and close Tilo apps, a host Wayland app and an X11 app."""

from __future__ import annotations

import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import wait_until

AREA = "apps"

# Session helpers that start on demand and legitimately outlive one window.
SHARED_HELPERS = ("Xwayland", "xwayland-satellite", "tilo-portal", "xdg-desktop-portal",
                  "pipewire", "wireplumber", "dbus-broker", "at-spi", "tilo-fs",
                  "tilo-model", "fs", "tilo-app-host")
CLOSE_GRACE_S = 20


def _procs(ctx) -> dict[int, str]:
    out = ctx.vm.exec("for d in /proc/[0-9]*; do u=$(stat -c %u $d 2>/dev/null); "
                      "[ \"$u\" = \"$(id -u)\" ] || continue; "
                      "printf '%s\\t%s\\n' ${d#/proc/} \"$(tr '\\0' ' ' < $d/cmdline 2>/dev/null | cut -c1-160)\"; done")
    procs = {}
    for line in out.splitlines():
        pid, _, cmd = line.partition("\t")
        if pid.isdigit() and cmd.strip():
            procs[int(pid)] = cmd.strip()
    return procs


def _stable_new(ctx, before: dict) -> dict:
    a = {p: c for p, c in _procs(ctx).items() if p not in before}
    time.sleep(2.0)
    b = _procs(ctx)
    return {p: c for p, c in a.items() if p in b and "sh -c for d in /proc" not in c}


def _alive(ctx, pids) -> dict:
    now = _procs(ctx)
    return {p: now[p] for p in pids if p in now}


def _notifications(st) -> list:
    return st["chrome"].get("notifications") or []


def _is_undo_close(toast: dict, title: str) -> bool:
    """The transient "Closed X — select to undo" toast a close is meant to raise."""
    return (toast.get("severity") == "Info" and not toast.get("persistent")
            and (bool(title) and title in (toast.get("title") or "")
                 or "Super+Shift+T" in (toast.get("body") or "")))


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
        r = frame["rect"]
        rect = (r["x"], r["y"], r["w"], r["h"])
        drect = core.display_rects(st)[disp]
        res.metrics["surface_rect"] = rect
        expect(core.rect_inside(rect, drect, slack=2), f"{label}'s surface {rect} lies inside its output {drect}")
        img = core.load_image(shot)
        stats = core.region_stats(img, rect)
        res.metrics["surface_pixels"] = stats
        expect(stats["stddev"] > 0.02, f"{label}'s first frame is a flat fill (stddev {stats['stddev']})")
    app_procs = _stable_new(ctx, before)
    res.metrics["processes"] = sorted(set(c.split()[0].rsplit("/", 1)[-1] for c in app_procs.values()))
    ctx.facts[f"open:{label}"] = gid
    return gid, app_procs, toasts0


def _close(ctx, res, label, gid, app_procs, toasts0):
    title = (core.group_by_id(ctx.state(), gid) or {"tabs": [{}]})["tabs"][0].get("title") or ""
    # Windows the app raised on its own (a first-run dialog) are closed with it.
    before = ctx.facts.get(f"groups_before:{label}", set())
    extra = [g["id"] for g in core.all_groups(ctx.state()) if g["id"] not in before and g["id"] != gid]
    res.metrics["extra_windows"] = len(extra)
    for other in [gid, *extra]:
        ctx.vm.invoke("desktop-windows", f"group.close:{other}")
    ctx.wait_state(lambda s: core.group_by_id(s, gid) is None, 10, f"{label}'s window gone after close")
    owned = {p: c for p, c in app_procs.items() if not any(h in c for h in SHARED_HELPERS)}
    left = wait_until(lambda: not _alive(ctx, owned) or None, CLOSE_GRACE_S, 1.0)
    lingering = _alive(ctx, owned)
    res.metrics["lingering_after_close"] = list(lingering.values())
    st = ctx.state()
    toasts = _notifications(st)[toasts0:]
    res.metrics["undo_toasts"] = [t for t in toasts if _is_undo_close(t, title)]
    false = [t for t in toasts if not _is_undo_close(t, title)]
    res.metrics["toasts"] = false
    expect(not owned or left or not lingering,
           f"{label}'s processes still running {CLOSE_GRACE_S} s after close: {list(lingering.values())[:4]}")
    expect(not false, f"unexpected notifications while opening/closing {label}: {false}")


def _app_check(app_id, label, foreign=True):
    def body(ctx, res):
        gid, procs, toasts0 = _lifecycle(ctx, res, app_id, "applications", label, foreign)
        if foreign:
            expect(procs, f"no process of {label} observed while its window was open")
        _close(ctx, res, label, gid, procs, toasts0)
    return body


@check("A01", AREA, "Terminal opens with a live shell and closes cleanly")
def terminal(ctx, res):
    gid, procs, toasts0 = _lifecycle(ctx, res, "so.tilo.terminal", "applications", "A01-terminal", False)
    probe = f"acc-term-{int(time.time())}"
    ctx.vm.control("type", f"echo ok > ~/{probe}")
    ctx.vm.control("key", "Enter")
    found = wait_until(lambda: [l for l in ctx.vm.exec(
        f'find "$HOME" /tmp -maxdepth 4 -name {probe} 2>/dev/null; true').splitlines() if l.strip()], 8, 1)
    ctx.shot("A01-terminal-command")
    res.metrics["shell_ran_command"] = bool(found)
    has_shell = any(c.split()[0].rsplit("/", 1)[-1] in ("bash", "sh", "zsh", "fish") for c in procs.values())
    res.metrics["shell_process"] = has_shell
    expect(found, "a command typed into Terminal (control-socket type + Enter) never ran"
           + (" although a shell process is running: the keys did not reach it (see W17)"
              if has_shell else ", and no shell process was started"))
    _close(ctx, res, "A01-terminal", gid, procs, toasts0)


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
    expect(any("foot" in c for c in procs.values()), f"no foot process observed ({list(procs.values())[:4]})")
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
    xw = [c for c in _procs(ctx).values() if "Xwayland" in c or "xwayland" in c]
    res.metrics["xwayland"] = xw[:2]
    expect(xw, "no Xwayland server is running while an X11 app is open")
    _close(ctx, res, "A07-x11", gid, procs, toasts0)
