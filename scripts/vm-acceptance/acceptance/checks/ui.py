"""Essential 2: Settings, quick settings, workspace settings and shell chrome
in each theme and language: automated layout checks plus shots for review."""

from __future__ import annotations

import json
import re
import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect

AREA = "ui"

# Each theme as the palette rows that select it, by provider and id: a mode
# leaves high contrast, and the high contrast switch then turns it on.
THEMES = {
    "light": (("actions", "appearance.theme.light"),),
    "dark": (("actions", "appearance.theme.dark"),),
    "high-contrast": (
        ("actions", "appearance.theme.dark"),
        ("settings-pages", "setting:appearance.high_contrast"),
    ),
}

# Chrome only: window contents scroll out of view by design.
NOT_CHROME = re.compile(r"GroupRow|foreign-surface|^tab:|tabbar_new_tab|TileHost|WindowGroup")
CONTAINER = re.compile(r"(Host|Container|Layer|Root)(\[\d+\])?$")


def set_theme(ctx, name: str) -> None:
    """Select the theme through shell actions, which persist it and repaint chrome."""
    for provider, action in THEMES[name]:
        ctx.vm.invoke(provider, action)
    time.sleep(2.5)


def layout_problems(targets: list[dict], displays: dict) -> dict:
    tmap = {}
    for t in targets:
        tmap.setdefault(tuple(t["rect"]), t["name"])  # one name per rect: indexed aliases collapse
    rects = [(n, r) for r, n in tmap.items() if r[2] > 0 and r[3] > 0 and not NOT_CHROME.search(n)]
    desk = (min(d[0] for d in displays.values()), min(d[1] for d in displays.values()),
            max(d[0] + d[2] for d in displays.values()), max(d[1] + d[3] for d in displays.values()))
    desk_rect = (desk[0], desk[1], desk[2] - desk[0], desk[3] - desk[1])
    outside, overlaps = [], []
    for n, r in rects:
        if core.rect_inside(r, desk_rect) and (r[2] >= desk_rect[2] - 1 or r[3] >= desk_rect[3] - 1):
            continue  # a desktop-wide container
        # Wholly off screen is scrolled away (a panned strip), not clipped.
        visible = any(core.overlap(r, d) > 0 for d in displays.values())
        if visible and not any(core.rect_inside(r, d, slack=1.5) for d in displays.values()):
            outside.append((n, r))
    names = {n for n, _ in rects}
    leaves = [(n, r) for n, r in rects
              if not any(o != n and o.startswith(n + "/") for o in names)]
    by_parent: dict = {}
    for n, r in leaves:
        by_parent.setdefault(n.rsplit("/", 1)[0] if "/" in n else "", []).append((n, r))
    for parent, kids in by_parent.items():
        # A host or container spans its children; it is not a control they can collide with.
        kids = [(n, r) for n, r in kids if not CONTAINER.search(n.rsplit("/", 1)[-1])]
        for i, (a, ra) in enumerate(kids):
            for b, rb in kids[i + 1:]:
                if core.rect_inside(ra, rb) or core.rect_inside(rb, ra):
                    continue  # a hit area around its own visual, not two controls colliding
                ov = core.overlap(ra, rb)
                small = min(ra[2] * ra[3], rb[2] * rb[3])
                if small and ov / small > 0.25:
                    overlaps.append((a, b, round(ov / small, 2)))
    return {"outside_output": outside, "overlapping_siblings": overlaps, "chrome_targets": len(rects)}


SURFACES = [
    ("desktop", None, None),
    ("quick-settings", ("actions", "quick-settings.open"), "quick_settings"),
    ("workspace-settings", ("actions", "settings.open"), "ws_settings"),
    ("overview", ("actions", "overview.toggle"), "overview"),
]


def _surface_pass(ctx, res, label):
    """Shoot each surface and check its chrome layout; return per-surface findings."""
    findings = {}
    for name, action, overlay in SURFACES:
        ctx.close_overlays()
        if action:
            ctx.vm.invoke(*action)
            try:
                ctx.wait_state(lambda s: core.overlays(s).get(overlay), 5, f"{name} open")
            except CheckFailed as exc:
                findings[name] = {"error": str(exc)}
                continue
            time.sleep(0.8)
        st = ctx.state()
        shot = ctx.shot(f"{label}-{name}")
        lp = layout_problems(ctx.vm.targets(), core.display_rects(st))
        img = core.load_image(shot)
        d = core.display_rects(st)[0]
        lp["topbar_pixels"] = core.region_stats(img, (d[0], d[1], d[2], 48))
        lp["centre_pixels"] = core.region_stats(img, (d[0] + d[2] * 0.3, d[1] + d[3] * 0.3,
                                                      d[2] * 0.4, d[3] * 0.4))
        findings[name] = lp
    ctx.close_overlays()
    return findings


