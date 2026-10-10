"""Essential 1: windows, groups, tabs, strips, workspaces, outputs, overview,
shortcuts, touch and touchpad gestures, animation pacing."""

from __future__ import annotations

import json
import re
import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, wait_until

AREA = "window"


def _two_windows(ctx):
    ctx.reset()
    g1 = ctx.new_terminal_window()
    g2 = ctx.new_terminal_window()
    ctx.settle()
    return g1["id"], g2["id"]


def _order(st):
    return [g["id"] for g in core.groups(st)]


def _group_rect(ctx, gid):
    """The laid-out rect of window group `gid`: the WindowGroup holding its tab."""
    t = core.target_map(ctx.vm.targets())
    # The tab bar hides in full screen; the tile target carries the group id too.
    tab = next((r for n, r in t.items() if n.startswith("tab:") and n.endswith(f"[{gid}]")), None)
    if tab is None:
        tab = next((r for n, r in t.items() if n.endswith(f"[{gid}]")), None)
    if tab is None:
        raise Unobservable(f"no target tagged with group {gid} in list-targets")
    cands = [r for n, r in t.items() if re.search(r"/WindowGroup(\[\d+\])?$", n)
             and core.rect_inside(tab, r)]
    if not cands:
        raise Unobservable(f"no WindowGroup target contains group {gid}'s tab")
    return min(cands, key=lambda r: r[2] * r[3])


@check("W01", AREA, "Two new windows open side by side and the newest takes focus")
def open_two(ctx, res):
    g1, g2 = _two_windows(ctx)
    st = ctx.state()
    order = _order(st)
    res.metrics["groups_in_strip"] = len(order)
    expect(g1 in order and g2 in order, f"both windows in the active strip (strip holds {order})")
    expect(core.active_group_id(st) == g2, f"newest window {g2} focused, focused is {core.active_group_id(st)}")
    ctx.facts["two_windows"] = (g1, g2)
    ctx.shot("W01-two-windows")


@check("W02", AREA, "Super+Left / Super+Right move focus between windows", needs=("two_windows",))
def spatial_focus(ctx, res):
    g1, g2 = ctx.facts["two_windows"]
    ctx.vm.key("super+Left")
    ctx.wait_state(lambda s: core.active_group_id(s) == g1, 5, "focus on the left window after Super+Left")
    ctx.vm.key("super+Right")
    ctx.wait_state(lambda s: core.active_group_id(s) == g2, 5, "focus back on the right window after Super+Right")


@check("W03", AREA, "Alt+Tab switches to the previously used window", needs=("two_windows",))
def alt_tab(ctx, res):
    g1, g2 = ctx.facts["two_windows"]
    ctx.vm.key("alt+Tab")
    ctx.wait_state(lambda s: core.active_group_id(s) == g1, 5, "Alt+Tab to the previous window")
    ctx.vm.key("alt+Tab")
    ctx.wait_state(lambda s: core.active_group_id(s) == g2, 5, "Alt+Tab back")
    ctx.close_overlays()


@check("W04", AREA, "Super+Shift+Left moves the focused window left in its strip", needs=("two_windows",))
def move_group(ctx, res):
    g1, g2 = ctx.facts["two_windows"]
    before = _order(ctx.state())
    ctx.vm.key("super+shift+Left")
    st = ctx.wait_state(lambda s: _order(s).index(g2) < _order(s).index(g1), 5,
                        f"window {g2} left of {g1} (order was {before})")
    expect(core.active_group_id(st) == g2, "the moved window keeps focus")
    ctx.vm.key("super+shift+Right")
    ctx.wait_state(lambda s: _order(s).index(g2) > _order(s).index(g1), 5, "window moved back right")


@check("W05", AREA, "Super+= / Super+- resize the focused window", needs=("two_windows",))
def resize(ctx, res):
    _, g2 = ctx.facts["two_windows"]
    w0 = _group_rect(ctx, g2)[2]
    ctx.vm.key("super+equal")
    ctx.settle(1.0)
    w1 = _group_rect(ctx, g2)[2]
    ctx.vm.key("super+minus")
    ctx.settle(1.0)
    w2 = _group_rect(ctx, g2)[2]
    res.metrics.update(width_px_before=w0, width_px_wider=w1, width_px_narrower=w2)
    expect(w1 > w0 + 10, f"Super+= widened the window ({w0:.0f} -> {w1:.0f} px)")
    expect(w2 < w1 - 10, f"Super+- narrowed it again ({w1:.0f} -> {w2:.0f} px)")


@check("W06", AREA, "Super+M maximizes and restores the focused window", needs=("two_windows",))
def maximize(ctx, res):
    _, g2 = ctx.facts["two_windows"]
    # Super+Up/Down move strip focus (docs/architecture/window-management.md).
    ctx.vm.key("super+m")
    ctx.wait_state(lambda s: (core.group_by_id(s, g2) or {}).get("maximized") is True, 5, "window maximized")
    ctx.shot("W06-maximized")
    ctx.vm.key("super+m")
    ctx.wait_state(lambda s: (core.group_by_id(s, g2) or {}).get("maximized") is False, 5, "window restored")


