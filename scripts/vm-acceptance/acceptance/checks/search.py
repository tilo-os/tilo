"""Essential 4: search (empty view, English and German names, apps, windows,
actions, files) and the history / activity views."""

from __future__ import annotations

import secrets
import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, wait_until

AREA = "search"

EN_NAMES = [("Terminal", "so.tilo.terminal"), ("Files", "so.tilo.file-explorer"),
            ("Settings", "so.tilo.settings"), ("Notes", "so.tilo.notes"), ("Store", "so.tilo.store")]
DE_NAMES = [("Terminal", "so.tilo.terminal"), ("Dateien", "so.tilo.file-explorer"),
            ("Einstellungen", "so.tilo.settings"), ("Notizen", "so.tilo.notes")]


def _palette(st) -> dict:
    return st["chrome"].get("palette") or {}


def _open_palette(ctx):
    ctx.close_overlays()
    ctx.vm.key("super+space")
    return ctx.wait_state(lambda s: _palette(s).get("visible"), 5, "search open after Super+Space")


def _top_app(ctx, query):
    results = ctx.vm.actions(query)
    top = results[0] if results else None
    return top, [(a["provider"], a["id"]) for a in results[:5]]


@check("S01", AREA, "Super+Space opens search with a non-empty empty-query view")
def empty_view(ctx, res):
    ctx.reset()
    st = _open_palette(ctx)
    ctx.settle(1.0)
    st = ctx.state()
    rows = _palette(st).get("rows") or []
    res.metrics["rows"] = len(rows)
    res.metrics["row_sample"] = rows[:8]
    ctx.write("S01-palette.json", __import__("json").dumps(_palette(st), indent=1))
    ctx.shot("S01-empty-query")
    ctx.close_overlays()
    expect(rows, "the empty-query search view lists nothing")


def _names_check(ctx, res, names):
    wrong = []
    for query, app in names:
        top, head = _top_app(ctx, query)
        res.metrics[query] = head
        if not top or top["id"] != app:
            wrong.append(f"'{query}' -> {head[:2]} (want {app} first)")
    expect(not wrong, "; ".join(wrong))


@check("S02", AREA, "English app names find the app first (Terminal, Files, Settings, Notes, Store)")
def english(ctx, res):
    _names_check(ctx, res, EN_NAMES)


@check("S03", AREA, "German app names find the app first in a German session", needs=("locale_de",))
def german(ctx, res):
    ctx.facts["need_de"] = True
    _names_check(ctx, res, DE_NAMES)


@check("S04", AREA, "Typing an app name in search and pressing Enter opens it")
def palette_open(ctx, res):
    ctx.reset()
    _open_palette(ctx)
    ctx.vm.control("type", "Notes")
    st = ctx.wait_state(lambda s: any("Notes" in str(r) for r in (_palette(s).get("rows") or [])), 6,
                        "Notes among the search rows")
    res.metrics["rows"] = (_palette(st).get("rows") or [])[:5]
    ctx.shot("S04-typed")
    before = {g["id"] for g in core.all_groups(st)}
    ctx.vm.key("Return")
    st = ctx.wait_state(lambda s: [g for g in core.all_groups(s) if g["id"] not in before], 20,
                        "a window opened from search")
    new = [g for g in core.all_groups(st) if g["id"] not in before][0]
    res.metrics["opened"] = core.tab_app(new["tabs"][0])
    expect("notes" in str(res.metrics["opened"]).lower() or "Notes" in str(new["tabs"][0].get("title")),
           f"Enter opened {res.metrics['opened']}, not Notes")


@check("S05", AREA, "An open window is findable by name in search")
def windows(ctx, res):
    ctx.reset()
    ctx.new_terminal_window()
    ctx.settle(1.0)
    results = ctx.vm.actions("Terminal")
    wins = [a for a in results if a["provider"] == "desktop-windows"
            and not a["id"].startswith(("tab.close", "group.close"))]
    res.metrics["window_results"] = [(a["provider"], a["id"], a["title"]) for a in wins[:5]]
    res.metrics["all"] = [(a["provider"], a["id"]) for a in results[:10]]
    expect(wins, "search for 'Terminal' offers only close actions for the open window, "
                 "no result that switches to it")


