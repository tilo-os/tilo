"""Essential 3, continued: the apps people run every day, one per toolkit and protocol path.

Chromium (the engine Electron apps ship) on Wayland and forced onto X11, GTK4/libadwaita,
Qt6 on Wayland, alacritty (winit/OpenGL) and an OpenGL X11 client the way games are. The dev
image carries them (os/Containerfile.dev); a disk without them fails these checks as
unobservable. Each one has to open in the current strip and take focus, draw a first frame
that is not a flat fill, render at the scale of each of the two outputs as it is moved
between them, take typed text from the real keyboard, copy to and paste from the
clipboard Tilo's own apps use, and exit when its window is closed.
"""

from __future__ import annotations

import base64
import math
import random
import string
import time
from dataclasses import dataclass

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import wait_until
from .apps import AREA, _app_procs, _close, _notifications, _procs, close_group

FIRST_FRAME_S = 45
NON_FLAT_S = 25
SCALE_SETTLE_S = 10
# Buffers are whole pixels, logical sizes are not: this much rounding is no wrong scale.
SCALE_SLACK_PX = 3


@dataclass(frozen=True)
class DailyApp:
    label: str
    name: str
    desktop_id: str
    # The dev image package and the program it puts on PATH.
    package: str
    program: str
    # Names to find the entry by in the host-applications catalogue.
    queries: tuple
    # Through Xwayland rather than as a Wayland client.
    x11: bool
    # A Wayland client that draws at the output's fractional scale (wp_fractional_scale_v1).
    fractional: bool


CHROMIUM_WAYLAND = DailyApp("A08-chromium-wayland", "Chromium on Wayland", "tilo-acceptance-chromium-wayland",
                            "chromium", "chromium-browser", ("Chromium",), x11=False, fractional=True)
CHROMIUM_X11 = DailyApp("A09-chromium-x11", "Chromium on X11", "tilo-acceptance-chromium-x11",
                        "chromium", "chromium-browser", ("Chromium",), x11=True, fractional=False)
TEXT_EDITOR = DailyApp("A10-gnome-text-editor", "GNOME Text Editor", "org.gnome.TextEditor",
                       "gnome-text-editor", "gnome-text-editor", ("Text Editor",), x11=False, fractional=True)
FEATHERPAD = DailyApp("A11-featherpad", "FeatherPad", "featherpad",
                      "featherpad", "featherpad", ("FeatherPad",), x11=False, fractional=True)
ALACRITTY = DailyApp("A12-alacritty", "alacritty", "Alacritty",
                     "alacritty", "alacritty", ("Alacritty",), x11=False, fractional=True)
GLXGEARS = DailyApp("A13-glxgears", "glxgears", "tilo-acceptance-glxgears",
                    "glx-utils", "glxgears", ("glxgears",), x11=True, fractional=False)


def _token() -> str:
    return str(int(time.time()) % 100000) + "".join(random.choices(string.ascii_lowercase, k=4))


def _lines(text: str) -> list[str]:
    return [l.strip() for l in text.splitlines() if l.strip() and not l.startswith("receipt:")]


def _require_installed(ctx, res, app: DailyApp) -> None:
    """The app's entry is in the host-applications catalogue, or the check cannot run."""
    def listed():
        ids = {a["id"] for q in app.queries for a in ctx.vm.actions(q) if a["provider"] == "host-applications"}
        return app.desktop_id in ids or None

    if wait_until(listed, 30, 2.0):
        return
    found = _lines(ctx.vm.exec(f"command -v {app.program}; "
                               f"ls /usr/share/applications/{app.desktop_id}.desktop; true"))
    res.metrics["installed"] = found
    if not any(l.endswith(f"/{app.program}") for l in found):
        raise Unobservable(f"{app.name} is not in this image: the dev image installs `{app.package}` "
                           "(os/Containerfile.dev); build the disk from a commit that has it")
    if not any(l.endswith(".desktop") for l in found):
        raise Unobservable(f"{app.desktop_id}.desktop is missing although {app.program} is installed: "
                           "the dev image ships it from os/dev-image/")
    raise CheckFailed(f"{app.desktop_id}.desktop is installed but host-applications never listed it "
                      f"(queried {list(app.queries)})")