@check("W07", AREA, "Super+F makes the focused Tilo window full screen and back", needs=("two_windows",))
def fullscreen(ctx, res):
    _, g2 = ctx.facts["two_windows"]
    r0 = _group_rect(ctx, g2)
    ctx.vm.key("super+f")
    st = ctx.wait_state(lambda s: s.get("chrome", {}).get("fullscreen_group") == g2
                        or s.get("chrome", {}).get("fullscreen") is True
                        or (core.group_by_id(s, g2) or {}).get("maximized") is True
                        or (core.group_by_id(s, g2) or {}).get("fullscreen") is True,
                        5, "window full screen")
    r1 = _group_rect(ctx, g2)
    disp = core.display_rects(st)[core.display_of_group(st, g2) or 0]
    res.metrics.update(rect_before=r0, rect_full=r1, display=disp)
    ctx.shot("W07-fullscreen")
    expect(r1[2] >= disp[2] * 0.9, f"full-screen window spans its display ({r1[2]:.0f} of {disp[2]:.0f} px)")
    ctx.vm.key("super+f")
    ctx.wait_state(lambda s: not (core.group_by_id(s, g2) or {}).get("maximized")
                   and not (core.group_by_id(s, g2) or {}).get("fullscreen"), 5, "window restored")


@check("W08", AREA, "Tabs: add, switch, split into a group, close")
def tabs(ctx, res):
    ctx.reset()
    g = ctx.new_terminal_window()["id"]
    ctx.vm.invoke("actions", "tab.new.terminal.here")
    st = ctx.wait_state(lambda s: len((core.group_by_id(s, g) or {}).get("tabs", [])) == 2, 8,
                        "a second tab in the window")
    first_active = core.group_by_id(st, g)["active_tab"]
    ctx.vm.key("ctrl+Tab")
    ctx.wait_state(lambda s: core.group_by_id(s, g)["active_tab"] != first_active, 5, "Ctrl+Tab switched tab")
    ctx.shot("W08-two-tabs")
    n = len(core.groups(ctx.state()))
    ctx.vm.key("super+shift+Return")
    st = ctx.wait_state(lambda s: len(core.groups(s)) == n + 1, 5, "Super+Shift+Enter moved the tab into a new group")
    expect(len(core.group_by_id(st, g)["tabs"]) == 1, "the source window kept one tab")
    ctx.vm.key("super+w")
    ctx.wait_state(lambda s: len(core.groups(s)) == n, 5, "Super+W closed the focused tab")


@check("W09", AREA, "A window can be moved to another strip (keyboard or action route)")
def strips(ctx, res):
    ctx.reset()
    g = ctx.new_terminal_window()["id"]
    routes = [a for a in ctx.vm.actions("strip") if "strip" in (a["id"] + a["title"]).lower()]
    res.metrics["strip_actions"] = [a["id"] for a in routes]
    ws0 = core.active_workspace(ctx.state())
    n_strips = len(ws0["strips"])
    # The documented chords only move focus between existing strips; try them.
    ctx.vm.key("super+shift+Down")
    ctx.settle(1.0)
    ws1 = core.active_workspace(ctx.state())
    res.metrics.update(strips_before=n_strips, strips_after=len(ws1["strips"]))
    moved = any(g in [x["id"] for x in s["window_groups"]] for s in ws1["strips"]
                if s["id"] != ws0["strips"][0]["id"])
    expect(routes or moved or len(ws1["strips"]) > n_strips,
           "no action or chord creates a strip or moves a window to one "
           "(Super+Shift+Down only walks existing strips; no `strip` action is exported)")


@check("W10", AREA, "A new workspace can be created and switched to with Super+Page Down/Up")
def workspaces(ctx, res):
    ctx.reset()
    st = ctx.state()
    n0 = len(st["workspace_tree"]["workspaces"])
    if n0 < 2:
        ctx.vm.key("super+Tab")
        ctx.wait_state(lambda s: core.overlays(s)["overview"], 5, "overview open")
        names = [t["name"] for t in ctx.vm.targets()]
        add = [n for n in names if re.search(r"(new|add)[_-]?workspace", n, re.I)]
        res.metrics["add_workspace_targets"] = add
        if not add:
            ctx.shot("W10-overview")
            raise CheckFailed("overview offers no addressable `new workspace` control "
                              "(no list-targets entry, no action)")
        ctx.vm.control("click", add[0])
        ctx.wait_state(lambda s: len(s["workspace_tree"]["workspaces"]) > n0, 6, "a new workspace")
        ctx.close_overlays()
    st = ctx.state()
    first = st["workspace_tree"]["active"]
    ids = [w["id"] for w in st["workspace_tree"]["workspaces"]]
    # Page Down walks towards the end of the list; from the last workspace walk back first.
    there, back = ("super+pagedown", "super+pageup")
    if ids.index(first) == len(ids) - 1:
        there, back = back, there
    res.metrics.update(workspaces=len(ids), start_index=ids.index(first), first_chord=there)
    ctx.vm.key(there)
    st = ctx.wait_state(lambda s: s["workspace_tree"]["active"] != first, 5, f"{there} switched workspace")
    ctx.shot("W10-other-workspace")
    ctx.vm.key(back)
    ctx.wait_state(lambda s: s["workspace_tree"]["active"] == first, 5, f"{back} switched back")
    ctx.facts["workspaces"] = True


