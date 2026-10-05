#!/usr/bin/env bash
# Input devices that appear after login reach the running compositor.
#
#   input-hotplug-check.sh count NAME
#       Print how many attached devices the compositor lists under NAME.
#   input-hotplug-check.sh wait-count NAME N
#       Wait until the compositor lists exactly N devices named NAME.
#   input-hotplug-check.sh uinput SNAPSHOT
#       As root: plug and unplug a virtual keyboard-and-pointer several
#       times, requiring the compositor at SNAPSHOT (the session user's
#       $XDG_RUNTIME_DIR/tilo/input-state.json) to open and drop it every
#       time, then press Super+L on a freshly plugged one.
#   input-hotplug-check.sh touchscreen SNAPSHOT FIFO OWNER
#       As root: plug a touchscreen, wait until the compositor at SNAPSHOT
#       opens it and print `ready`, then tap where OWNER (the session user)
#       writes `tap FX FY` lines into FIFO: fractions of the output's width
#       and height. `done` unplugs it.
#
# The compositor publishes the devices it has opened to input-state.json, so
# an entry there means libinput took the device, not merely that udev saw it.
# Each unplug raises a burst of uevents; repeating the cycle catches a
# compositor that falls behind on such a burst and then leaves the next
# keyboard or mouse someone plugs in unopened.
set -euo pipefail

snapshot_default() {
  printf '%s/tilo/input-state.json' "${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset}"
}

case "${1:-}" in
  count)
    exec python3 - "$(snapshot_default)" "${2:?device name}" <<'PY'
import json, sys
path, name = sys.argv[1:]
devices = json.load(open(path)).get("devices", [])
print(sum(1 for d in devices if d.get("name") == name))
PY
    ;;
  wait-count)
    exec python3 - "$(snapshot_default)" "${2:?device name}" "${3:?device count}" <<'PY'
import json, sys, time
path, name, want = sys.argv[1], sys.argv[2], int(sys.argv[3])
deadline = time.monotonic() + 20
seen = None
while time.monotonic() < deadline:
    try:
        devices = json.load(open(path)).get("devices", [])
        seen = sum(1 for d in devices if d.get("name") == name)
    except (OSError, ValueError):
        seen = None
    if seen == want:
        print(f"compositor lists {want} x {name!r}")
        sys.exit(0)
    time.sleep(0.2)
sys.exit(f"compositor lists {seen} x {name!r}, expected {want}")
PY
    ;;
  uinput)
    snapshot="${2:?usage: uinput SNAPSHOT}"
    modprobe uinput 2>/dev/null || true
    exec python3 - "$snapshot" <<'PY'
import fcntl, json, os, struct, sys, time

SNAPSHOT = sys.argv[1]
NAME = "Tilo hotplug probe"
CYCLES = 4
# A device opened promptly is listed within milliseconds; the bound only
# absorbs a loaded runner.
DEADLINE = 5

UI_SET_EVBIT, UI_SET_KEYBIT, UI_SET_ABSBIT = 0x40045564, 0x40045565, 0x40045567
UI_DEV_CREATE, UI_DEV_DESTROY = 0x5501, 0x5502
UI_DEV_SETUP, UI_ABS_SETUP = 0x405C5503, 0x401C5504
EV_SYN, EV_KEY, EV_ABS = 0, 1, 3
ABS_X, ABS_Y = 0, 1
KEY_A, KEY_L, KEY_LEFTMETA = 30, 38, 125
BTN_LEFT, BTN_RIGHT = 0x110, 0x111
BUS_USB = 0x03
# struct input_event's timeval is two native longs.
EVENT = "@llHHi"


def listed():
    try:
        devices = json.load(open(SNAPSHOT)).get("devices", [])
    except (OSError, ValueError):
        return None
    return sum(1 for d in devices if d.get("name") == NAME)


def wait_listed(want, what):
    deadline = time.monotonic() + DEADLINE
    seen = None
    while time.monotonic() < deadline:
        seen = listed()
        if seen == want:
            return
        time.sleep(0.05)
    sys.exit(f"{what}: compositor lists {seen} x {NAME!r}, expected {want} (snapshot {SNAPSHOT})")