def _frame(ctx, surface_id):
    return next((f for f in ctx.vm.foreign_frames() if f["surface_id"] == surface_id), None)


def _rect(frame) -> tuple:
    r = frame["rect"]
    return (r["x"], r["y"], r["w"], r["h"])


def output_scales(ctx, st) -> dict:
    """Each display's scale: the mode the run asked for (--outputs, left to right) over the
    display's logical width."""
    rects = core.display_rects(st)
    ordered = sorted(rects, key=lambda d: (rects[d][0], rects[d][1]))
    scales = {}
    for d, spec in zip(ordered, ctx.config["outputs"]):
        mode_w = int(spec.split("@")[0].split("x")[0])
        scales[d] = round(mode_w / rects[d][2], 3) if rects[d][2] else None
    return scales


def _scale_verdict(app: DailyApp, frame, scale: float):
    """None while the frame has no size yet; else (ok, what the buffer says)."""
    r, b = frame.get("rect"), frame.get("buffer")
    if not r or not b or not r["w"] or not r["h"]:
        return None
    seen = {"output_scale": scale, "rect": [round(r["w"], 1), round(r["h"], 1)], "buffer": [b["w"], b["h"]],
            "buffer_scale": round(b["w"] / r["w"], 3)}

    def at(k):
        return abs(b["w"] - r["w"] * k) <= SCALE_SLACK_PX and abs(b["h"] - r["h"] * k) <= SCALE_SLACK_PX

    if app.fractional:
        # Drawn at exactly the output's scale: a whole-number buffer scale on a fractional
        # output means the client lost wp_fractional_scale and is downsampled.
        return at(scale), seen
    # Xwayland: drawn at 1 and scaled by the compositor, or at a whole scale, but sized to the tab.
    return any(at(k) for k in {1, scale, *range(1, math.ceil(scale) + 1)}), seen


def _check_scale(ctx, res, app: DailyApp, surface_id, display, scales, where: str) -> None:
    scale = scales.get(display)
    expect(scale, f"display {display} has a known scale (outputs {ctx.config['outputs']})")
    last = {}

    def settled():
        f = _frame(ctx, surface_id)
        if f is None:
            return None
        last["frame"] = f
        st = ctx.state()
        drect = core.display_rects(st).get(display)
        verdict = _scale_verdict(app, f, scale)
        if verdict is None or drect is None:
            return None
        last["verdict"] = verdict
        return (verdict[0] and core.rect_inside(_rect(f), drect, slack=2)) or None

    ok = wait_until(settled, SCALE_SETTLE_S, 0.5)
    seen = (last.get("verdict") or (False, {}))[1]
    res.metrics.setdefault("scale", {})[where] = seen
    if not ok:
        ctx.shot(f"{app.label}-scale-{where}")
    want = (f"its output's scale {scale}" if app.fractional
            else f"a whole scale up to {math.ceil(scale)} (or {scale})")
    expect(ok, f"{app.name} on output {display} ({where}): buffer {seen.get('buffer')} for a "
               f"{seen.get('rect')} tab is buffer scale {seen.get('buffer_scale')}, want {want}, "
               "inside that output")