@check("W11", AREA, "Super+Ctrl+Shift+Right/Left move a window between the two outputs")
def outputs(ctx, res):
    ctx.reset()
    st = ctx.state()
    expect(len(st["chrome"]["displays"]) >= 2, f"two outputs present (have {len(st['chrome']['displays'])})")
    g = ctx.new_terminal_window()["id"]
    d0 = core.display_of_group(ctx.state(), g)
    ctx.vm.key("super+ctrl+shift+Right")
    st = ctx.wait_state(lambda s: core.display_of_group(s, g) not in (None, d0), 5,
                        f"window on the other output (was on {d0})")
    res.metrics["moved_to_display"] = core.display_of_group(st, g)
    ctx.shot("W11-second-output")
    ctx.vm.key("super+ctrl+shift+Left")
    ctx.wait_state(lambda s: core.display_of_group(s, g) == d0, 5, "window back on the first output")


@check("W12", AREA, "Overview opens and closes with Super+Tab, Escape and its action")
def overview(ctx, res):
    ctx.reset()
    ctx.new_terminal_window()
    ctx.vm.key("super+Tab")
    ctx.wait_state(lambda s: core.overlays(s)["overview"], 5, "overview open after Super+Tab")
    ctx.shot("W12-overview")
    ctx.vm.key("Escape")
    ctx.wait_state(lambda s: not core.overlays(s)["overview"], 5, "overview closed after Escape")
    ctx.vm.invoke("actions", "overview.toggle")
    ctx.wait_state(lambda s: core.overlays(s)["overview"], 5, "overview open after overview.toggle")
    ctx.vm.invoke("actions", "overview.toggle")
    ctx.wait_state(lambda s: not core.overlays(s)["overview"], 5, "overview closed after overview.toggle")


@check("W13", AREA, "Super+S summons and dismisses the workspace wheel")
def wheel(ctx, res):
    ctx.reset()
    d0 = core.display_rects(ctx.state())[0]
    px, py = int(d0[0] + d0[2] / 2), int(d0[1] + d0[3] / 2)
    ctx.vm.input("move", str(px), str(py))
    ctx.settle(0.5)
    before_disc = core.target_map(ctx.vm.targets()).get("wheel_disc")
    ctx.vm.key("super+s")
    st = ctx.wait_state(lambda s: s["chrome"].get("wheel_summon"), 5,
                        "chrome.wheel_summon after Super+S")
    summoned = st["chrome"]["wheel_summon"]
    after_disc = core.target_map(ctx.vm.targets()).get("wheel_disc")
    expect(after_disc, "summoned wheel stays addressable as wheel_disc")
    # A chrome flag without a moved disc is the same lie the previous
    # check was blind to: the target name never changes, only the rect.
    moved = (before_disc is None
             or abs(after_disc[0] - before_disc[0]) > 20
             or abs((after_disc[0] + after_disc[2]) - px) < 40)
    expect(moved, f"wheel_disc stayed at {before_disc} after summon to {px},{py} (now {after_disc})")
    res.metrics.update(pointer=[px, py], wheel_summon=summoned,
                       wheel_disc_before=before_disc, wheel_disc_after=after_disc)
    ctx.shot("W13-wheel")
    ctx.vm.key("Escape")
    ctx.wait_state(lambda s: not s["chrome"].get("wheel_summon"), 5,
                   "chrome.wheel_summon cleared after Escape")


@check("W14", AREA, "Touch: a tap on a window focuses it; a horizontal swipe pans the strip")
def touch(ctx, res):
    g1, g2 = _two_windows(ctx)
    r = _group_rect(ctx, g1)
    st = ctx.state()
    d0 = core.display_rects(st)[0]
    x0, x1 = max(r[0], d0[0]), min(r[0] + r[2], d0[0] + d0[2])
    expect(x1 - x0 >= 80, f"left window visible on the first output for a tap ({r})")
    x, y = int((x0 + x1) / 2), int(r[1] + r[3] / 2)
    ctx.vm.input("touch", "tap", str(x), str(y))
    ctx.wait_state(lambda s: core.active_group_id(s) == g1, 5, f"tap at {x},{y} focused the left window")
    ctx.shot("W14-tapped")
    st = ctx.state()
    strip = next(x for x in st["chrome"]["strip_scroll"] if x["strip"] == core.active_strip(st)["id"])
    # Swipe towards the side with room: at the right end a leftward swipe is clamped.
    rightward = strip["scroll_x"] > strip["max_scroll_x"] / 2
    x_from, x_to = (d0[2] * 0.3, d0[2] * 0.8) if rightward else (d0[2] * 0.8, d0[2] * 0.3)
    attempts = {"tab_bar": r[1] + 16, "content": r[1] + r[3] * 0.6}
    panned = []
    for where, y in attempts.items():
        before = strip["scroll_x"]
        ctx.vm.input("touch", "drag", str(int(x_from)), str(int(y)), str(int(x_to)), str(int(y)))
        ctx.settle(1.2)
        st = ctx.state()
        strip = next(x for x in st["chrome"]["strip_scroll"] if x["strip"] == strip["strip"])
        res.metrics[f"scroll_{where}"] = [round(before, 1), round(strip["scroll_x"], 1)]
        if abs(strip["scroll_x"] - before) > 20:
            panned.append(where)
    res.metrics["panned_by"] = panned
    expect(panned, "a one-finger horizontal touch swipe (over the tab bar or the window) did not pan "
                   f"the strip ({'rightward' if rightward else 'leftward'}; scroll before/after: "
                   f"tab bar {res.metrics['scroll_tab_bar']}, window {res.metrics['scroll_content']})")