def plug():
    """A keyboard with an absolute pointer and buttons, like a combined
    receiver or a KVM: the kernel gives it event, mouse and joystick nodes,
    so every plug and unplug raises a burst of four uevents."""
    fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    fcntl.ioctl(fd, UI_SET_EVBIT, EV_KEY)
    fcntl.ioctl(fd, UI_SET_EVBIT, EV_ABS)
    for code in [*range(KEY_A, KEY_L + 1), KEY_LEFTMETA, BTN_LEFT, BTN_RIGHT]:
        fcntl.ioctl(fd, UI_SET_KEYBIT, code)
    for axis, maximum in ((ABS_X, 1919), (ABS_Y, 1079)):
        fcntl.ioctl(fd, UI_SET_ABSBIT, axis)
        fcntl.ioctl(fd, UI_ABS_SETUP, struct.pack("=H2x6i", axis, 0, 0, maximum, 0, 0, 0))
    setup = struct.pack("=4H80sI", BUS_USB, 0x1209, 0x0001, 1, NAME.encode(), 0)
    fcntl.ioctl(fd, UI_DEV_SETUP, setup)
    fcntl.ioctl(fd, UI_DEV_CREATE)
    return fd


def unplug(fd):
    fcntl.ioctl(fd, UI_DEV_DESTROY)
    os.close(fd)


def emit(fd, kind, code, value):
    os.write(fd, struct.pack(EVENT, 0, 0, kind, code, value))
    os.write(fd, struct.pack(EVENT, 0, 0, EV_SYN, 0, 0))


wait_listed(0, "before the first plug")
for cycle in range(1, CYCLES + 1):
    fd = plug()
    wait_listed(1, f"plug {cycle}")
    unplug(fd)
    wait_listed(0, f"unplug {cycle}")
    print(f"plug {cycle}: opened and dropped")

fd = plug()
wait_listed(1, "plug for Super+L")
for code, value in ((KEY_LEFTMETA, 1), (KEY_L, 1), (KEY_L, 0), (KEY_LEFTMETA, 0)):
    emit(fd, EV_KEY, code, value)
    time.sleep(0.03)
# Let the compositor read the presses before the device goes away.
time.sleep(1)
unplug(fd)
wait_listed(0, "unplug after Super+L")
print("Super+L sent from a keyboard plugged in after login")
PY
    ;;
  touchscreen)
    snapshot="${2:?usage: touchscreen SNAPSHOT FIFO OWNER}"
    fifo="${3:?usage: touchscreen SNAPSHOT FIFO OWNER}"
    owner="${4:?usage: touchscreen SNAPSHOT FIFO OWNER}"
    modprobe uinput 2>/dev/null || true
    rm -f "$fifo"
    mkfifo -m 0600 "$fifo"
    chown "$owner" "$fifo"
    trap 'rm -f "$fifo"' EXIT
    python3 - "$snapshot" "$fifo" <<'PY'
import fcntl, json, os, struct, sys, time

SNAPSHOT, FIFO = sys.argv[1:]
NAME = "Tilo touchscreen probe"
DEADLINE = 10

UI_SET_EVBIT, UI_SET_KEYBIT, UI_SET_ABSBIT = 0x40045564, 0x40045565, 0x40045567
UI_SET_PROPBIT = 0x4004556E
UI_DEV_CREATE, UI_DEV_DESTROY = 0x5501, 0x5502
UI_DEV_SETUP, UI_ABS_SETUP = 0x405C5503, 0x401C5504
EV_SYN, EV_KEY, EV_ABS = 0, 1, 3
ABS_X, ABS_Y = 0x00, 0x01
ABS_MT_SLOT, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID = 0x2F, 0x35, 0x36, 0x39
BTN_TOUCH = 0x14A
INPUT_PROP_DIRECT = 0x01
BUS_USB = 0x03
RANGE = 32767
EVENT = "@llHHi"