def _open(ctx, res, app: DailyApp):
    """Open from the launcher; wait for its first presented frame and a first frame with content.
    Returns (group id, surface id, its processes, notifications before)."""
    ctx.reset()
    st0 = ctx.state()
    toasts0 = len(_notifications(st0))
    focused_display = next(d["id"] for d in st0["chrome"]["displays"] if d.get("focused"))
    surfaces0 = {f["surface_id"] for f in ctx.vm.foreign_frames()}
    before = _procs(ctx)
    ctx.facts[f"groups_before:{app.label}"] = {g["id"] for g in core.all_groups(st0)}
    try:
        group, t_group = ctx.open_app(app.desktop_id, "host-applications")
    except CheckFailed:
        # Whether it started at all, and what it and the compositor said.
        ctx.shot(f"{app.label}-no-window")
        res.metrics["running"] = [c.cmd[:200] for c in _procs(ctx).values() if app.program in c.cmd][:4]
        ctx.write(f"{app.label}-no-window.log", ctx.vm.exec(
            "sed 's/\\x1b\\[[0-9;]*m//g' \"$HOME/.local/state/tilo/session-logs/latest.log\" 2>/dev/null "
            f"| grep -aiE '{app.desktop_id}|{app.program}|xwayland|x11' | tail -n 80 | cut -c1-400; true",
            check=False))
        raise
    gid = group["id"]
    res.metrics["seconds_to_window"] = t_group
    started = time.monotonic()

    def presented():
        return next((f for f in ctx.vm.foreign_frames() if f["surface_id"] not in surfaces0
                     and f.get("presented") and f.get("frame_seq", 0) > 0 and f.get("rect")), None)

    frame = wait_until(presented, FIRST_FRAME_S, 0.5)
    if frame is None:
        ctx.shot(f"{app.label}-no-frame")
        frames = [(f["app_id"], f.get("frame_seq"), f.get("last_error")) for f in ctx.vm.foreign_frames()]
        raise CheckFailed(f"{app.name} never presented a first frame within {FIRST_FRAME_S} s "
                          f"(foreign frames: {frames})")
    sid = frame["surface_id"]
    res.metrics["app_id"] = frame.get("app_id")
    res.metrics["seconds_to_first_frame"] = round(t_group + time.monotonic() - started, 2)

    # A first frame can be the toolkit's background before any widget is drawn: wait, bounded,
    # for one with content.
    path = ctx.out / "shots" / f"{app.label}-open.png"
    stats, deadline = None, time.monotonic() + NON_FLAT_S
    while True:
        f = _frame(ctx, sid) or frame
        ctx.vm.shot(path)
        stats = core.region_stats(core.load_image(path), _rect(f))
        if stats["stddev"] > 0.02 or time.monotonic() >= deadline:
            break
        time.sleep(1.0)
    ctx.evidence(path)
    res.metrics["surface_pixels"] = stats
    res.metrics["seconds_to_content"] = round(t_group + time.monotonic() - started, 2)
    expect(stats["stddev"] > 0.02,
           f"{app.name}'s window is still a flat fill {NON_FLAT_S} s after its first frame "
           f"(stddev {stats['stddev']})")

    st = ctx.state()
    if core.group_by_id(st, gid) is None:
        # The surface that drew is the window: say where it went rather than only
        # that its first group is gone (a group replaced by the compositor vs. a
        # window that really closed).
        moved = group_of_surface(st, sid)
        res.metrics["replaced_group"] = {"opened_as": gid, "now": moved and moved["id"]}
        expect(moved is None, f"{app.name}'s window group {gid} was replaced by group {moved and moved['id']} "
                              "holding the same window: the compositor re-created the group after opening")
        ctx.shot(f"{app.label}-vanished")
        expect(False, f"{app.name}'s window group vanished after opening (and no group holds its surface {sid})")
    expect(any(g["id"] == gid for g in core.groups(st)), f"{app.name} opened in the current strip")
    expect(core.active_group_id(st) == gid,
           f"{app.name} took focus (focused group is {core.active_group_id(st)})")
    disp = core.display_of_group(st, gid)
    expect(disp == focused_display, f"{app.name} opened on the focused output {focused_display}, not {disp}")
    launch, procs, _ = _app_procs(ctx, before, app.desktop_id)
    ctx.facts[f"launch:{app.label}"] = launch
    res.metrics["processes"] = sorted(set(c.cmd.split()[0].rsplit("/", 1)[-1] for c in procs.values()))
    expect(any(app.program in c.cmd for c in procs.values()),
           f"no {app.program} process observed ({[c.cmd for c in procs.values()][:4]})")
    if app.x11:
        xw = [c.cmd for c in _procs(ctx).values() if "Xwayland" in c.cmd or "xwayland" in c.cmd]
        res.metrics["xwayland"] = xw[:2]
        expect(xw, f"no Xwayland server is running while {app.name} is open")
    return gid, sid, procs, toasts0