def _pointer_kind(st) -> str:
    return (st["chrome"].get("pointer") or {}).get("kind", "")


def _app_groups(st, app_id: str) -> list:
    return [g for g in core.all_groups(st)
            if any((t.get("tile") or {}).get("Foreign", {}).get("app_id") == app_id
                   for t in g["tabs"])]


@check("W18", AREA, "Touch is not the mouse: a tap on a launchpad card opens its app, the cursor "
                    "stays hidden while touch is in use and returns with the mouse")
def touch_first(ctx, res):
    ctx.reset()
    # A visible mouse cursor first, somewhere away from the launchpad cards.
    ctx.vm.input("move", "8", "300")
    ctx.wait_state(lambda s: _pointer_kind(s) != "hidden", 5, "a visible cursor after moving the mouse")
    t = core.target_map(ctx.vm.targets())
    card = t.get("launchpad:notes")
    if card is None:
        raise Unobservable("the empty workspace shows no launchpad:notes target "
                           f"(targets: {sorted(n for n in t if n.startswith('launchpad'))})")
    x, y = int(card[0] + card[2] / 2), int(card[1] + card[3] / 2)
    ctx.vm.input("touch", "tap", str(x), str(y))
    st = ctx.wait_state(lambda s: _pointer_kind(s) == "hidden", 5,
                        "the cursor hidden after a touch")
    res.metrics["pointer_after_tap"] = st["chrome"].get("pointer")
    st = ctx.wait_state(lambda s: _app_groups(s, "so.tilo.notes"), 25,
                        f"Notes opened by a tap on its launchpad card at {x},{y}")
    expect(st["chrome"]["input"]["modality"] == "touch",
           f"touch is the input in use after a tap ({st['chrome']['input']})")
    ctx.shot("W18-tapped-card")
    ctx.vm.input("move", "12", "300")
    ctx.wait_state(lambda s: _pointer_kind(s) != "hidden", 5, "the cursor back after moving the mouse")


@check("W19", AREA, "On-screen keyboard: tapping a text field shows it, tapping its keys types into "
                    "the field, and typing on the physical keyboard hides it")
def on_screen_keyboard(ctx, res):
    ctx.reset()
    group, _ = ctx.open_app("so.tilo.scratchpad")
    ctx.settle(2.0)
    r = _group_rect(ctx, group["id"])
    # Scratchpad is one multi-line field filling the window under its heading.
    x, y = int(r[0] + r[2] / 2), int(r[1] + r[3] * 0.65)
    # The first touch plugs the touchscreen in; with one present the keyboard
    # service runs in the default automatic mode.
    ctx.vm.input("touch", "tap", str(x), str(y))
    ctx.wait_state(lambda s: (s["chrome"].get("on_screen_keyboard") or {}).get("connected"), 40,
                   "the on-screen keyboard service connected once a touchscreen is present")
    ctx.vm.input("touch", "tap", str(x), str(y))
    st = ctx.wait_state(lambda s: (s["chrome"].get("on_screen_keyboard") or {}).get("shown"), 15,
                        f"the on-screen keyboard shown after tapping the field at {x},{y}")
    osk = st["chrome"]["on_screen_keyboard"]
    res.metrics["keyboard"] = osk["shown"]
    ctx.shot("W19-keyboard")
    k = osk["shown"]
    before = osk.get("commits", 0)
    # Letter keys fill the middle rows; three taps across the second row.
    for fx in (0.2, 0.45, 0.7):
        ctx.vm.input("touch", "tap", str(int(k["x"] + k["w"] * fx)), str(int(k["y"] + k["h"] * 0.55)))
        ctx.settle(0.4)
    st = ctx.wait_state(lambda s: (s["chrome"].get("on_screen_keyboard") or {}).get("commits", 0) > before,
                        10, "text committed into the field by tapping keyboard keys")
    res.metrics["commits"] = st["chrome"]["on_screen_keyboard"]["commits"] - before
    expect(_group_rect(ctx, group["id"]) is not None, "the field's window is still open")
    ctx.shot("W19-typed")
    ctx.vm.input("type", "x")
    ctx.wait_state(lambda s: not (s["chrome"].get("on_screen_keyboard") or {}).get("shown"), 10,
                   "the on-screen keyboard hidden after typing on the physical keyboard")


def _keyboard(st) -> dict:
    return st["chrome"].get("on_screen_keyboard") or {}


# Where W20 may touch the empty workspace to plug the touchscreen in: low on the first
# output, below the launchpad's card grid, as fractions of the output.
_INERT_TOUCH = ((0.5, 0.88), (0.12, 0.88), (0.88, 0.88), (0.5, 0.96))


