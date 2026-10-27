"""Essential 8: input fidelity, through uinput devices that copy real hardware.

The suite's default input devices are `BUS_VIRTUAL` "tilo-vm keyboard (seat)": udev's hwdb and
libinput's quirks, keyed on bus, vendor, product and name, never see a laptop. Here the guest
kernel registers the devices of a Lenovo Yoga Slim 7x (83ED), copied from the real ones
(`os/hardware/profiles/lenovo-83ed/input/*.json`: bus I2C, 048D:8987 keyboard, 0488:1054
precision touchpad with its 130x76 mm clickpad axes), and the checks drive them with
`tilo-vm input device plug`, `input key`/`hold`/`release` and `input touchpad ...`.

What this cannot reproduce is the kernel driver: hwdb `KEYBOARD_KEY_<scan>` remaps (the
Copilot and Fn-row keys) apply through the driver's keymap, which a uinput device lacks.
Those need the real machine (the hardware-in-the-loop lane).

I03 and I04 are about state that must survive an event (a device appearing, a lock): their
subject is the compositor's keyboard state, observed by what the next keystroke types.
I05-I07 observe the touchpad through libinput's own decisions (tap, scroll, disable-while-
typing); they are `calibrating` until a CI run has confirmed each observable (see
core.check), and say so in the report while they are.
"""

from __future__ import annotations

import contextlib
import json
import re
import time

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, wait_until
from .basics import _settings
from .window import _app_groups, _ensure_unlocked, _group_rect, _locked, _two_windows

AREA = "input"

KEYBOARD = "lenovo-83ed-keyboard"
TOUCHPAD = "lenovo-83ed-touchpad"
# The real device's identity: /proc/bus/input/devices on the 83ED, bus 0018 is I2C.
KEYBOARD_ID = {"bus": 0x18, "vendor": 0x048D, "product": 0x8987, "name": "hid-over-i2c 048D:8987"}
TOUCHPAD_ID = {"bus": 0x18, "vendor": 0x0488, "product": 0x1054, "name": "hid-over-i2c 0488:1054 Touchpad"}
# udev derives these from the axes: the real pad reports the same.
TOUCHPAD_SIZE_MM = (130, 76)

CALIBRATING = ("the observable was chosen from the compositor's code and has not yet been seen to work "
               "in a CI run")
PASSCODE = "input7"


# -- plugging ----------------------------------------------------------------------------------

def plug(ctx, *profiles: str) -> None:
    for name in profiles:
        ctx.vm.input("device", "plug", name)


def unplug_all(ctx) -> None:
    """Take every profile device out again, whatever state the check ended in: the keyboard
    profile would otherwise take over `input key` for every later check."""
    for name in (KEYBOARD, TOUCHPAD):
        with contextlib.suppress(VmError):
            ctx.vm.input("device", "unplug", name)


@contextlib.contextmanager
def plugged(ctx, *profiles: str):
    unplug_all(ctx)  # a failed earlier check may have left one
    try:
        plug(ctx, *profiles)
        yield
    finally:
        unplug_all(ctx)


@contextlib.contextmanager
def layouts(ctx, layout: str = "de:nodeadkeys", *more: str):
    """The session's keyboard layout set to `layout` (and `more`, in order) for the block."""
    doc = _settings(ctx)
    saved = dict(doc.get("input") or {})

    def write(document):
        ctx.vm.write_home_file(".config/tilo/session/settings.json", json.dumps(document, indent=2).encode())
        time.sleep(2.5)

    doc.setdefault("input", {}).update(keyboard_layout=layout, keyboard_layouts=[layout, *more])
    write(doc)
    try:
        yield
    finally:
        doc = _settings(ctx)
        doc["input"] = saved
        write(doc)


def search_text(ctx, *combos: str) -> str:
    """What these physical key presses put into the search field, read back from the state dump."""
    ctx.close_overlays()
    ctx.vm.key("super+space")
    try:
        ctx.wait_state(lambda s: (s["chrome"].get("palette") or {}).get("visible"), 5, "search open")
    except CheckFailed as exc:
        raise CheckFailed(f"{exc}: {keyboard_diagnosis(ctx)}") from exc
    for combo in combos:
        ctx.vm.key(combo)
    ctx.settle(0.8)
    query = (ctx.state()["chrome"].get("palette") or {}).get("query") or ""
    ctx.close_overlays()
    return query