def group_of_surface(st, sid):
    """The window group whose tab shows foreign surface `sid`."""
    for g in core.all_groups(st):
        for tab in g["tabs"]:
            tile = tab.get("tile")
            if isinstance(tile, dict) and (tile.get("Foreign") or {}).get("surface_id") == sid:
                return g
    return None


def _alive(ctx, owned: dict) -> dict:
    """The processes in `owned` still running with the same command."""
    now = _procs(ctx)
    return {p: now[p] for p, c in owned.items() if p in now and now[p].cmd == c.cmd}


def _across_outputs(ctx, res, app: DailyApp, gid, sid) -> None:
    """Right scale where it opened, on the other output after the move, and back."""
    st = ctx.state()
    rects = core.display_rects(st)
    expect(len(rects) >= 2, f"two outputs present (have {len(rects)})")
    scales = output_scales(ctx, st)
    res.metrics["output_scales"] = scales
    home = core.display_of_group(st, gid)
    _check_scale(ctx, res, app, sid, home, scales, "opened")
    other = next(d for d in sorted(rects, key=lambda d: rects[d][0]) if d != home)
    there, back = (("super+ctrl+shift+Right", "super+ctrl+shift+Left") if rects[other][0] > rects[home][0]
                   else ("super+ctrl+shift+Left", "super+ctrl+shift+Right"))
    ctx.vm.key(there)
    ctx.wait_state(lambda s: core.display_of_group(s, gid) == other, 5,
                   f"{app.name} on the other output {other} after {there}")
    _check_scale(ctx, res, app, sid, other, scales, "moved")
    ctx.shot(f"{app.label}-other-output")
    ctx.vm.key(back)
    ctx.wait_state(lambda s: core.display_of_group(s, gid) == home, 5,
                   f"{app.name} back on output {home} after {back}")
    _check_scale(ctx, res, app, sid, home, scales, "back")


def _clipboard(ctx):
    """What Tilo's own apps would paste: the in-process clipboard route chrome and apps share."""
    return (ctx.vm.raw({"op": "clipboard_paste"}).get("clipboard") or {}).get("text")


def _wait_clipboard(ctx, want: str):
    got = {}

    def probe():
        got["text"] = _clipboard(ctx)
        return (got["text"] or "").strip() == want or None

    return bool(wait_until(probe, 8, 0.5)), got.get("text")


