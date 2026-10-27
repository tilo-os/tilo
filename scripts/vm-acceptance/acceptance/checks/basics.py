"""Essential 6: brightness, Wi-Fi and Bluetooth without hardware, keyboard
layout import, display arrangement and hotplug."""

from __future__ import annotations

import json
import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError

AREA = "basics"


def _hw(ctx) -> dict:
    out = ctx.vm.exec("echo backlight=$(ls /sys/class/backlight 2>/dev/null | wc -l); "
                      "echo wifi=$(ls -d /sys/class/net/*/wireless 2>/dev/null | wc -l); "
                      "echo bluetooth=$(ls /sys/class/bluetooth 2>/dev/null | wc -l)")
    return {k: int(v) for k, _, v in (l.partition("=") for l in out.splitlines()) if v.strip().isdigit()}


def _named(action_id: str, names) -> bool:
    """Whether an action's address ends in one of `names` (an app's action
    address is its provider, resource and action joined by \\x1f)."""
    return action_id.rsplit("\x1f", 1)[-1] in names


# What a listed action without a contract is taken to be: it declares no availability, so
# nothing tells a caller it does not work, and it counts as offered.
UNDECLARED = {"state": "available", "reason": "listed without a contract: no availability declared"}


def _availability(action: dict) -> dict:
    """An action row's declared availability. The listing carries rows without a contract
    (`contract: null`), and those never say they are unavailable."""
    return (action.get("contract") or {}).get("availability") or UNDECLARED


# The provider of Settings' pages: its `page:<slug>` rows open Settings at that page.
SETTINGS_PAGES = "settings-pages"


def _shows_only(action: dict) -> bool:
    """Whether a row only changes what is shown, so it is no toggle of the hardware: a view
    or navigation (`operation_kind` View), an action whose contract only reads, or a row
    that opens a Settings page (`page:bluetooth`, which launches Settings there and
    declares no contract)."""
    if str(action.get("operation_kind", "")).lower() == "view":
        return True
    if (action.get("contract") or {}).get("kind") == "read":
        return True
    return action.get("provider") == SETTINGS_PAGES and str(action.get("id", "")).startswith("page:")


def _offered(ctx, names) -> list:
    """Every published action named one of `names`, with its availability."""
    return [(a["id"], _availability(a)) for a in ctx.vm.actions() if _named(a["id"], names)]


def _action(ctx, query, action_id):
    return next((a for a in ctx.vm.actions(query) if a["id"] == action_id), None)


def _qs_targets(ctx, words):
    ctx.close_overlays()
    ctx.vm.invoke("actions", "quick-settings.open")
    ctx.wait_state(lambda s: core.overlays(s)["quick_settings"], 5, "quick settings open")
    time.sleep(0.8)
    names = [t["name"] for t in ctx.vm.targets()]
    return [n for n in names if any(w in n.lower() for w in words)]


def _no_hardware_control(ctx, res, kind, query, action_id, args, words):
    hw = _hw(ctx)
    res.metrics["hardware"] = hw
    if hw.get(kind):
        raise Unobservable(f"this VM has {kind} hardware; the no-hardware case cannot be observed")
    problems = []
    a = _action(ctx, query, action_id)
    res.metrics["action_availability"] = a and _availability(a)
    if a and _availability(a).get("state") == "available":
        try:
            out = ctx.vm.invoke("actions", action_id, args)
            state = out.get("receipt", {}).get("update", {}).get("state")
            res.metrics["invoke_result"] = out.get("receipt")
            if state == "succeeded":
                problems.append(f"`{action_id}` is offered as available and reports success with no {kind} device")
        except VmError as exc:
            res.metrics["invoke_refusal"] = str(exc)[:300]
            problems.append(f"`{action_id}` is advertised available but fails when invoked; "
                            f"it should be unavailable with a reason")
    targets = _qs_targets(ctx, words)
    res.metrics["quick_settings_targets"] = targets
    ctx.shot(f"B-{kind}-quick-settings")
    ctx.close_overlays()
    return problems, targets