def keyboard_diagnosis(ctx) -> str:
    """Why Super+Space may not have reached the compositor: the keyboards it has opened (its
    published input snapshot, `$XDG_RUNTIME_DIR/tilo/input-state.json`), the one it last
    took keys from, and how udev tagged the profile keyboard. Kept as evidence too."""
    found = {}
    try:
        found["compositor"] = json.loads(ctx.vm.exec(
            'cat "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/tilo/input-state.json" 2>/dev/null || echo "{}"'))
    except (VmError, ValueError) as exc:
        found["compositor"] = {"unreadable": str(exc)[:200]}
    try:
        dev = find_device(parse_input_devices(ctx.vm.root_exec("cat /proc/bus/input/devices")), KEYBOARD_ID)
        node = dev and event_node(dev)
        found["udev"] = ({p: v for p, v in parse_properties(ctx.vm.root_exec(
            f"udevadm info -q property -n /dev/input/{node}")).items()
            if p.startswith(("ID_INPUT", "ID_SEAT", "ID_INTEGRATION", "LIBINPUT", "TAGS", "CURRENT_TAGS"))}
            if node else "the profile keyboard is not registered")
    except VmError as exc:
        found["udev"] = {"unreadable": exc.reason()}
    if ctx.current is not None:
        ctx.write(f"{ctx.current.id}-keyboards.json", json.dumps(found, indent=1))
    compositor = found["compositor"] if isinstance(found["compositor"], dict) else {}
    names = [d.get("name") for d in compositor.get("devices", []) if isinstance(d, dict)]
    seen = KEYBOARD_ID["name"] in names
    return (f"the compositor {'has' if seen else 'has not'} opened the profile keyboard "
            f"{KEYBOARD_ID['name']!r} (its devices: {names}; it last typed from "
            f"{compositor.get('active_keyboard')!r}); udev: {found['udev']}")


# -- what the guest kernel and udev say --------------------------------------------------------

def parse_input_devices(text: str) -> list[dict]:
    """`/proc/bus/input/devices` as one dict per device: bus, vendor, product, name, handlers, bits."""
    devices = []
    for block in re.split(r"\n\s*\n", text.strip()):
        dev: dict = {"bits": {}}
        for line in block.splitlines():
            if line.startswith("I: "):
                ident = dict(p.split("=") for p in line[3:].split())
                dev.update(bus=int(ident["Bus"], 16), vendor=int(ident["Vendor"], 16),
                           product=int(ident["Product"], 16), version=int(ident["Version"], 16))
            elif line.startswith("N: Name="):
                dev["name"] = line.split("=", 1)[1].strip().strip('"')
            elif line.startswith("H: Handlers="):
                dev["handlers"] = line.split("=", 1)[1].split()
            elif line.startswith("B: "):
                key, _, value = line[3:].partition("=")
                dev["bits"][key] = value.strip()
        if "name" in dev:
            devices.append(dev)
    return devices


def event_node(dev: dict) -> str | None:
    return next((h for h in dev.get("handlers", []) if re.fullmatch(r"event\d+", h)), None)


def parse_properties(text: str) -> dict:
    return dict(line.split("=", 1) for line in text.splitlines() if "=" in line)


def find_device(devices: list[dict], ident: dict) -> dict | None:
    return next((d for d in devices if all(d.get(k) == v for k, v in ident.items())), None)


@check("I01", AREA, "Device profiles: the guest registers the Yoga's keyboard and touchpad with their real "
                    "ids, and udev classifies them as a keyboard and a 130x76 mm touchpad")