def _inert_point(ctx, display) -> tuple[int, int]:
    """A point on `display` inside no launchpad card or button. The empty workspace is the
    launchpad, and a touch on a card activates it like a click: run 89618's W20 tapped
    "Invite people" with its fixed 60 %/60 % point and left the Invite composer holding the
    keyboard, so search never opened and A01 typed its Terminal command into the composer."""
    cards = [r for n, r in core.target_map(ctx.vm.targets()).items() if n.startswith("launchpad:")]
    for fx, fy in _INERT_TOUCH:
        x, y = int(display[0] + display[2] * fx), int(display[1] + display[3] * fy)
        if not any(core.rect_inside((x, y, 0, 0), r, slack=8.0) for r in cards):
            return x, y
    raise Unobservable(f"every touch point W20 tries on the first output is on a launchpad card ({cards})")


def _close_search(ctx) -> None:
    """Search closed again, whatever W20 got to: Escape while it shows (bounded)."""
    for _ in range(3):
        try:
            if not (ctx.state()["chrome"].get("palette") or {}).get("visible"):
                return
            ctx.vm.key("Escape")
        except (VmError, KeyError, TypeError):
            return
        ctx.settle(0.4)


@check("W20", AREA, "On-screen keyboard in the shell: tapping the search field shows it, and its keys "
                    "type into the shell's own field")
def on_screen_keyboard_in_the_shell(ctx, res):
    ctx.reset()
    try:
        _on_screen_keyboard_in_the_shell(ctx, res)
    finally:
        _close_search(ctx)


def _on_screen_keyboard_in_the_shell(ctx, res):
    d0 = core.display_rects(ctx.state())[0]
    # The first touch plugs the touchscreen in; with one present the keyboard
    # service runs in the default automatic mode. It must activate nothing.
    x, y = _inert_point(ctx, d0)
    res.metrics["first_touch"] = [x, y]
    ctx.vm.input("touch", "tap", str(x), str(y))
    ctx.wait_state(lambda s: _keyboard(s).get("connected"), 40,
                   "the on-screen keyboard service connected once a touchscreen is present")
    ctx.vm.key("super+space")
    ctx.wait_state(lambda s: (s["chrome"].get("palette") or {}).get("visible"), 5, "search open")
    t = core.target_map(ctx.vm.targets())
    fields = sorted((r for n, r in t.items() if "SearchField" in n), key=lambda r: r[2] * r[3])
    if not fields:
        raise Unobservable(f"no SearchField among the targets while search is open ({len(t)} targets)")
    f = fields[-1]
    ctx.vm.input("touch", "tap", str(int(f[0] + f[2] / 2)), str(int(f[1] + f[3] / 2)))
    st = ctx.wait_state(lambda s: _keyboard(s).get("shown"), 15,
                        "the on-screen keyboard shown for the search field")
    kb = _keyboard(st)
    expect(kb.get("serves_shell_field"), f"the keyboard serves the shell's own field ({kb})")
    res.metrics["keyboard"] = kb["shown"]
    ctx.shot("W20-keyboard-over-search")
    k = kb["shown"]
    before = kb.get("commits", 0)
    for fx in (0.2, 0.45, 0.7):
        ctx.vm.input("touch", "tap", str(int(k["x"] + k["w"] * fx)), str(int(k["y"] + k["h"] * 0.55)))
        ctx.settle(0.4)
    st = ctx.wait_state(lambda s: (s["chrome"].get("palette") or {}).get("query"), 10,
                        "the keyboard's keys typed into the search field")
    res.metrics["query"] = st["chrome"]["palette"]["query"]
    res.metrics["commits"] = _keyboard(st).get("commits", 0) - before
    expect((st["chrome"].get("palette") or {}).get("visible"),
           "tapping the keyboard kept search open")
    ctx.shot("W20-typed")


def _lock_auth(st) -> dict:
    return st["chrome"].get("lock_auth") or {}


def _locked(ctx):
    """Whether the session is locked: True or False, or None when no state dump answers."""
    try:
        return bool(ctx.state()["chrome"].get("session_locked"))
    except (VmError, KeyError, TypeError):
        return None


def _expect_snapshot_refused_while_locked(ctx) -> None:
    """A security property, not a picture: while locked the compositor refuses every
    control-socket snapshot with `session_locked`, so no socket client reads the desktop the
    lock hides. One attempt, no retry, to a scratch file that never becomes evidence and is
    deleted whatever happened."""
    probe = ctx.out / "scratch" / "W21-locked-snapshot.png"
    try:
        ctx.vm.snapshot_once(probe)
    except VmError as exc:
        if "session_locked" not in str(exc):
            raise Unobservable("a snapshot while locked failed, but not as refused by the lock: "
                               f"{exc.reason()}") from exc
        return
    finally:
        probe.unlink(missing_ok=True)
    raise CheckFailed("a control-socket snapshot succeeded while the session was locked: the lock "
                      "screen lets a socket client read the desktop it hides")


