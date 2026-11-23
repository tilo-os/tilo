"""Essential 6: brightness, Wi-Fi and Bluetooth without hardware, keyboard
layout import, display modes and hotplug."""

from __future__ import annotations

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, address, ended, wait_until

AREA = "basics"


def _hw(ctx) -> dict:
    out = ctx.vm.exec("echo backlight=$(ls /sys/class/backlight 2>/dev/null | wc -l); "
                      "echo wifi=$(ls -d /sys/class/net/*/wireless 2>/dev/null | wc -l); "
                      "echo bluetooth=$(ls /sys/class/bluetooth 2>/dev/null | wc -l)")
    return {k: int(v) for k, _, v in (l.partition("=") for l in out.splitlines()) if v.strip().isdigit()}


def _named(action: dict, names) -> bool:
    """Whether a listed row's action is named one of `names`."""
    return address(action)[1] in names


def _id(action: dict) -> str:
    """A listed row's provider, resource and action, as the report names it."""
    return "#".join((action["provider"], *address(action)))


# What a listed action without a contract is taken to be: it declares no availability, so
# nothing tells a caller it does not work, and it counts as offered.
UNDECLARED = {"state": "available", "reason": "listed without a contract: no availability declared"}


def _availability(action: dict) -> dict:
    """An action row's declared availability. The listing carries rows without a contract
    (`contract: null`), and those never say they are unavailable."""
    return (action["action"].get("contract") or {}).get("availability") or UNDECLARED


def _shows_only(action: dict) -> bool:
    """Whether a row only changes what is shown, so it is no toggle of the hardware: a view
    or navigation (its kind is View), or an action whose contract only reads."""
    if str(action["action"].get("kind", "")).lower() == "view":
        return True
    return (action["action"].get("contract") or {}).get("kind") == "read"


def _offered(ctx, names) -> list:
    """Every published action named one of `names`, with its availability."""
    return [(_id(a), _availability(a)) for a in ctx.vm.actions() if _named(a, names)]


def _action(ctx, query, action):
    """The listed action named `action`: a Quick Settings effect (`set-wifi`), which this
    device's catalogue of private windows lists by its window, record and name."""
    return next((a for a in ctx.vm.actions(query) if _named(a, (action,))), None)


def _qs_targets(ctx, words):
    ctx.close_overlays()
    ctx.vm.invoke(core.SESSION, *core.CHROME_QUICK_SETTINGS, {})
    ctx.wait_state(lambda s: core.overlays(s).get("quick-settings") is True, 5, "quick settings open")
    ctx.wait_still()
    names = [t["name"] for t in ctx.vm.targets()]
    return [n for n in names if any(w in n.lower() for w in words)]


def _no_hardware_control(ctx, res, kind, query, action, args, words):
    hw = _hw(ctx)
    res.metrics["hardware"] = hw
    if hw.get(kind):
        raise Unobservable(f"this VM has {kind} hardware; the no-hardware case cannot be observed")
    problems = []
    a = _action(ctx, query, action)
    res.metrics["action_availability"] = a and _availability(a)
    if a and _availability(a).get("state") == "available":
        action_id = _id(a)
        try:
            status = ctx.vm.invoke_row(a, args).status
            res.metrics["invoke_result"] = status
            if ended(status) and "Completed" in status:
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
                                             "set-brightness", {"level": 0.4},
                                             ("bright",))
    if targets:
        problems.append(f"quick settings shows a brightness control with no backlight: {targets[:3]}")
    expect(not problems, "; ".join(problems))


