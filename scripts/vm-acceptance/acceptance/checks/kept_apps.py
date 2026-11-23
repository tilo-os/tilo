"""Essential 3, continued: the machine's own apps stay open while the desktop restarts in place.

A desktop update restarts the compositor without logging the person out (the session wrapper's
SIGUSR1, as the device steps and a live update ask for it). Every host app runs behind an app keeper
(crates/desktop/app-keeper), which keeps its Wayland state while no desktop is there. This check
opens a GPU-rendered app (alacritty, OpenGL), a shared-memory one (foot) and an X11 one (xterm),
the last in a workspace of its own so the others stand in one behind it, gives each work that is saved nowhere (a running loop, typed scrollback), restarts the desktop in
place, and then requires the same processes, their windows back in their own tabs and workspaces
(the X11 one on the same X server), the loop's output still drawn and typed keys reaching them;
finally a logout ends every one of them and leaves no keeper behind, and the login after it opens
each app again in the tab it was left in: a booted desktop reads the layout its last login kept.
"""

from __future__ import annotations

import time

from .. import core
from ..core import CheckFailed, check, expect
from ..vm import wait_until
from .apps import AREA, _app_procs, _procs
from .daily_apps import (ALACRITTY, FIRST_FRAME_S, DailyApp, _action_id, _frame, _home_file, _open,
                         _require_installed, _token, place_of_surface,
                         print_launch_log)

FOOT = DailyApp("A18-foot", "foot", "foot", "foot", "foot", ("foot",), x11=False, fractional=True)
# An X11 program of a machine application: it draws through the display its keeper serves,
# which is a process of its own and outlives the desktop as the app does.
XTERM = DailyApp("A18-xterm", "xterm", "xterm", "xterm", "xterm", ("xterm",), x11=True,
                 fractional=False)
KEPT_APPS = (ALACRITTY, FOOT, XTERM)
# A restart takes the old compositor down and the new one up: seconds, not minutes.
RESTART_S = 90
# After the new desktop answers, how long the kept windows may take to show again.
BACK_S = 30
# After a login, how long the saved tabs may take to open their apps again: they are asked for
# one at a time, each waiting for the window before.
REOPEN_S = 90

# The wrapper (os/usr/libexec/tilo-session) and the compositor it runs, by command line.
_SESSION_PIDS = ("for d in /proc/[0-9]*; do "
                 "[ \"$(stat -c %u $d 2>/dev/null)\" = \"$(id -u)\" ] || continue; "
                 "c=$(tr '\\0' ' ' < $d/cmdline 2>/dev/null); "
                 "case \"$c\" in *libexec/tilo-session*) echo wrapper ${d#/proc/};; "
                 "/usr/bin/tilo-session*) echo compositor ${d#/proc/};; esac; done")


def _start_times(ctx, pids) -> dict[int, str]:
    """Each live pid's start time (field 22 of its stat), the pair that names one process."""
    if not pids:
        return {}
    out = ctx.vm.exec("for p in " + " ".join(str(p) for p in pids) + "; do "
                      "s=$(sed 's/.*) //' /proc/$p/stat 2>/dev/null | cut -d' ' -f20) && "
                      "[ -n \"$s\" ] && echo $p $s; done; true")
    return {int(p): s for p, s in (line.split() for line in out.splitlines() if len(line.split()) == 2)}


def _session_pids(ctx) -> dict[str, list[int]]:
    found: dict[str, list[int]] = {}
    for line in ctx.vm.exec(_SESSION_PIDS + "; true").splitlines():
        role, _, pid = line.partition(" ")
        if pid.isdigit():
            found.setdefault(role, []).append(int(pid))
    return found


def _kept_dirs(ctx) -> list[str]:
    return [l for l in ctx.vm.exec('ls -1 "$XDG_RUNTIME_DIR/tilo-session/kept" 2>/dev/null; true').split() if l]


def _keepers(ctx) -> dict[int, str]:
    return {p: c.cmd for p, c in _procs(ctx).items() if c.exe == "tilo-app-keeper"}


def _restart_in_place(ctx, res) -> None:
    """Ask the wrapper for an in-place restart and wait for a new compositor that answers."""
    pids = _session_pids(ctx)
    expect(len(pids.get("wrapper", [])) == 1 and len(pids.get("compositor", [])) == 1,
           f"one session wrapper and one compositor run for the account ({pids})")
    old = pids["compositor"][0]
    ctx.vm.exec(f"kill -USR1 {pids['wrapper'][0]}")

    def new_compositor():
        now = _session_pids(ctx).get("compositor", [])
        return now if now and old not in now else None

    now = wait_until(new_compositor, RESTART_S, 1.0)
    expect(now, f"no new compositor replaced pid {old} within {RESTART_S} s of the restart request")
    ctx.vm.wait_answering(RESTART_S)
    res.metrics.setdefault("compositors", []).append([old, now[0]])