def _ensure_unlocked(ctx, passcode: str) -> str:
    """Leave the session unlocked on every exit path, and say how. First the way a person
    does it: clear the lock's field (Escape), type the passcode on the real keyboard, Return.
    Still locked after that, restart the slot's session (a fresh compositor starts unlocked).
    Never raises, so it cannot replace the failure that brought it here."""
    if _locked(ctx) is False:
        return "none"
    try:
        ctx.vm.key("Escape")
        ctx.vm.input("type", passcode)
        ctx.vm.key("Return")
        if wait_until(lambda: _locked(ctx) is False, 20, interval=0.5):
            return "passcode typed on the real keyboard"
    except VmError:
        pass
    try:
        ctx.vm.restart_session()
    except VmError as exc:
        return f"failed: still locked, and restarting the slot session failed: {exc.reason()}"
    return "slot session restarted" if _locked(ctx) is False else "failed: still locked after a slot restart"


@check("W21", AREA, "Touch unlock: the lock screen draws its own keyboard on touch, refuses to be "
                    "snapshotted, and tapping the passcode on it unlocks the session")
def touch_unlock(ctx, res):
    ctx.reset()
    # The slot user's account password is what the lock verifies (PAM through
    # the unlock helper). This throwaway VM's account gets a known one.
    passcode = "touch7"
    uid = ctx.vm.slot_uid()
    ctx.vm.root_exec(f'user=$(getent passwd {uid} | cut -d: -f1) && '
                     f'echo "$user:{passcode}" | chpasswd')
    # Every later check shares this session, so whatever fails in here, it ends unlocked
    # (and run_check fails any check that leaves it locked, then restarts the session).
    try:
        ctx.vm.key("super+l")
        ctx.wait_state(lambda s: s["chrome"].get("session_locked"), 10, "the session locked")
        expect(not _lock_auth(ctx.state()).get("keyboard"),
               "no keyboard on the lock screen before a touch")
        # A touch near the top wakes the lock screen and brings its keyboard up;
        # that touch itself types nothing.
        d0 = core.display_rects(ctx.state())[0]
        ctx.vm.input("touch", "tap", str(int(d0[0] + d0[2] / 2)), str(int(d0[1] + d0[3] * 0.08)))
        st = ctx.wait_state(lambda s: _lock_auth(s).get("keyboard"), 10,
                            "the lock screen's keyboard shown after a touch")
        expect(_lock_auth(st).get("password_len") == 0,
               "the touch that showed the keyboard typed nothing")
        keys = {k["key"]: k for k in _lock_auth(st)["keyboard"]}
        # No picture of a locked session may be taken, and the next line holds the lock to
        # that; the evidence is the lock's own state: its keys and where they are.
        _expect_snapshot_refused_while_locked(ctx)
        ctx.write("W21-lock-keyboard.json", json.dumps(_lock_auth(st), indent=1))

        def tap(name):
            k = keys[name]
            ctx.vm.input("touch", "tap", str(int(k["x"] + k["w"] / 2)), str(int(k["y"] + k["h"] / 2)))
            ctx.settle(0.3)

        for c in passcode:
            tap(c)
        ctx.wait_state(lambda s: _lock_auth(s).get("password_len") == len(passcode), 10,
                       f"{len(passcode)} characters typed on the lock screen's keyboard")
        tap("enter")
        ctx.wait_state(lambda s: not s["chrome"].get("session_locked"), 20,
                       "the session unlocked with the passcode typed by touch")
        res.metrics["keys"] = len(keys)
    finally:
        res.metrics["unlock_recovery"] = _ensure_unlocked(ctx, passcode)
    ctx.shot("W21-unlocked")


def _strip_scroll(st, sid):
    return next(x for x in st["chrome"]["strip_scroll"] if x["strip"] == sid)


@check("W15", AREA, "Touchpad: a 3-finger pinch toggles the overview, a 3-finger horizontal swipe "
                    "pans the strip, a 4-finger vertical swipe switches workspace", needs=("workspaces",))
def touchpad(ctx, res):
    # `tilo-control swipe/pinch` feed the gesture tracker the DRM runtime feeds from libinput,
    # so this covers the tracker and what it drives, not libinput's touchpad decoding.
    res.metrics["route"] = "control socket swipe/pinch (the shell's gesture tracker)"
    _two_windows(ctx)
    try:
        ctx.vm.control("pinch", "3", "0.5")
    except VmError as exc:
        raise Unobservable(f"the control socket takes no synthetic touchpad gesture: {exc}") from exc
    ctx.wait_state(lambda s: core.overlays(s)["overview"], 5, "overview after a 3-finger pinch in")
    ctx.shot("W15-pinch-overview")
    ctx.vm.control("pinch", "3", "2.0")
    ctx.wait_state(lambda s: not core.overlays(s)["overview"], 5, "overview closed after a 3-finger pinch out")

    st = ctx.state()
    sid = core.active_strip(st)["id"]
    strip = _strip_scroll(st, sid)
    expect(strip["max_scroll_x"] > 20, f"two windows overflow the strip so it can pan ({strip})")
    # Pan towards the side with room: at either end one direction is clamped.
    dx = 600 if strip["scroll_x"] > strip["max_scroll_x"] / 2 else -600
    before = strip["scroll_x"]
    ctx.vm.control("swipe", "3", str(dx), "0")
    ctx.settle(1.2)
    after = _strip_scroll(ctx.state(), sid)["scroll_x"]
    res.metrics["strip_pan"] = {"dx": dx, "scroll_before": round(before, 1), "scroll_after": round(after, 1)}
    expect(abs(after - before) > 20, f"a 3-finger swipe of dx={dx} did not pan the strip "
                                     f"(scroll {before:.1f} -> {after:.1f})")

    expect(ctx.facts.get("workspaces"), "a second workspace to switch to (W10 did not make one)")
    first = ctx.state()["workspace_tree"]["active"]
    for dy in (-300, 300):
        ctx.vm.control("swipe", "4", "0", str(dy))
        ctx.settle(1.0)
        if ctx.state()["workspace_tree"]["active"] != first:
            break
    else:
        raise CheckFailed("neither a 4-finger swipe up nor down switched workspace")
    res.metrics["workspace_swipe_dy"] = dy
    ctx.shot("W15-swipe-workspace")
    ctx.vm.control("swipe", "4", "0", str(-dy))
    ctx.wait_state(lambda s: s["workspace_tree"]["active"] == first, 5,
                   "the opposite 4-finger swipe switched back")