def identity(ctx, res):
    with plugged(ctx, KEYBOARD, TOUCHPAD):
        devices = parse_input_devices(ctx.vm.root_exec("cat /proc/bus/input/devices"))
        res.metrics["registered"] = [(d["name"], f"{d['bus']:04x}:{d['vendor']:04x}:{d['product']:04x}")
                                     for d in devices]
        keyboard, touchpad = find_device(devices, KEYBOARD_ID), find_device(devices, TOUCHPAD_ID)
        expect(keyboard, f"no input device with the 83ED keyboard's identity {KEYBOARD_ID} in "
                         f"/proc/bus/input/devices ({[d['name'] for d in devices]})")
        expect(touchpad, f"no input device with the 83ED touchpad's identity {TOUCHPAD_ID}")
        # INPUT_PROP_POINTER | INPUT_PROP_BUTTONPAD, as on the machine: a clickpad.
        expect(touchpad["bits"].get("PROP") == "5", f"the touchpad is not a clickpad (PROP={touchpad['bits'].get('PROP')})")
        props = {}
        for label, dev in (("keyboard", keyboard), ("touchpad", touchpad)):
            node = event_node(dev)
            expect(node, f"the {label} has no event node ({dev.get('handlers')})")
            props[label] = parse_properties(ctx.vm.root_exec(f"udevadm info -q property -n /dev/input/{node}"))
        res.metrics["udev"] = {k: {p: v for p, v in d.items() if p.startswith("ID_INPUT")} for k, d in props.items()}
        expect(props["keyboard"].get("ID_INPUT_KEYBOARD") == "1", "udev does not classify the keyboard as a keyboard")
        expect(props["touchpad"].get("ID_INPUT_TOUCHPAD") == "1", "udev does not classify the touchpad as a touchpad")
        size = (props["touchpad"].get("ID_INPUT_WIDTH_MM"), props["touchpad"].get("ID_INPUT_HEIGHT_MM"))
        expect(size == tuple(str(v) for v in TOUCHPAD_SIZE_MM),
               f"udev sizes the touchpad {size} mm, the real one is {TOUCHPAD_SIZE_MM}")


@check("I02", AREA, "German layout end to end: the physical Y and Z keys swap, umlauts, sharp s and AltGr "
                    "reach the compositor as the right characters")
def german_keys(ctx, res):
    ctx.reset()
    with layouts(ctx), plugged(ctx, KEYBOARD):
        # Physical keys by their US names: Y is z, Z is y, `;` ö, `'` ä, `[` ü, `-` ß;
        # shifted `;` Ö; AltGr+Q is @.
        typed = search_text(ctx, "y", "z", "semicolon", "apostrophe", "leftbrace", "minus",
                            "shift+semicolon", "altgr+q")
        res.metrics["typed"] = typed
        ctx.shot("I02-layout-de")
        expect(typed == "zyöäüßÖ@", f"the physical keys y z ; ' [ - shift+; altgr+q typed {typed!r} on "
                                    "de:nodeadkeys, not 'zyöäüßÖ@'")


@check("I03", AREA, "The layout survives keyboard hotplug: unplug and replug the internal keyboard and a "
                    "second device, the German layout is still in effect")
def layout_survives_hotplug(ctx, res):
    ctx.reset()
    seen = {}
    with layouts(ctx, "de:nodeadkeys", "us"):
        with plugged(ctx, KEYBOARD):
            seen["first_plug"] = search_text(ctx, "y")
        # Keyboard gone: the default keyboard types on the same layout.
        seen["after_unplug_default_keyboard"] = search_text(ctx, "y")
        with plugged(ctx, KEYBOARD):
            seen["replug"] = search_text(ctx, "y")
            plug(ctx, TOUCHPAD)  # a second device appearing must not reset the keyboard state
            seen["with_touchpad_added"] = search_text(ctx, "y")
        unplug_all(ctx)
        seen["after_unplug_all"] = search_text(ctx, "y")
    res.metrics.update(seen)
    wrong = {k: v for k, v in seen.items() if v != "z"}
    expect(not wrong, f"the physical Y key typed something but 'z' after hotplug on de:nodeadkeys: {wrong}")


@check("I04", AREA, "No stuck modifiers after a lock: Shift, Ctrl, Alt or Super held into the lock and "
                    "released behind it do not stay down once unlocked")
