"""Essential 1: windows, groups, tabs, strips, workspaces, outputs, overview,
shortcuts, touch and touchpad gestures, animation pacing."""

from __future__ import annotations

import re
import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError

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


# OVERVIEW_TRANSITION_MS in the shell's overlay host; workspace switches use the same span.
ANIMATION_MS = 400


def _animation_window(ctx, res, name, act, undo):
    t0 = core.trace_now_us(ctx)
    act()
    time.sleep(1.2)
    undo()
    time.sleep(1.2)
    rows = core.trace_since(core.read_trace(ctx), t0 + 1)
    ctx.write(f"W16-{name}-frames.csv", "\n".join(",".join(str(v) for v in r.values()) for r in rows))
    stats = core.burst_stats(rows, ANIMATION_MS)
    res.metrics[name] = stats
    return stats


@check("W16", AREA, "Animations are paced: overview and workspace transitions paint many frames with no stalls")
def animations(ctx, res):
    ctx.reset()
    ctx.new_terminal_window()
    ctx.settle(1.5)
    budgets = ctx.config["budgets"]
    stats = _animation_window(ctx, res, "overview",
                              lambda: ctx.vm.key("super+Tab"), lambda: ctx.vm.key("Escape"))
    if ctx.facts.get("workspaces"):
        _animation_window(ctx, res, "workspace_switch",
                          lambda: ctx.vm.key("super+pagedown"), lambda: ctx.vm.key("super+pageup"))
    res.metrics["renderer"] = ctx.facts.get("renderer")
    problems = []
    for name, st in ((k, v) for k, v in res.metrics.items() if isinstance(v, dict)):
        # One burst for the open, one for the close; each should be an animation, not a jump.
        few = [n for n in st["frames_per_burst"][:2] if n < budgets["animation_min_frames"]]
        if len(st["frames_per_burst"]) < 2 or few:
            problems.append(f"{name}: frames per transition {st['frames_per_burst']} "
                            f"(a jump, not an animation; want >= {budgets['animation_min_frames']} "
                            f"in each of open and close)")
        if st["interval_ms_p95"] and st["interval_ms_p95"] > budgets["paint_period_ms_p95"]:
            problems.append(f"{name}: frame interval p95 {st['interval_ms_p95']} ms inside the "
                            f"transition > {budgets['paint_period_ms_p95']} ms")
        if st["interval_ms_max"] and st["interval_ms_max"] > budgets["paint_gap_ms_max"]:
            problems.append(f"{name}: a {st['interval_ms_max']} ms stall inside the transition "
                            f"> {budgets['paint_gap_ms_max']} ms")
    expect(not problems, "; ".join(problems) + f" [renderer: {ctx.facts.get('renderer')}]")
    del stats


def _find_probe(ctx, name):
    out = ctx.vm.exec(f'find "$HOME" /tmp -maxdepth 4 -name {name} 2>/dev/null | head -1; true')
    return [l for l in out.splitlines() if l.strip() and not l.startswith("receipt:")]


@check("W17", AREA, "Real keyboard: typed text, Space and Return reach the focused Terminal")
def typing(ctx, res):
    ctx.reset()
    ctx.new_terminal_window()
    ctx.settle(2.0)
    probe = f"acc-kbd-{int(time.time())}"
    ctx.vm.input("type", f"echo ok > ~/{probe}")
    ctx.vm.key("Return")
    time.sleep(2.0)
    real = _find_probe(ctx, probe)
    ctx.shot("W17-typed")
    res.metrics["real_keyboard_ran_command"] = bool(real)
    if real:
        return
    # Tell a product bug from a harness one: the same through the control socket.
    ctx.vm.key("ctrl+u")
    probe2 = probe + "-ctl"
    ctx.vm.control("type", f"echo ok > ~/{probe2}")
    ctx.vm.control("key", "Enter")
    time.sleep(2.0)
    synthetic = bool(_find_probe(ctx, probe2))
    res.metrics["control_socket_ran_command"] = synthetic
    ctx.shot("W17-after-control")
    ctx.close_overlays()
    raise CheckFailed("the command typed on the real keyboard never ran in Terminal "
                      "(W17-typed shows what arrived: spaces and Return are the usual casualties)"
                      + ("; the same through the control socket ran" if synthetic
                         else "; through the control socket it did not run either"))