# A transition is over once the compositor has asked for no frame for this long. In a
# VM the display may take frames slowly; a held transition then paints its steps more
# slowly, never fewer of them, so the wait is generous.
TRANSITION_QUIET_MS = 500
TRANSITION_TIMEOUT_S = 20


def _transition(ctx, res, name, act, reached):
    mark = core.trace_mark(ctx)
    act()
    rows, quiet = core.trace_until_quiet(ctx, mark, TRANSITION_QUIET_MS, TRANSITION_TIMEOUT_S)
    ctx.write(f"W16-{name}-frames.csv", core.trace_csv(rows))
    st = core.transition_stats(rows)
    st["settled"] = quiet
    st["cpu_recorded"] = core.has_cpu(rows)
    res.metrics[name] = st
    if not reached(ctx.state()):
        raise CheckFailed(f"{name}: the transition did not reach its end state")


def _judge(name, st, b, hardware) -> tuple[list[str], list[str]]:
    """(problems, unobservable) for one transition."""
    problems, unseen = [], []
    if not st["settled"] or (st["frames"] == 0 and st["blocked_ticks"]):
        held = (f"{name}: the display took {st['frames']} frames in {TRANSITION_TIMEOUT_S} s while "
                f"the compositor waited {st['display_wait_ms']} ms on it with a frame ready")
        if hardware:
            problems.append(held)
        else:
            unseen.append(held + " (the virtual display stalled; QEMU presents on its own timer)")
        return problems, unseen
    if st["frames"] < b["animation_min_frames"]:
        problems.append(f"{name}: {st['frames']} frames painted (a jump, not an animation; "
                        f"want >= {b['animation_min_frames']})")
    if st["late_ms_p95"] is not None and st["late_ms_p95"] > b["frame_late_ms_p95"]:
        cpu = st.get("longest_tick_cpu_ms")
        split = ("" if cpu is None else
                 f"; its longest loop tick ran {st['longest_tick_ms']} ms, {cpu} ms of it on the CPU, "
                 f"blocked or descheduled for the rest")
        problems.append(f"{name}: frames submitted p95 {st['late_ms_p95']} ms after the compositor's "
                        f"own deadline > {b['frame_late_ms_p95']} ms{split}")
    if not st["cpu_recorded"]:
        unseen.append(f"{name}: the frame trace carries no per-tick CPU time (cpu_us); the "
                      "installed compositor predates it")
    elif st["cpu_ms_max"] > b["frame_cpu_ms_max"]:
        problems.append(f"{name}: a {st['cpu_ms_max']} ms CPU stall in the compositor loop "
                        f"> {b['frame_cpu_ms_max']} ms")
    if st["input_to_first_frame_compositor_ms"] is None:
        unseen.append(f"{name}: no painted frame carried the input's latency")
    elif st["input_to_first_frame_compositor_ms"] > b["input_first_frame_ms"]:
        problems.append(f"{name}: first frame {st['input_to_first_frame_compositor_ms']} ms after the "
                        f"input (display waits taken out) > {b['input_first_frame_ms']} ms")
    if hardware:
        if (st["span_ms"] or 0) > b["transition_ms_max"]:
            problems.append(f"{name}: the transition took {st['span_ms']} ms first to last frame "
                            f"> {b['transition_ms_max']} ms")
        if (st["present_interval_ms_p95"] or 0) > b["paint_period_ms_p95"]:
            problems.append(f"{name}: frame interval p95 {st['present_interval_ms_p95']} ms "
                            f"> {b['paint_period_ms_p95']} ms")
        if (st["present_interval_ms_max"] or 0) > b["paint_gap_ms_max"]:
            problems.append(f"{name}: a {st['present_interval_ms_max']} ms stall between frames "
                            f"> {b['paint_gap_ms_max']} ms")
    return problems, unseen