def _text_field(ctx, res, app: DailyApp, sid, undo: bool) -> None:
    """Keys typed on the real keyboard land in the focused text field; Ctrl+C reaches Tilo's
    clipboard; Ctrl+V pastes what a Tilo app copied."""
    token = _token()
    typed = f"tilo{token}"
    frame = _frame(ctx, sid)
    expect(frame and frame.get("rect"), f"{app.name}'s window is still there to type into")
    rect = _rect(frame)
    before = core.load_image(ctx.shot(f"{app.label}-before-typing"))
    ctx.vm.input("type", typed)
    time.sleep(1.0)
    after = core.load_image(ctx.shot(f"{app.label}-typed"))
    changed = core.region_diff(before, after, rect)
    res.metrics["typing_changed_luminance"] = changed
    ctx.vm.key("ctrl+a", "ctrl+c")
    copied, got = _wait_clipboard(ctx, typed)
    res.metrics["copied_to_tilo"] = copied
    expect(copied, f"text typed into {app.name} on the real keyboard and copied with Ctrl+A, Ctrl+C "
                   f"never reached Tilo's clipboard (it holds {got!r}; the window's pixels changed by "
                   f"{changed} while typing: {'the keys arrived, the copy did not' if changed > 0.0005 else 'the keys may not have arrived either (see W17)'})")

    # A pasted text followed by one typed letter: a paste that does nothing leaves just the letter.
    pasted = f"tilo{token}p"
    ctx.vm.raw({"op": "clipboard_copy", "text": pasted})
    time.sleep(0.5)
    ctx.vm.key("ctrl+a", "ctrl+v")
    time.sleep(0.8)
    ctx.vm.input("type", "q")
    time.sleep(0.5)
    ctx.vm.key("ctrl+a", "ctrl+c")
    ok, got = _wait_clipboard(ctx, pasted + "q")
    res.metrics["pasted_from_tilo"] = ok
    ctx.shot(f"{app.label}-pasted")
    if undo:
        # Back to the unmodified document, so closing asks nothing.
        ctx.vm.key(*["ctrl+z"] * 12)
        time.sleep(0.8)
    expect(ok, f"text a Tilo app copied, pasted into {app.name} with Ctrl+V, did not arrive "
               f"(the field then held {got!r}, want {pasted + 'q'!r})")


def _home_file(ctx, name):
    return core.terminal_probe(ctx, name)


def _terminal(ctx, res, app: DailyApp) -> None:
    """A shell in the terminal runs what the real keyboard types; OSC 52 copies to Tilo's
    clipboard; Ctrl+Shift+V pastes what a Tilo app copied."""
    token = _token()
    ran = f"acc-{token}-typed"
    ctx.vm.input("type", f"echo ok > ~/{ran}")
    ctx.vm.key("Return")
    typed = wait_until(lambda: _home_file(ctx, ran), 8, 1.0)
    ctx.shot(f"{app.label}-typed")
    res.metrics["typed_command_ran"] = bool(typed)
    expect(typed, f"a command typed into {app.name} on the real keyboard never ran (see W17)")

    copied_text = f"tilo{token}"
    payload = base64.b64encode(copied_text.encode()).decode()
    ctx.vm.input("type", f"printf '\\033]52;c;{payload}\\007'")
    ctx.vm.key("Return")
    copied, got = _wait_clipboard(ctx, copied_text)
    res.metrics["copied_to_tilo"] = copied

    pasted = f"acc-{token}-pasted"
    ctx.vm.raw({"op": "clipboard_copy", "text": f"echo ok > ~/{pasted}"})
    time.sleep(0.5)
    ctx.vm.key("ctrl+shift+v")
    time.sleep(0.5)
    ctx.vm.key("Return")
    ran_paste = wait_until(lambda: _home_file(ctx, pasted), 8, 1.0)
    res.metrics["pasted_from_tilo"] = bool(ran_paste)
    ctx.shot(f"{app.label}-pasted")
    ctx.vm.exec(f'rm -f "$HOME/{ran}" "$HOME/{pasted}"; true')
    expect(copied, f"{app.name}'s clipboard write (OSC 52) never reached Tilo's clipboard (it holds {got!r})")
    expect(ran_paste, f"a command a Tilo app copied, pasted into {app.name} with Ctrl+Shift+V, never ran")


def _ozone_flag(ctx, res, flag: str) -> None:
    """The running browser is the one this launch started, on the platform it asked for."""
    # A pgrep -f literal also matches the shell executing that very probe,
    # making a missing platform flag look healthy. Read actual browser argv instead.
    hits = [str(pid) for pid, proc in _procs(ctx).items()
            if proc.exe in ("chromium-browser", "chromium", "chrome")
            and f"--ozone-platform={flag}" in proc.cmd.split()][:3]
    res.metrics[f"ozone_{flag}_pids"] = hits
    expect(hits, f"no Chromium process runs with --ozone-platform={flag}")