@check("B01", AREA, "Brightness without a backlight: no working-looking slider, action unavailable")
def brightness(ctx, res):
    problems, targets = _no_hardware_control(ctx, res, "backlight", "brightness",
                                             "quick-settings.brightness.set", {"percent": 40},
                                             ("bright",))
    if targets:
        problems.append(f"quick settings shows a brightness control with no backlight: {targets[:3]}")
    expect(not problems, "; ".join(problems))


@check("B02", AREA, "Wi-Fi without an adapter: shown unavailable, the toggle does nothing fake")
def wifi(ctx, res):
    problems, targets = _no_hardware_control(ctx, res, "wifi", "wifi",
                                             "quick-settings.wifi.set-enabled", {"enabled": True},
                                             ("wifi", "wi-fi", "wlan"))
    # Settings switches the same radio; it must not offer what Quick Settings withholds.
    settings = _offered(ctx, ("set-wifi", "set-hotspot", "start-hotspot"))
    res.metrics["settings_actions"] = settings
    live = [a for a, av in settings if av.get("state") == "available"]
    if live:
        problems.append(f"Wi-Fi actions offered as available with no adapter: {live}")
    expect(not problems, "; ".join(problems))


@check("B03", AREA, "Bluetooth without an adapter: shown unavailable, no fake toggle")
def bluetooth(ctx, res):
    hw = _hw(ctx)
    res.metrics["hardware"] = hw
    if hw.get("bluetooth"):
        raise Unobservable("this VM has a Bluetooth adapter")
    # Settings' radio effects by name (`set-discovering` does not say
    # "bluetooth"), and every other Bluetooth action but the pairing prompts.
    # Rows that only show something (the Bluetooth settings page) are no toggle.
    actions = [(a["id"], _availability(a)) for a in ctx.vm.actions()
               if not _shows_only(a)
               and (("bluetooth" in a["id"] and "pairing" not in a["id"])
                    or _named(a["id"], ("set-bluetooth", "set-discovering", "refresh-bluetooth")))]
    res.metrics["actions"] = actions
    targets = _qs_targets(ctx, ("bluetooth",))
    res.metrics["quick_settings_targets"] = targets
    ctx.shot("B03-bluetooth-quick-settings")
    ctx.close_overlays()
    live = [a for a, av in actions if av.get("state") == "available"]
    expect(not live, f"Bluetooth actions offered as available with no adapter: {live}")


def _settings(ctx) -> dict:
    raw = ctx.vm.exec('cat "$HOME/.config/tilo/session/settings.json" 2>/dev/null || echo "{}"')
    return json.loads("\n".join(l for l in raw.splitlines() if not l.startswith("receipt:")) or "{}")


def _typed_in_search(ctx, key: str) -> str:
    """The text a physical key press puts into the search field (read back from state-dump)."""
    ctx.close_overlays()
    ctx.vm.key("super+space")
    ctx.wait_state(lambda s: (s["chrome"].get("palette") or {}).get("visible"), 5, "search open")
    ctx.vm.key(key)
    ctx.settle(0.8)
    query = (ctx.state()["chrome"].get("palette") or {}).get("query") or ""
    ctx.close_overlays()
    return query


@check("B04", AREA, "Keyboard layout: de nodeadkeys with a compose key applies to the physical keys")
def keyboard_layout(ctx, res):
    ctx.reset()
    doc = _settings(ctx)
    saved = dict(doc.get("input") or {})
    res.metrics["us_y"] = _typed_in_search(ctx, "y")
    doc.setdefault("input", {}).update(keyboard_layout="de:nodeadkeys",
                                       keyboard_layouts=["de:nodeadkeys"], compose_key="ralt")
    ctx.vm.write_home_file(".config/tilo/session/settings.json", json.dumps(doc, indent=2).encode())
    time.sleep(2.5)
    try:
        # The physical Y key is `z` on a German layout; a US one types `y`.
        typed = _typed_in_search(ctx, "y")
        res.metrics["de_y"] = typed
        ctx.shot("B04-layout-de")
    finally:
        doc = _settings(ctx)
        doc["input"] = saved
        ctx.vm.write_home_file(".config/tilo/session/settings.json", json.dumps(doc, indent=2).encode())
        time.sleep(2.5)
    expect(res.metrics["us_y"] == "y", f"with the default layout the Y key typed {res.metrics['us_y']!r}")
    expect(typed == "z", f"with de:nodeadkeys the physical Y key typed {typed!r}, not 'z'")