def judge_switch_lines(switches: list[dict], expected: int, b: dict, hardware: bool) -> list[str]:
    """Problems in the compositor's own `WORKSPACE-SWITCH` timing of `expected` switches.

    Everywhere: one line per switch, and a frame run rather than a jump. On real display
    hardware also the first frame shown and the intervals without a new frame. A virtual
    display presents on its own timer and holds frames back, so there its times are only
    recorded; the trace-based judgement above takes the display's waits out instead."""
    if len(switches) != expected:
        return [f"{expected} workspace switches, {len(switches)} WORKSPACE-SWITCH lines in the session log"]
    problems = []
    for index, switch in enumerate(switches, 1):
        name = f"switch {index}"
        if switch["frames"] < b["animation_min_frames"]:
            problems.append(f"{name}: {switch['frames']} frames in the switch's frame run "
                            f"(want >= {b['animation_min_frames']})")
        if not hardware:
            continue
        first = switch["first_shown_us"] / 1000
        if first > b["input_first_frame_ms"]:
            problems.append(f"{name}: first frame shown {first:.1f} ms after the chord "
                            f"> {b['input_first_frame_ms']} ms")
        if switch["dropped"]:
            problems.append(f"{name}: {switch['dropped']} refresh intervals without a new frame")
    return problems


@check("W16", AREA, "Animations are paced: overview and workspace transitions paint many frames "
                    "with no stalls", needs=("workspaces",))
def animations(ctx, res):
    # The compositor's side is judged everywhere: frames painted per transition, their
    # lateness against its own deadlines, CPU per tick, input to first frame. On a
    # virtual display, presentation intervals are the host's and are only recorded.
    ctx.reset()
    ctx.new_terminal_window()
    ctx.settle(1.5)
    display = ctx.facts.get("display") or {}
    hardware = bool(display.get("hardware"))
    res.metrics.update(renderer=ctx.facts.get("renderer"), display=display)
    overview = lambda s: core.overlays(s)["overview"]  # noqa: E731
    _transition(ctx, res, "overview_open", lambda: ctx.vm.key("super+Tab"), overview)
    _transition(ctx, res, "overview_close", lambda: ctx.vm.key("Escape"), lambda s: not overview(s))
    st = ctx.state()
    first = st["workspace_tree"]["active"]
    ids = [w["id"] for w in st["workspace_tree"]["workspaces"]]
    # Page Down walks towards the end of the list; from the last workspace walk back first.
    there, back = ("super+pagedown", "super+pageup")
    if ids.index(first) == len(ids) - 1:
        there, back = back, there
    log_mark = core.session_log_mark(ctx)
    _transition(ctx, res, "workspace_switch", lambda: ctx.vm.key(there),
                lambda s: s["workspace_tree"]["active"] != first)
    _transition(ctx, res, "workspace_back", lambda: ctx.vm.key(back),
                lambda s: s["workspace_tree"]["active"] == first)
    switches = core.switch_lines_after(ctx, log_mark)
    res.metrics["workspace_switch_lines"] = switches
    b = ctx.config["budgets"]
    problems, unseen = [], []
    for name in ("overview_open", "overview_close", "workspace_switch", "workspace_back"):
        p, u = _judge(name, res.metrics[name], b, hardware)
        problems += p
        unseen += u
    problems += judge_switch_lines(switches, 2, b, hardware)
    where = f" [renderer: {ctx.facts.get('renderer')}]"
    expect(not problems, "; ".join(problems + unseen) + where)
    if unseen:
        raise Unobservable("; ".join(unseen) + where)


@check("W17", AREA, "Real keyboard: typed text, Space and Return reach the focused Terminal")
def typing(ctx, res):
    ctx.reset()
    shells = core.terminal_shells(ctx)
    ctx.new_terminal_window()
    # Keys typed before the shell runs only echo; this check is about the keys.
    ready = core.wait_new_terminal_shell(ctx, shells, core.TERMINAL_SHELL_S)
    res.metrics["seconds_to_shell"] = ready
    if ready is None:
        ctx.write("W17-ush.txt", core.ush_diagnostics(ctx))
        raise CheckFailed(f"no shell ran in the new Terminal within {core.TERMINAL_SHELL_S} s, "
                          "so nothing could read the keys (W17-ush.txt shows the shell runtime)")
    ctx.settle(1.0)
    probe = f"acc-kbd-{int(time.time())}"
    ctx.vm.input("type", f"echo ok > ~/{probe}")
    ctx.vm.key("Return")
    real = wait_until(lambda: core.terminal_probe(ctx, probe), 4, 0.5)
    ctx.shot("W17-typed")
    res.metrics["real_keyboard_ran_command"] = bool(real)
    if real:
        return
    # Tell a product bug from a harness one: the same through the control socket.
    ctx.vm.key("ctrl+u")
    probe2 = probe + "-ctl"
    ctx.vm.control("type", f"echo ok > ~/{probe2}")
    ctx.vm.control("key", "Enter")
    synthetic = bool(wait_until(lambda: core.terminal_probe(ctx, probe2), 4, 0.5))
    res.metrics["control_socket_ran_command"] = synthetic
    ctx.shot("W17-after-control")
    ctx.write("W17-ush.txt", core.ush_diagnostics(ctx))
    ctx.close_overlays()
    raise CheckFailed("the command typed on the real keyboard never ran in Terminal "
                      "(W17-typed shows what arrived: spaces and Return are the usual casualties)"
                      + ("; the same through the control socket ran" if synthetic
                         else "; through the control socket it did not run either"))
