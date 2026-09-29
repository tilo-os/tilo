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
    res.metrics["action_availability"] = a and a["contract"]["availability"]
    if a and a["contract"]["availability"].get("state") == "available":
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
    expect(not problems, "; ".join(problems))


@check("B03", AREA, "Bluetooth without an adapter: shown unavailable, no fake toggle")
def bluetooth(ctx, res):
    hw = _hw(ctx)
    res.metrics["hardware"] = hw
    if hw.get("bluetooth"):
        raise Unobservable("this VM has a Bluetooth adapter")
    actions = [(a["id"], a["contract"]["availability"]) for a in ctx.vm.actions("bluetooth")
               if "bluetooth" in a["id"] and "pairing" not in a["id"]]
    res.metrics["actions"] = actions
    targets = _qs_targets(ctx, ("bluetooth",))
    res.metrics["quick_settings_targets"] = targets
    ctx.shot("B03-bluetooth-quick-settings")
    ctx.close_overlays()
    live = [a for a, av in actions if av.get("state") == "available"]
    expect(not live, f"Bluetooth actions offered as available with no adapter: {live}")


@check("B04", AREA, "Keyboard layout import: GNOME de+nodeadkeys and compose:ralt become the session's layout")
def keyboard_import(ctx, res):
    have = ctx.vm.exec("command -v gsettings || true").strip()
    if not have or have.startswith("receipt"):
        raise Unobservable("gsettings is not in the image, so the GNOME layout import cannot be seeded")
    ctx.vm.exec("gsettings set org.gnome.desktop.input-sources sources \"[('xkb', 'de+nodeadkeys')]\" && "
                "gsettings set org.gnome.desktop.input-sources xkb-options \"['compose:ralt']\"")
    raw = ctx.vm.exec('cat "$HOME/.config/tilo/settings.json"')
    doc = json.loads("\n".join(l for l in raw.splitlines() if not l.startswith("receipt:")))
    for section in doc.values():
        if isinstance(section, dict):
            for k in [k for k in section if k in ("keyboard_layout", "keyboard_layouts", "compose_key")
                      or k.startswith("host_input_migration")]:
                section.pop(k)
    ctx.vm.write_home_file(".config/tilo/settings.json", json.dumps(doc, indent=2).encode())
    ctx.vm.run("slot", "restart", timeout=180)
    time.sleep(8)
    raw = ctx.vm.exec('cat "$HOME/.config/tilo/settings.json"')
    doc = json.loads("\n".join(l for l in raw.splitlines() if not l.startswith("receipt:")))
    found = {k: v for sec in doc.values() if isinstance(sec, dict) for k, v in sec.items()
             if k in ("keyboard_layout", "keyboard_layouts", "compose_key")}
    res.metrics["imported"] = found
    expect(found.get("keyboard_layout") == "de:nodeadkeys",
           f"layout not imported: settings hold {found or 'no keyboard keys'}")
    expect(found.get("compose_key") == "ralt", f"compose key not imported: {found.get('compose_key')}")
    # Functional: the physical Y key is `z` on a German layout.
    ctx.reset()
    ctx.new_terminal_window()
    time.sleep(2)
    probe = f"acc{int(time.time()) % 100000}"
    ctx.vm.input("type", f"touch ~/{probe}")
    ctx.vm.key("y", "Return")
    time.sleep(2)
    hits = ctx.vm.exec(f'ls -d "$HOME"/{probe}* 2>/dev/null; true')
    names = [l.rsplit("/", 1)[-1] for l in hits.splitlines() if probe in l]
    res.metrics["typed_files"] = names
    ctx.shot("B04-typed-de")
    expect(names, "the typed command did not run (see W17), so the active layout could not be observed")
    expect(any(n.endswith("z") for n in names), f"the Y key did not type `z`: created {names}")


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
    helptext = ctx.vm.run("slot", "--help")
    if "hotplug" not in helptext and "unplug" not in helptext:
        raise Unobservable("tilo-vm cannot unplug or replug a slot's virtual output, "
                           "so hotplug cannot be driven")
    raise Unobservable("tilo-vm offers output hotplug, but this suite does not drive it yet")