def _no_running(ctx, res, app: DailyApp) -> None:
    """A browser left from an earlier check would take this launch over as a new window.

    That earlier check already failed for it; this one starts from a clean slate by
    closing its windows and stopping what is left, and fails only if it cannot."""
    def running():
        return [c.cmd for c in _procs(ctx).values() if app.program in c.cmd]

    left = running()
    if not left:
        return
    res.metrics["stopped_leftovers"] = left[:4]
    ctx.reset()
    # `[c]hromium-browser` matches the browser but not the shell running pkill.
    pattern = f"[{app.program[0]}]{app.program[1:]}"
    for signal in ("TERM", "KILL"):
        ctx.vm.exec(f"pkill -{signal} -u \"$(id -u)\" -f -- '{pattern}'; true")
        if wait_until(lambda: not running() or None, 10, 0.5):
            return
    left = running()
    expect(not left, f"a {app.program} from before is still running after SIGKILL ({left[:2]}): "
                     "this launch would open in it instead of starting the platform it asks for")


def _daily(app: DailyApp, interact, after_open=None):
    def body(ctx, res):
        _require_installed(ctx, res, app)
        if app.program == "chromium-browser":
            _no_running(ctx, res, app)
        gid, sid, procs, toasts0 = _open(ctx, res, app)
        if after_open:
            after_open(ctx, res)
        _across_outputs(ctx, res, app, gid, sid)
        interact(ctx, res, app, sid)
        _close(ctx, res, app.label, gid, procs, toasts0)
    return body


check("A08", AREA, "Chromium on Wayland (ozone, the Electron path): window, focus, content, "
                   "scale on both outputs, typing, clipboard both ways, exit on close")(
    _daily(CHROMIUM_WAYLAND, lambda ctx, res, app, sid: _text_field(ctx, res, app, sid, undo=False),
           lambda ctx, res: _ozone_flag(ctx, res, "wayland")))
check("A09", AREA, "Chromium forced onto X11 through Xwayland: window, focus, content, "
                   "scale on both outputs, typing, clipboard both ways, exit on close")(
    _daily(CHROMIUM_X11, lambda ctx, res, app, sid: _text_field(ctx, res, app, sid, undo=False),
           lambda ctx, res: _ozone_flag(ctx, res, "x11")))
check("A10", AREA, "GTK4/libadwaita (GNOME Text Editor): window, focus, content, fractional scale "
                   "on both outputs, typing, clipboard both ways, exit on close")(
    _daily(TEXT_EDITOR, lambda ctx, res, app, sid: _text_field(ctx, res, app, sid, undo=True)))
check("A11", AREA, "Qt6 on Wayland (FeatherPad): window, focus, content, fractional scale "
                   "on both outputs, typing, clipboard both ways, exit on close")(
    _daily(FEATHERPAD, lambda ctx, res, app, sid: _text_field(ctx, res, app, sid, undo=True)))
check("A12", AREA, "alacritty (winit, OpenGL): window, focus, content, fractional scale on both "
                   "outputs, typing into its shell, OSC 52 copy and paste, exit on close")(
    _daily(ALACRITTY, lambda ctx, res, app, sid: _terminal(ctx, res, app)))


def _fullscreen_state(s, gid) -> bool:
    g = core.group_by_id(s, gid) or {}
    return (s.get("chrome", {}).get("fullscreen_group") == gid or g.get("fullscreen") is True
            or g.get("maximized") is True)