@check("S06", AREA, "Actions are findable ('overview' finds Toggle workspace overview)")
def actions(ctx, res):
    results = ctx.vm.actions("overview")
    head = [(a["provider"], a["id"]) for a in results[:3]]
    res.metrics["top"] = head
    expect(("actions", "overview.toggle") in head, f"overview.toggle not in the top 3: {head}")


@check("S07", AREA, "A file in the home folder is found by name")
def files(ctx, res):
    needle = f"accneedle{secrets.token_hex(3)}"
    ctx.vm.write_home_file(f"Documents/{needle}.txt", b"acceptance search probe\n")
    started = time.monotonic()

    def hit():
        for a in ctx.vm.actions(needle):
            if needle in (a["title"] + a["id"] + str(a.get("subtitle", ""))):
                return a
        return None

    found = wait_until(hit, 45, 3)
    res.metrics["seconds_to_indexed"] = round(time.monotonic() - started, 1) if found else None
    res.metrics["hit"] = found and (found["provider"], found["id"], found["title"])
    ctx.vm.exec(f'rm -f "$HOME/Documents/{needle}.txt"', check=False)
    expect(found, f"'{needle}.txt' in ~/Documents not found by search within 45 s")


@check("S08", AREA, "Workspace activity opens, shows content and closes")
def activity(ctx, res):
    ctx.reset()
    before = {g["id"] for g in core.all_groups(ctx.state())}
    ctx.vm.invoke("actions", "room.activity.open")
    # The activity view is an overlay in some builds and a window tile in others.
    st = ctx.wait_state(lambda s: core.overlays(s)["room_activity"]
                        or any(g["id"] not in before for g in core.all_groups(s)), 5, "activity view open")
    tile = next((g for g in core.all_groups(st) if g["id"] not in before), None)
    res.metrics["presented_as"] = core.tab_app(tile["tabs"][0]) if tile else "overlay"
    ctx.settle(1.0)
    shot = ctx.shot("S08-activity")
    img = core.load_image(shot)
    d = core.display_rects(st)[0]
    stats = core.region_stats(img, (d[0] + d[2] * 0.25, d[1] + 60, d[2] * 0.5, d[3] * 0.6))
    res.metrics["center_pixels"] = stats
    expect(stats["stddev"] > 0.03, f"the activity view's centre is blank (stddev {stats['stddev']})")
    if tile is None:
        ctx.close_overlays()
        return
    try:
        ctx.vm.invoke("desktop-windows", f"group.close:{tile['id']}")
    except VmError as exc:
        raise CheckFailed(f"the activity window cannot be closed: {exc.reason()}") from exc
    ctx.wait_state(lambda s: core.group_by_id(s, tile["id"]) is None, 5, "activity window closed")


@check("S09", AREA, "Super+H opens the workspace history")
def history(ctx, res):
    ctx.reset()
    ctx.new_terminal_window()
    before = ctx.state()
    t_before = {t["name"] for t in ctx.vm.targets()}
    ctx.vm.key("super+h")
    ctx.settle(1.5)
    after = ctx.state()
    t_after = {t["name"] for t in ctx.vm.targets()}
    ctx.shot("S09-history")
    new_targets = sorted(t_after - t_before)
    changed = [k for k in after["chrome"] if after["chrome"][k] != before["chrome"].get(k) and k != "revision"]
    new_groups = len(core.all_groups(after)) - len(core.all_groups(before))
    # An activity window left open earlier (S08) is toggled or focused rather than duplicated.
    focus_moved = core.active_group_id(after) != core.active_group_id(before)
    res.metrics.update(new_targets=new_targets[:15], chrome_changed=changed, new_groups=new_groups,
                       focus_moved=focus_moved,
                       overlays={k: v for k, v in core.overlays(after).items() if v})
    ctx.close_overlays()
    expect(new_groups != 0 or focus_moved or any(core.overlays(after).values()) or new_targets,
           "Super+H changed nothing observable (no overlay, no window opened, closed or focused, "
           "no new target)")