# The GNOME keyboard import is not checked here: the session imports the host's GNOME
# input settings only in host-desktop mode (`--host-desktop-session`, which only the
# packaged tilo-session-runner passes), so a Tilo OS session never runs it. The
# host-desktop VM test (dist/session/vm-e2e, H05) seeds GNOME de+nodeadkeys and
# compose:ralt and requires the session to import them.


@check("B05", AREA, "Display arrangement: mirror and extend apply, and revert restores")
def arrangement(ctx, res):
    ctx.reset()
    st = ctx.state()
    before = core.display_rects(st)
    expect(len(before) >= 2, "needs two outputs")
    ctx.vm.invoke("actions", "display.layout.mirror")
    try:
        st = ctx.wait_state(lambda s: len({r[:2] for r in core.display_rects(s).values()}) == 1, 10,
                            "both outputs at the same origin (mirrored)")
    finally:
        ctx.shot("B05-mirror")
    res.metrics["mirrored"] = core.display_rects(st)
    ctx.vm.invoke("actions", "display.change.revert")
    st = ctx.wait_state(lambda s: core.display_rects(s) == before, 10, "the extended layout restored by revert")
    res.metrics["restored"] = core.display_rects(st)


@check("B06", AREA, "Monitor hotplug: unplugging and replugging an output moves windows and restores the layout")
def hotplug(ctx, res):
    # The guest kernel's DRM connector status is the hotplug: forcing it off and back to
    # detect sends the same uevent a real cable does, so the compositor's own path runs.
    ctx.reset()
    st = ctx.state()
    rects = core.display_rects(st)
    expect(len(rects) >= 2, f"two outputs present (have {len(rects)})")
    g = ctx.new_terminal_window()["id"]
    st = ctx.state()
    # Unplug whichever output the new window landed on.
    d1 = core.display_of_group(st, g)
    expect(d1 is not None, "the new window is on an output")
    monitor = next((d.get("monitor", "") for d in st["workspace_tree"]["outputs"]["displays"]
                    if d["id"] == d1), "")
    name = monitor.removeprefix("connector:")
    if not name or name == monitor:
        raise Unobservable(f"display {d1} names no DRM connector (monitor={monitor!r})")
    found = ctx.vm.root_exec(f"ls -d /sys/class/drm/card*-{name} 2>/dev/null").split()
    if len(found) != 1:
        raise Unobservable(f"connector {name} of display {d1} is not one DRM connector: {found}")
    victim = found[0]
    res.metrics["unplugged"] = victim

    card = victim.rsplit("/", 1)[1].split("-", 1)[0]
    widths = {d: r[2:4] for d, r in rects.items() if d != d1}

    def force(state):
        # Forcing a connector's status sends no uevent; the card's synthetic change
        # event is the hotplug notification compositors listen for.
        ctx.vm.vm_cmd("ssh", "--", f"sh -c 'echo {state} > {victim}/status && "
                      f"echo change > /sys/class/drm/{card}/uevent'", timeout=60)

    force("off")
    try:
        st = ctx.wait_state(lambda s: len(core.display_rects(s)) == len(rects) - 1, 10,
                            "the unplugged output is gone from the layout")
        ctx.shot("B06-unplugged")
        kept = {d: r[2:4] for d, r in core.display_rects(st).items()}
        res.metrics["remaining_sizes"] = kept
        expect(all(kept.get(d) == wh for d, wh in widths.items()),
               f"remaining outputs keep their modes: had {widths}, now {kept}")
        ctx.wait_state(lambda s: core.display_of_group(s, g) not in (None, d1), 5,
                       "the window moved to a remaining output")
    finally:
        force("detect")
    ctx.wait_state(lambda s: len(core.display_rects(s)) == len(rects), 10,
                   "the replugged output is back in the layout")
    st = ctx.wait_state(lambda s: core.display_of_group(s, g) == d1, 5,
                        f"the window returned to the replugged output {d1}")
    res.metrics["restored_rects"] = core.display_rects(st)
    ctx.shot("B06-replugged")