def stuck_modifiers(ctx, res):
    ctx.reset()
    # The lock verifies the slot user's account password (see W21): give this throwaway VM's one.
    ctx.vm.root_exec(f'user=$(getent passwd {ctx.vm.slot_uid()} | cut -d: -f1) && '
                     f'echo "$user:{PASSCODE}" | chpasswd')
    held, result = [], {}
    try:
        with plugged(ctx, KEYBOARD):
            for modifier in ("shift", "ctrl", "alt", "super"):
                ctx.vm.input("hold", modifier)
                held.append(modifier)
                ctx.vm.invoke("session", "lock")
                ctx.wait_state(lambda s: s["chrome"].get("session_locked"), 10, f"locked with {modifier} down")
                ctx.vm.input("release", modifier)  # the key comes up behind the lock
                held.remove(modifier)
                ctx.settle(0.5)
                result[f"unlock_{modifier}"] = _ensure_unlocked(ctx, PASSCODE)
                expect(_locked(ctx) is False, f"could not unlock after {modifier} was held into the lock")
                # A stuck Shift types a capital, a stuck Ctrl/Alt/Super is a shortcut, not a letter.
                typed = search_text(ctx, "a")
                result[f"typed_after_{modifier}"] = typed
                expect(typed == "a", f"after a lock with {modifier} held and released behind it, the next "
                                     f"keystroke 'a' typed {typed!r}: {modifier} is stuck")
    finally:
        for modifier in held:
            with contextlib.suppress(VmError):
                ctx.vm.input("release", modifier)
        res.metrics.update(result)
        _ensure_unlocked(ctx, PASSCODE)


def _modality(st) -> str:
    return (st["chrome"].get("input") or {}).get("modality", "")


def _notes_groups(st) -> list:
    return _app_groups(st, "so.tilo.notes")


def _launchpad_card(ctx):
    t = core.target_map(ctx.vm.targets())
    card = t.get("launchpad:notes")
    if card is None:
        raise Unobservable("the empty workspace shows no launchpad:notes target "
                           f"(targets: {sorted(n for n in t if n.startswith('launchpad'))})")
    return int(card[0] + card[2] / 2), int(card[1] + card[3] / 2)


@check("I05", AREA, "Touchpad: a tap and a physical click press what the pointer is over (a launchpad card "
                    "opens its app)", calibrating=CALIBRATING)
def touchpad_tap_click(ctx, res):
    with plugged(ctx, TOUCHPAD):
        for how, args in (("tap", ("touchpad", "tap")), ("click", ("touchpad", "click"))):
            ctx.reset()
            ctx.vm.input("move", "8", "300")
            x, y = _launchpad_card(ctx)
            ctx.vm.input("move", str(x), str(y))
            ctx.settle(0.5)
            before = len(_notes_groups(ctx.state()))
            ctx.vm.input(*args)
            ctx.wait_state(lambda s: len(_notes_groups(s)) > before, 25,
                           f"Notes opened by a touchpad {how} on its launchpad card at {x},{y}")
            res.metrics[how] = "opened Notes"
            ctx.shot(f"I05-{how}")


@check("I06", AREA, "Touchpad: a two-finger horizontal scroll pans the strip", calibrating=CALIBRATING)
def touchpad_scroll(ctx, res):
    g1, _ = _two_windows(ctx)
    with plugged(ctx, TOUCHPAD):
        r = _group_rect(ctx, g1)
        x, y = int(r[0] + r[2] / 2), int(r[1] + r[3] * 0.6)
        st = ctx.state()
        sid = core.active_strip(st)["id"]

        def scroll_x():
            return next(s for s in ctx.state()["chrome"]["strip_scroll"] if s["strip"] == sid)["scroll_x"]

        moved = {}
        for dx in (60, -60):  # whichever way the scroll direction setting points, one has room
            before = scroll_x()
            ctx.vm.input("move", str(x), str(y))
            ctx.vm.input("touchpad", "swipe", str(dx), "0", "--fingers", "2")
            ctx.settle(1.2)
            moved[dx] = [round(before, 1), round(scroll_x(), 1)]
            if abs(moved[dx][1] - moved[dx][0]) > 20:
                break
        res.metrics["scroll_x_by_dx_mm"] = moved
        expect(any(abs(b - a) > 20 for a, b in moved.values()),
               f"a two-finger horizontal touchpad scroll did not pan the strip ({moved})")