def theme_contrast_problems(lum: dict) -> list[str]:
    """Light Quick Settings must read clearly lighter than dark ones. A pass that
    measured neither cannot say whether the theme reached the chrome."""
    missing = [t for t in ("light", "dark") if lum.get(t) is None]
    if missing:
        return [f"quick settings luminance not measured in {' and '.join(missing)}: "
                "cannot tell whether the theme change reached the chrome"]
    if lum["light"] <= lum["dark"] + 0.15:
        return [f"light and dark quick settings look alike (luminance {lum['light']} vs {lum['dark']}): "
                "the theme change did not reach the chrome"]
    return []


def _matrix(ctx, res, locale):
    ctx.reset()
    try:
        ctx.open_app("so.tilo.settings")
    except CheckFailed as exc:
        res.detail = f"Settings did not open ({exc}); chrome checked without it"
    time.sleep(2)
    per_theme, problems = {}, []
    for theme in THEMES:
        set_theme(ctx, theme)
        f = _surface_pass(ctx, res, f"U-{locale}-{theme}")
        per_theme[theme] = f
        for surface, lp in f.items():
            if "error" in lp:
                problems.append(f"{theme}/{surface}: {lp['error']}")
                continue
            if lp["outside_output"]:
                problems.append(f"{theme}/{surface}: {len(lp['outside_output'])} chrome targets cross an "
                                f"output edge, e.g. {lp['outside_output'][0]}")
            if lp["overlapping_siblings"]:
                problems.append(f"{theme}/{surface}: overlapping controls {lp['overlapping_siblings'][:3]}")
    set_theme(ctx, "dark")
    lum = {t: per_theme[t]["quick-settings"].get("centre_pixels", {}).get("mean")
           for t in THEMES if "centre_pixels" in per_theme[t].get("quick-settings", {})}
    res.metrics["quick_settings_centre_luminance"] = lum
    problems += theme_contrast_problems(lum)
    ctx.write(f"U-{locale}-layout.json", json.dumps(per_theme, indent=1, default=str))
    res.metrics["surfaces"] = {t: {s: {k: (len(v) if isinstance(v, list) else v)
                                       for k, v in lp.items() if k != "centre_pixels"}
                                   for s, lp in f.items()} for t, f in per_theme.items()}
    expect(not problems, "; ".join(problems[:8]) + (f" (+{len(problems) - 8} more)" if len(problems) > 8 else ""))


@check("U01", AREA, "Theme and language can be changed through actions (not only by clicking)")
def semantic(ctx, res):
    found = {}
    for q in ("dark theme", "light theme", "high contrast", "language", "Sprache"):
        found[q] = [(a["provider"], a["id"]) for a in ctx.vm.actions(q)
                    if re.search(r"theme|contrast|appearance|language|locale", a["id"] + a["title"], re.I)
                    and a["provider"] != "applications"][:3]
    res.metrics["found"] = found
    missing = [q for q, v in found.items() if not v and q != "Sprache"]
    expect(not missing, f"no action sets: {missing} (Settings exports none; Theme Studio only opens)")


@check("U02", AREA, "Chrome, quick settings, workspace settings and Settings in light/dark/high-contrast (English)")
def matrix_en(ctx, res):
    _matrix(ctx, res, "en")


@check("U03", AREA, "The same surfaces in German", needs=("locale_de",))
def matrix_de(ctx, res):
    _matrix(ctx, res, "de")


@check("U04", AREA, "German session: shell strings and action titles are German", needs=("locale_de",))
def german_strings(ctx, res):
    titles = {a["id"]: a["title"] for a in ctx.vm.actions()}
    sample = {k: titles.get(k) for k in ("overview.toggle", "settings.open", "quick-settings.open")}
    res.metrics["titles"] = sample
    english = {"Show workspace overview", "Open workspace settings", "Open quick settings"}
    still = [v for v in sample.values() if v in english]
    expect(all(sample.values()), f"actions missing: {sample}")
    expect(not still, f"English titles in a German session: {still}")