@check("B02", AREA, "Wi-Fi without an adapter: shown unavailable, the toggle does nothing fake")
def wifi(ctx, res):
    problems, targets = _no_hardware_control(ctx, res, "wifi", "wifi",
                                             "set-wifi", {"on": True},
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
    actions = [(_id(a), _availability(a)) for a in ctx.vm.actions()
               if not _shows_only(a)
               and (("bluetooth" in _id(a) and "pairing" not in _id(a))
                    or _named(a, ("set-bluetooth", "set-discovering", "refresh-bluetooth")))]
    res.metrics["actions"] = actions
    targets = _qs_targets(ctx, ("bluetooth",))
    res.metrics["quick_settings_targets"] = targets
    ctx.shot("B03-bluetooth-quick-settings")
    ctx.close_overlays()
    live = [a for a, av in actions if av.get("state") == "available"]
    expect(not live, f"Bluetooth actions offered as available with no adapter: {live}")


OUTPUT = "settings/input"
LAYOUTS = core.action(OUTPUT, "set-layouts")


def set_layouts(ctx, layouts: list[str], active: str) -> None:
    """Type with `layouts`, switching between them in that order, `active` in use."""
    ctx.vm.invoke(core.SESSION, *LAYOUTS, {"layouts": layouts, "active": active})


def search_query(ctx) -> str:
    return ctx.search().get("query") or ""


def _typed_in_search(ctx, key: str) -> str:
    """The text a physical key press puts into the search field (read back from the search)."""
    ctx.close_overlays()
    ctx.vm.key("super+space")
    ctx.wait_state(lambda s: core.overlays(s).get("search") is True, 5, "search open")
    ctx.vm.key(key)
    query = wait_until(lambda: search_query(ctx), 3, 0.2) or ""
    ctx.close_overlays()
    return query


@check("B04", AREA, "Keyboard layout: de nodeadkeys applies to the physical keys")
def keyboard_layout(ctx, res):
    ctx.reset()
    res.metrics["us_y"] = _typed_in_search(ctx, "y")
    set_layouts(ctx, ["de:nodeadkeys"], "de:nodeadkeys")
    typed = None
    try:
        # The physical Y key is `z` on a German layout; a US one types `y`.
        typed = wait_until(
            lambda: (text := _typed_in_search(ctx, "y")) == "z" and text, 20, 1.0) or _typed_in_search(ctx, "y")
        res.metrics["de_y"] = typed
        ctx.shot("B04-layout-de")
    finally:
        set_layouts(ctx, ["us"], "us")
    expect(res.metrics["us_y"] == "y", f"with the default layout the Y key typed {res.metrics['us_y']!r}")
    expect(typed == "z", f"with de:nodeadkeys the physical Y key typed {typed!r}, not 'z'")


# The GNOME keyboard import is not checked here: the session imports the host's GNOME
# input settings only in host-desktop mode (`--host-desktop`, which only the
# packaged tilo-session-runner passes), so a Tilo OS session never runs it. The
# host-desktop VM test (dist/session/vm-e2e, H05) seeds GNOME de+nodeadkeys and
# compose:ralt and requires the session to import them.


@check("B05", AREA, "Display modes: mirror and extend apply, and extending restores the layout")
def display_modes(ctx, res):
    ctx.reset()
    st = ctx.state()
    before = core.display_rects(st)
    expect(len(before) >= 2, "needs two outputs")
    ctx.vm.invoke(core.SESSION, *core.DISPLAYS_SET_MODE, {"mode": "mirror"})
    try:
        st = ctx.wait_state(lambda s: len({r[:2] for r in core.display_rects(s).values()}) == 1, 10,
                            "both outputs at the same origin (mirrored)")
    finally:
        ctx.shot("B05-mirror")
    res.metrics["mirrored"] = core.display_rects(st)
    ctx.vm.invoke(core.SESSION, *core.DISPLAYS_SET_MODE, {"mode": "extend"})
    st = ctx.wait_state(lambda s: core.display_rects(s) == before, 10,
                        "the extended layout restored")
    res.metrics["restored"] = core.display_rects(st)


def _sizes_by_monitor(state: dict) -> dict:
    """Each output's size, by the monitor it shows."""
    return {d["monitor"]: (d["width"], d["height"])
            for d in state["workspace_tree"]["outputs"]["displays"]}


def _monitor_of_group(state: dict, gid):
    """The monitor showing window group `gid`, when an output shows that monitor now."""
    shown = {d["monitor"] for d in state["workspace_tree"]["outputs"]["displays"]}
    monitor = (state["workspace_tree"]["outputs"].get("home") or {}).get(str(gid))
    return monitor if monitor in shown else None


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
    # The partition names a display by its monitor (an EDID identity, or the connector for a
    # sink without one); the connector that can be forced is what the compositor published
    # for that monitor.
    by_monitor = core.connectors_by_monitor(core.display_snapshot(ctx))
    name = by_monitor.get(monitor)
    if not name:
        raise Unobservable(f"display {d1} names monitor {monitor!r}, which the compositor's "
                           f"published displays do not name ({sorted(by_monitor)})")
    found = ctx.vm.root_exec(f"ls -d /sys/class/drm/card*-{name} 2>/dev/null").split()
    if len(found) != 1:
        raise Unobservable(f"connector {name} of display {d1} is not one DRM connector: {found}")
    victim = found[0]
    res.metrics["unplugged"] = victim

    card = victim.rsplit("/", 1)[1].split("-", 1)[0]
    # Outputs are told apart by their monitor: a display's `id` is the backend's output
    # number, renumbered whenever the outputs are enumerated again, as a hotplug does.
    sizes = _sizes_by_monitor(st)
    widths = {m: wh for m, wh in sizes.items() if m != monitor}

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
        kept = _sizes_by_monitor(st)
        res.metrics["remaining_sizes"] = kept
        expect(all(kept.get(m) == wh for m, wh in widths.items()),
               f"remaining outputs keep their modes: had {widths}, now {kept}")
        ctx.wait_state(lambda s: _monitor_of_group(s, g) in widths, 5,
                       "the window moved to a remaining output")
    finally:
        force("detect")
    ctx.wait_state(lambda s: len(core.display_rects(s)) == len(rects), 10,
                   "the replugged output is back in the layout")
    st = ctx.wait_state(lambda s: _monitor_of_group(s, g) == monitor, 5,
                        f"the window returned to the replugged output ({monitor})")
    res.metrics["restored_rects"] = core.display_rects(st)
    ctx.shot("B06-replugged")