@check("I07", AREA, "Touchpad: disable-while-typing suppresses the touchpad while a key of the internal "
                    "keyboard is down, and releases it afterwards", calibrating=CALIBRATING
       + "; libinput may not pair a uinput keyboard and touchpad for typing at all, which this would show")
def disable_while_typing(ctx, res):
    ctx.reset()
    with plugged(ctx, KEYBOARD, TOUCHPAD):
        ctx.vm.input("move", "400", "300")
        # Baseline: with no key in use the pad moves the pointer.
        ctx.vm.key("Escape")
        ctx.wait_state(lambda s: _modality(s) == "keyboard", 5, "keyboard in use after a key")
        ctx.settle(1.5)
        ctx.vm.input("touchpad", "swipe", "20", "0", "--duration", "100")
        ctx.wait_state(lambda s: _modality(s) == "pointer", 5, "the pad moved the pointer with no key in use")
        # A key held down: libinput keeps the pad off while it is, and for a moment after.
        ctx.vm.input("hold", "a")
        try:
            ctx.wait_state(lambda s: _modality(s) == "keyboard", 5, "keyboard in use while a key is held")
            ctx.settle(0.5)
            ctx.vm.input("touchpad", "swipe", "20", "0", "--duration", "100")
            ctx.settle(0.8)
            suppressed = _modality(ctx.state()) == "keyboard"
        finally:
            ctx.vm.input("release", "a")
        res.metrics["suppressed_while_typing"] = suppressed
        expect(suppressed, "the touchpad moved the pointer while a key of the internal keyboard was held: "
                           "disable-while-typing did not engage")
        ctx.settle(2.0)
        ctx.vm.input("touchpad", "swipe", "-20", "0", "--duration", "100")
        ctx.wait_state(lambda s: _modality(s) == "pointer", 5,
                       "the pad moved the pointer again after the key was released")
        ctx.close_overlays()


def _log_lines_after(ctx, mark: int, pattern: str) -> list[str]:
    """The session log's lines past `mark` that match the extended regex `pattern`."""
    out = ctx.vm.exec(f'tail -n +{mark + 1} "{core.SESSION_LOG}" 2>/dev/null '
                      f"| grep -a -E '{pattern}'; true", check=False)
    return [line.strip() for line in out.splitlines() if line.strip()]


def parse_kms_state(text: str) -> list[dict]:
    """The blocks of the kernel's atomic state dump (`/sys/kernel/debug/dri/<minor>/state`):
    one per plane, CRTC and connector, with its `kind`, `name` and first-level properties."""
    blocks, current = [], None
    for line in text.splitlines():
        if line and not line[0].isspace() and "]: " in line:
            current = {"kind": line.split("[", 1)[0], "name": line.split("]: ", 1)[1].strip()}
            blocks.append(current)
        elif current is not None and line.startswith("\t") and not line.startswith("\t\t"):
            key, eq, value = line.strip().partition("=")
            if eq:
                current.setdefault(key.strip(), value.strip())
    return blocks


def scanning_out(blocks: list[dict], connectors) -> dict:
    """For each named connector: the CRTC it is bound to, if that CRTC is active and a plane
    on it holds a framebuffer, else None."""
    crtcs = {b["name"]: b for b in blocks if b["kind"] == "crtc"}
    planes = [b for b in blocks if b["kind"] == "plane"]
    out = {}
    for block in blocks:
        if block["kind"] != "connector" or block["name"] not in connectors:
            continue
        crtc = crtcs.get(block.get("crtc", ""))
        lit = (crtc is not None and crtc.get("active") == "1"
               and any(p.get("crtc") == crtc["name"] and p.get("fb", "0") not in ("0", "")
                       for p in planes))
        out[block["name"]] = crtc["name"] if lit else None
    return out