def listed():
    try:
        devices = json.load(open(SNAPSHOT)).get("devices", [])
    except (OSError, ValueError):
        return None
    return sum(1 for d in devices if d.get("name") == NAME)


def wait_listed(want, what):
    deadline = time.monotonic() + DEADLINE
    seen = None
    while time.monotonic() < deadline:
        seen = listed()
        if seen == want:
            return
        time.sleep(0.05)
    sys.exit(f"{what}: compositor lists {seen} x {NAME!r}, expected {want} (snapshot {SNAPSHOT})")


def plug():
    """A direct-touch screen like a laptop's: multi-touch slots with the
    single-touch axes beside them, reported in its own coordinate range that
    the compositor maps onto the output."""
    fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    fcntl.ioctl(fd, UI_SET_EVBIT, EV_KEY)
    fcntl.ioctl(fd, UI_SET_EVBIT, EV_ABS)
    fcntl.ioctl(fd, UI_SET_KEYBIT, BTN_TOUCH)
    fcntl.ioctl(fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT)
    for axis, minimum, maximum, resolution in (
        (ABS_X, 0, RANGE, 100), (ABS_Y, 0, RANGE, 100),
        (ABS_MT_SLOT, 0, 9, 0), (ABS_MT_TRACKING_ID, 0, 65535, 0),
        (ABS_MT_POSITION_X, 0, RANGE, 100), (ABS_MT_POSITION_Y, 0, RANGE, 100),
    ):
        fcntl.ioctl(fd, UI_SET_ABSBIT, axis)
        fcntl.ioctl(fd, UI_ABS_SETUP, struct.pack("=H2x6i", axis, 0, minimum, maximum, 0, 0, resolution))
    setup = struct.pack("=4H80sI", BUS_USB, 0x1209, 0x0002, 1, NAME.encode(), 0)
    fcntl.ioctl(fd, UI_DEV_SETUP, setup)
    fcntl.ioctl(fd, UI_DEV_CREATE)
    return fd


def emit(fd, events):
    for kind, code, value in events:
        os.write(fd, struct.pack(EVENT, 0, 0, kind, code, value))
    os.write(fd, struct.pack(EVENT, 0, 0, EV_SYN, 0, 0))


def tap(fd, fx, fy, contact):
    x = round(min(max(fx, 0.0), 1.0) * RANGE)
    y = round(min(max(fy, 0.0), 1.0) * RANGE)
    emit(fd, [
        (EV_ABS, ABS_MT_SLOT, 0), (EV_ABS, ABS_MT_TRACKING_ID, contact),
        (EV_ABS, ABS_MT_POSITION_X, x), (EV_ABS, ABS_MT_POSITION_Y, y),
        (EV_KEY, BTN_TOUCH, 1), (EV_ABS, ABS_X, x), (EV_ABS, ABS_Y, y),
    ])
    time.sleep(0.06)
    emit(fd, [(EV_ABS, ABS_MT_SLOT, 0), (EV_ABS, ABS_MT_TRACKING_ID, -1), (EV_KEY, BTN_TOUCH, 0)])
    time.sleep(0.06)


fd = plug()
try:
    wait_listed(1, "touchscreen plugged")
    print("ready", flush=True)
    contact = 0
    finished = False
    while not finished:
        # Each writer's lines end at its close; the next one reopens.
        with open(FIFO) as commands:
            for line in commands:
                words = line.split()
                if words == ["done"]:
                    finished = True
                    break
                if len(words) != 3 or words[0] != "tap":
                    sys.exit(f"unknown touchscreen command {line.strip()!r}")
                contact += 1
                tap(fd, float(words[1]), float(words[2]), contact)
                print(f"tapped {words[1]} {words[2]}", flush=True)
finally:
    fcntl.ioctl(fd, UI_DEV_DESTROY)
    os.close(fd)
print("touchscreen unplugged")
PY
    ;;
  *)
    sed -n '2,18p' "$0" >&2
    exit 2
    ;;
esac