def _fullscreen(ctx, res, app: DailyApp, sid, gid) -> None:
    """Super+F gives the X11 client its whole output and it redraws at that size; Super+F again
    gives it back its tile."""
    st = ctx.state()
    disp = core.display_of_group(st, gid)
    drect = core.display_rects(st)[disp]
    tile = _rect(_frame(ctx, sid))
    ctx.vm.key("super+f")
    ctx.wait_state(lambda s: _fullscreen_state(s, gid), 5, f"{app.name} full screen after Super+F")
    last = {}

    def filled():
        f = _frame(ctx, sid)
        if not f or not f.get("rect") or not f.get("buffer"):
            return None
        last["f"] = f
        r, b = f["rect"], f["buffer"]
        # A lone tile already spans most of its output (1856 of 1920 px): full screen
        # is larger than the tile it left as well.
        spans = (r["w"] >= drect[2] * 0.95 and r["h"] >= drect[3] * 0.9
                 and (r["w"] > tile[2] + SCALE_SLACK_PX or r["h"] > tile[3] + SCALE_SLACK_PX))
        redrawn = abs(b["w"] / r["w"] - b["h"] / r["h"]) < 0.05 and b["w"] >= r["w"] - SCALE_SLACK_PX
        return (spans and redrawn) or None

    ok = wait_until(filled, SCALE_SETTLE_S, 0.5)
    f = last.get("f") or {}
    res.metrics["fullscreen"] = {"display": list(drect), "rect": f.get("rect"), "buffer": f.get("buffer")}
    path = ctx.shot(f"{app.label}-fullscreen")
    if f.get("rect"):
        res.metrics["fullscreen_pixels"] = core.region_stats(core.load_image(path), _rect(f))
    expect(ok, f"{app.name} full screen: its frame {f.get('rect')} (buffer {f.get('buffer')}) does not "
               f"cover output {disp} {drect} at a matching size")
    expect(res.metrics.get("fullscreen_pixels", {}).get("stddev", 0) > 0.02,
           f"{app.name} full screen is a flat fill ({res.metrics.get('fullscreen_pixels')})")
    ctx.vm.key("super+f")
    ctx.wait_state(lambda s: not _fullscreen_state(s, gid), 5, f"{app.name} restored after Super+F")

    full = f.get("rect") or {"w": drect[2], "h": drect[3]}

    def back_in_tile():
        # Smaller than full screen again, by more than rounding.
        f = _frame(ctx, sid)
        r = f and f.get("rect")
        return f if r and (r["w"] < full["w"] - SCALE_SLACK_PX or r["h"] < full["h"] - SCALE_SLACK_PX) else None

    restored = wait_until(back_in_tile, SCALE_SETTLE_S, 0.5)
    res.metrics["restored_rect"] = restored["rect"] if restored else None
    expect(restored, f"{app.name} back in a tile like {tile} after leaving full screen")


@check("A13", AREA, "An OpenGL X11 client (glxgears, the games path): window, focus, content, scale "
                    "on both outputs, full screen and back, Escape reaches it, exit on close")
def gl_x11(ctx, res):
    app = GLXGEARS
    _require_installed(ctx, res, app)
    gid, sid, procs, toasts0 = _open(ctx, res, app)
    _across_outputs(ctx, res, app, gid, sid)
    _fullscreen(ctx, res, app, sid, gid)
    _close(ctx, res, app.label, gid, procs, toasts0)

    # The keyboard: glxgears quits on Escape, so a key that reaches it closes its window.
    first = dict(res.metrics)
    gid, sid, procs, toasts0 = _open(ctx, res, app)
    relaunch = {k: res.metrics.get(k) for k in ("seconds_to_window", "seconds_to_first_frame")}
    res.metrics.clear()
    res.metrics.update(first, escape_launch=relaunch)
    ctx.vm.key("Escape")
    gone = wait_until(lambda: core.group_by_id(ctx.state(), gid) is None or None, 10, 0.5)
    res.metrics["escape_closed_window"] = bool(gone)
    if not gone:
        ctx.shot(f"{app.label}-after-escape")
        close_group(ctx, gid)
    owned = {p: c for p, c in procs.items() if app.program in c.cmd}
    left = wait_until(lambda: not _alive(ctx, owned) or None, 10, 1.0)
    expect(gone, f"Escape pressed on the real keyboard did not reach {app.name} (its window stayed)")
    expect(left, f"{app.name} still running after Escape: {[c.cmd for c in _alive(ctx, owned).values()][:2]}")