def _window_of(ctx, app_id: str):
    return next((f for f in ctx.vm.windows() if f.get("app_id") == app_id and f.get("rect")), None)


def _x11(ctx, pid: int) -> dict:
    """The X display `pid` was handed, and the X servers serving it."""
    try:
        env = ctx.vm.exec(f"tr '\\0' '\\n' < /proc/{pid}/environ 2>/dev/null; true")
    except Exception:  # noqa: BLE001 - an unreadable environment says nothing about a display
        env = ""
    display = next((line.split("=", 1)[1] for line in env.splitlines()
                    if line.startswith("DISPLAY=")), None)
    servers = sorted(p for p, c in _procs(ctx).items() if "xwayland-satellite" in c.cmd)
    return {"pid": pid, "display": display, "servers": servers}


def _type_runs(ctx, app_id: str, name: str) -> bool:
    """Focus the app's window, type a command on the real keyboard, and see it run."""
    frame = _window_of(ctx, app_id)
    if frame is None:
        return False
    x, y, w, h = frame["rect"]
    ctx.vm.input("click", "left", "--at", str(x + w // 2), str(y + h // 2))
    time.sleep(0.5)
    ctx.vm.input("type", f"echo ok > ~/{name}")
    ctx.vm.key("Return")
    return bool(wait_until(lambda: _home_file(ctx, name), 8, 1.0))


def _open_beside(ctx, app: DailyApp):
    """Open `app` without closing what is open; its group, window and processes."""
    surfaces0 = {f["window"] for f in ctx.vm.windows()}
    before = _procs(ctx)
    group, _ = ctx.open_app(_action_id(app))

    def presented():
        return next((f for f in ctx.vm.windows() if f["window"] not in surfaces0
                     and f.get("frames", 0) > 0 and f.get("rect")), None)

    frame = wait_until(presented, FIRST_FRAME_S, 0.5)
    if frame is None:
        ctx.shot(f"{app.label}-no-frame")
        print_launch_log(ctx, app)
        raise CheckFailed(f"{app.name} never drew a first frame within {FIRST_FRAME_S} s")
    _, procs, _ = _app_procs(ctx, before, app.desktop_id)
    expect(any(app.program in c.cmd for c in procs.values()),
           f"no {app.program} process observed ({[c.cmd for c in procs.values()][:4]})")
    return group["id"], frame["window"], procs


@check("A18", AREA, "Machine apps (alacritty on the GPU, foot on shared memory, xterm on its keeper's "
                    "X display) stay open through an in-place desktop restart: same processes, "
                    "windows back in their own tabs, input reaches them, and a logout ends them")
def kept_through_restart(ctx, res):
    for app in KEPT_APPS:
        _require_installed(ctx, res, app)
    token = _token()
    ticks = f"acc-{token}-ticks"
    opened = {}
    for n, app in enumerate(KEPT_APPS):
        # The first opens on a desktop of its own; the next ones beside it, and the last in a
        # workspace of its own, so the others stand in a workspace behind the one in front: a
        # login reads that one's layout as it reads every other.
        if n == len(KEPT_APPS) - 1 and n > 0:
            ctx.vm.invoke(core.SESSION, *core.CHROME_NEW_WORKSPACE, {"name": f"acc-{token}"})
        gid, sid, procs = _open(ctx, res, app) if n == 0 else _open_beside(ctx, app)
        frame = _frame(ctx, sid)
        place = place_of_surface(ctx.state(), sid)
        expect(place, f"{app.name}'s window stands in a tab of a workspace")
        app_proc = next((p for p, c in procs.items() if app.program in c.cmd), None)
        opened[app.label] = {"app_id": frame.get("app_id"), "pids": sorted(procs), "place": place,
                             "x11": _x11(ctx, app_proc) if app.x11 and app_proc else None}
        # Work that is saved nowhere: a loop that keeps writing (and drawing) through the restart.
        ctx.vm.input("type", f"while :; do date +%s >> ~/{ticks}-{app.program}; echo tick; sleep 0.3; done")
        ctx.vm.key("Return")
    res.metrics["opened"] = opened
    keepers = _keepers(ctx)
    res.metrics["keepers_before"] = len(keepers)
    expect(len(keepers) >= len(KEPT_APPS),
           f"every host app runs behind a keeper ({len(keepers)} keepers for {len(KEPT_APPS)} apps)")
    pids = sorted({p for o in opened.values() for p in o["pids"]} | set(keepers))
    before = _start_times(ctx, pids)
    ctx.shot("A18-before-restart")

    for round_ in range(3):
        # Opening through the control socket is no user activity, and the session locks itself
        # on idle a few minutes into a check that waits this long: press a key between rounds.
        core.keep_awake(ctx)
        frames = {o["app_id"]: (_window_of(ctx, o["app_id"]) or {}).get("frames", 0) for o in opened.values()}
        _restart_in_place(ctx, res)
        back = wait_until(lambda: all(_window_of(ctx, o["app_id"]) for o in opened.values()) or None,
                          BACK_S, 1.0)
        ctx.shot(f"A18-after-restart-{round_ + 1}")
        windows = [(f.get("app_id"), f.get("rect"), f.get("frames")) for f in ctx.vm.windows()]
        res.metrics[f"windows_after_{round_ + 1}"] = windows
        expect(back, f"restart {round_ + 1}: every kept app's window is back within {BACK_S} s ({windows})")
        after = _start_times(ctx, pids)
        gone = sorted(set(before) - {p for p, s in after.items() if before.get(p) == s})
        expect(not gone, f"restart {round_ + 1}: the apps and keepers are the same processes "
                         f"(gone or replaced: {gone})")

        # Each window is back in its own tab of the workspace it stood in, and an X11 program on
        # the display its keeper serves still reaches the same server.
        for label, o in opened.items():
            now = _window_of(ctx, o["app_id"])
            place = place_of_surface(ctx.state(), now["window"]) if now else None
            stood = o["place"]
            expect(place and (place["workspace"], place["tab"]) == (stood["workspace"], stood["tab"]),
                   f"restart {round_ + 1}: {label} is back in its own tab of its workspace "
                   f"(was {stood}, is {place})")
            if o["x11"] is not None:
                was = o["x11"]
                x11 = _x11(ctx, was["pid"])
                expect(x11["display"] == was["display"] and set(was["servers"]) <= set(x11["servers"]),
                       f"restart {round_ + 1}: {label} keeps its X display {was['display']} on the "
                       f"same server (was {was}, is {x11})")

        # The loops kept drawing: each window presents new frames on the new desktop.
        def drawing():
            now = {o["app_id"]: (_window_of(ctx, o["app_id"]) or {}).get("frames", 0) for o in opened.values()}
            return now if all(n > 2 for n in now.values()) else None

        drawn = wait_until(drawing, 10, 1.0)
        res.metrics[f"frames_after_{round_ + 1}"] = {"before": frames, "after": drawn}
        expect(drawn, f"restart {round_ + 1}: every kept window draws again on the new desktop")

    # Keys reach each app: stop its loop and run a command typed on the real keyboard.
    core.keep_awake(ctx)
    for app, o in zip(KEPT_APPS, opened.values()):
        core.keep_awake(ctx)
        frame = _window_of(ctx, o["app_id"])
        x, y, w, h = frame["rect"]
        ctx.vm.input("click", "left", "--at", str(x + w // 2), str(y + h // 2))
        ctx.vm.key("ctrl+c")
        ran = _type_runs(ctx, o["app_id"], f"acc-{token}-typed-{app.program}")
        res.metrics[f"typed_{app.program}"] = ran
        expect(ran, f"a command typed into {app.name} after the restarts ran")
    gaps = {}
    for app in KEPT_APPS:
        stamps = [int(s) for s in ctx.vm.exec(f"cat ~/{ticks}-{app.program} 2>/dev/null; true").split() if s.isdigit()]
        gaps[app.program] = max((b - a for a, b in zip(stamps, stamps[1:])), default=None)
    res.metrics["longest_loop_gap_s"] = gaps
    expect(all(g is not None and g <= 3 for g in gaps.values()),
           f"each app's loop ran on through the restarts without stopping (longest gaps: {gaps})")

    # A logout ends every kept app and its keeper, and leaves no keeper directory.
    ctx.vm.restart_session()
    left = wait_until(lambda: not (set(_start_times(ctx, pids)) & set(pids)) or None, 30, 1.0)
    still = sorted(set(_start_times(ctx, pids)) & set(pids)) if not left else []
    res.metrics["left_after_logout"] = still
    dirs = _kept_dirs(ctx)
    res.metrics["kept_dirs_after_logout"] = dirs
    ctx.vm.exec(f'rm -f "$HOME"/acc-{token}-*; true')
    expect(not still, f"a logout ended every kept app and keeper (still running: {still})")
    expect(not dirs, f"a logout left no keeper directory ({dirs})")

    # The login after the logout opens each app again, each in the tab it was left in: those of
    # the workspace behind the one in front too.
    def reopened():
        state = ctx.state()
        places = {}
        for label, o in opened.items():
            window = _window_of(ctx, o["app_id"])
            place = place_of_surface(state, window["window"]) if window else None
            if place is None:
                return None
            places[label] = place
        return places

    places = wait_until(reopened, REOPEN_S, 1.0)
    ctx.shot("A18-after-login")
    res.metrics["places_after_login"] = places
    expect(places, f"after a logout and a login every app is open again within {REOPEN_S} s "
                   f"({[(f.get('app_id'), f.get('rect')) for f in ctx.vm.windows()]})")
    for label, o in opened.items():
        stood = o["place"]
        place = places[label]
        expect((place["workspace"], place["tab"]) == (stood["workspace"], stood["tab"]),
               f"after the login {label} is in the tab of the workspace it was left in "
               f"(was {stood}, is {place})")
    ctx.close_all_windows()