class Kms:
    """The slot's DRM card as the kernel holds it, read from outside the compositor: its atomic
    state and its CRTCs' output CRCs (vkms computes one per composed frame), from debugfs in
    this throwaway VM."""

    def __init__(self, ctx, connectors: list[str]):
        cards = set()
        for name in connectors:
            for path in ctx.vm.root_exec(f"ls -d /sys/class/drm/card*-{name} 2>/dev/null").split():
                cards.add(path.rsplit("/", 1)[1].split("-", 1)[0])
        if len(cards) != 1:
            raise Unobservable(f"the slot's connectors {connectors} are not on one DRM card: {cards}")
        self.ctx, self.connectors = ctx, connectors
        self.dri = f"/sys/kernel/debug/dri/{cards.pop().removeprefix('card')}"
        ctx.vm.root_exec("mountpoint -q /sys/kernel/debug || mount -t debugfs none /sys/kernel/debug; true")
        if "state" not in ctx.vm.root_exec(f"ls {self.dri} 2>/dev/null; true").split():
            raise Unobservable(f"the kernel shows no atomic state at {self.dri}")

    def scanning_out(self) -> dict:
        return scanning_out(parse_kms_state(self.ctx.vm.root_exec(f"cat {self.dri}/state")),
                            self.connectors)

    def crc(self, crtc: str) -> str | None:
        """The output CRC of the newest frame `crtc` composed in a short capture."""
        crc = f"{self.dri}/{crtc}/crc"
        out = self.ctx.vm.root_exec(f"echo auto > {crc}/control 2>/dev/null && "
                                    f"timeout 2 cat {crc}/data 2>/dev/null; true")
        rows = [l.split()[1:] for l in out.splitlines() if len(l.split()) > 1]
        return " ".join(rows[-1]) if rows else None


def _suspend_and_resume(ctx, res) -> None:
    """Suspend the VM through logind, so the session sees both edges of PrepareForSleep as on
    a laptop whose lid closes, and have it resume by itself: the kernel's suspend test mode
    when it has one (devices suspend, a pause, then resume), else an RTC alarm."""
    caps = ctx.vm.root_exec("echo state=$(cat /sys/power/state 2>/dev/null); "
                            "test -w /sys/power/pm_test && echo pm_test=yes; "
                            "test -w /sys/class/rtc/rtc0/wakealarm && echo rtc=yes; true")
    res.metrics["suspend_support"] = caps.strip()
    states = next((l.split("=", 1)[1].split() for l in caps.splitlines() if l.startswith("state=")), [])
    if not {"mem", "freeze", "standby"} & set(states):
        raise Unobservable(f"the VM kernel cannot suspend (/sys/power/state: {states})")
    if "pm_test=yes" in caps:
        res.metrics["resumed_by"] = "pm_test devices"
        try:
            ctx.vm.root_exec("echo devices > /sys/power/pm_test && systemctl suspend", timeout=60)
            if not wait_until(lambda: _log_lines_after(ctx, res.metrics["log_mark"],
                                                       "session power Woke applied"), 60,
                              interval=1.0):
                raise Unobservable("the session logged nothing of a suspend within 60 s")
        finally:
            with contextlib.suppress(VmError):
                ctx.vm.root_exec("echo none > /sys/power/pm_test", timeout=60)
    elif "rtc=yes" in caps:
        res.metrics["resumed_by"] = "rtc alarm"
        ctx.vm.root_exec("rtcwake -m no -s 15 && systemctl suspend", timeout=60)
    else:
        raise Unobservable("the VM has neither the kernel's suspend test mode nor an RTC alarm "
                           "to wake it")


@check("I08", AREA, "Waking from sleep while blanked: the flight record shows the post-wake state, a forced "
                    "failure to light up is detected and recovered, and the keyboard works after it",
       calibrating=CALIBRATING)
def wake_from_sleep(ctx, res):
    ctx.reset()
    # Suspend locks the session; it is unlocked with the slot user's password, as in I04.
    ctx.vm.root_exec(f'user=$(getent passwd {ctx.vm.slot_uid()} | cut -d: -f1) && '
                     f'echo "$user:{PASSCODE}" | chpasswd')
    try:
        with plugged(ctx, KEYBOARD):
            # A client that keeps committing while the display is dark: the paint loop draws
            # nothing then and owes the change to the frame that wakes it.
            ctx.new_terminal_window()
            ctx.settle(1.0)
            ctx.vm.input("type", "i=0; while :; do i=$((i+1)); echo wake-$i; sleep 0.2; done")
            ctx.vm.key("Return")
            displays = ctx.state()["workspace_tree"]["outputs"]["displays"]
            connectors = [d.get("monitor", "").removeprefix("connector:") for d in displays]
            kms = Kms(ctx, connectors)
            before = kms.scanning_out()
            res.metrics["kms_before"] = before
            expect(before and all(before.values()), f"every output scans out before the test: {before}")
            mark = core.session_log_mark(ctx)
            res.metrics["log_mark"] = mark
            ctx.vm.raw({"op": "display_blank"})
            expect(wait_until(lambda: _log_lines_after(ctx, mark, "session power Blanked applied"),
                              10, interval=0.5),
                   "the display blanked on request")

            def all_off():
                now = kms.scanning_out()
                return now if not any(now.values()) else None

            blanked = wait_until(all_off, 10, interval=0.5)
            res.metrics["kms_blanked"] = blanked or kms.scanning_out()
            expect(blanked is not None, "the kernel shows every CRTC off while blanked: "
                                        f"{res.metrics['kms_blanked']}")
            # The next wake takes the CRTC as dark whatever it reads back: the check must
            # see that and redo the modeset.
            ctx.vm.raw({"op": "display_wake_fault", "stage": "crtc"})
            _suspend_and_resume(ctx, res)

            def recovered():
                return _log_lines_after(ctx, mark, "DRM wake check: .*outcome=lit .*redone=true")

            lit = wait_until(recovered, 90, interval=1.0)
            checks = _log_lines_after(ctx, mark, "DRM wake check: ")
            res.metrics["wake_checks"] = checks
            res.metrics["power"] = _log_lines_after(ctx, mark, "session power ")
            expect(any("outcome=redoing-modeset stage=crtc" in l for l in checks),
                   f"the forced dark CRTC was detected after the wake: {checks}")
            expect(bool(lit), f"the redone modeset lit the output: {checks}")
            expect(all("outcome=failed" not in l for l in checks), f"no output failed to light: {checks}")
            expect(_log_lines_after(ctx, mark, "session power Woke applied"),
                   "the session woke the display after the resume")
            # From outside the compositor: the kernel scans every output out again, and a
            # new frame reaches each CRTC after the recovery.
            after = kms.scanning_out()
            res.metrics["kms_after"] = after
            expect(all(after.get(c) for c in connectors),
                   f"every output's CRTC is active with a framebuffer after the recovery: {after}")
            locked_crcs = {c: kms.crc(after[c]) for c in connectors}
            res.metrics["crc_locked"] = locked_crcs
            if not all(locked_crcs.values()):
                raise Unobservable(f"the kernel gives no output CRCs for the slot's CRTCs: {locked_crcs}")
            held = _log_lines_after(ctx, mark, "DRM input: wake=resume seat_session=active")
            res.metrics["input_after_wake"] = held
            expect(any(KEYBOARD_ID["name"] in l for l in held),
                   f"the session still holds the keyboard after the resume: {held}")
            ctx.vm.key("shift")
            first = wait_until(lambda: _log_lines_after(ctx, mark, "DRM input: first event after wake="),
                               10, interval=0.5)
            res.metrics["first_input"] = first
            expect(bool(first), "a key pressed after the resume reached the compositor")
            res.metrics["unlock"] = _ensure_unlocked(ctx, PASSCODE)
            expect(_locked(ctx) is False, "the keyboard unlocks the session after the resume")
            # The desktop replaced the lock screen: each CRTC composes a different frame.

            def new_frame(connector):
                crc = kms.crc(after[connector])
                return crc if crc and crc != locked_crcs[connector] else None

            changed = {c: wait_until(lambda c=c: new_frame(c), 10, interval=0.5) for c in connectors}
            res.metrics["crc_unlocked"] = changed
            expect(all(changed.values()), f"a new frame reached every output after the recovery: {changed}")
            # The terminal committed all through the blank and the sleep; its output keeps
            # reaching the screen after the wake, frame after frame.
            locked_crcs = changed
            moving = {c: wait_until(lambda c=c: new_frame(c), 10, interval=0.5) for c in connectors}
            res.metrics["crc_moving"] = moving
            expect(any(moving.values()),
                   f"the terminal that kept committing while dark keeps drawing after the wake: {moving}")
            ctx.shot("I08-woken")
    finally:
        _ensure_unlocked(ctx, PASSCODE)
