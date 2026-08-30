#!/usr/bin/env python3
"""One take of a photograph going into the document, by hand, on film.

The storyboard's beat is a drag: a picture is dragged out of Files and
dropped into the word processor. This script is the recording half of
it. Everything is staged before a window appears; you drag the file;
it stops itself when the picture lands.

    ./scripts/yc-demo-insert.py              # you do the dragging
    ./scripts/yc-demo-insert.py --self-test  # it drags, to prove the rig
    ./scripts/yc-demo-insert.py --dry-run    # staging only, headless

What you do: one drag. Files is already open on a home with
`harbour.png` in it, the row is already selected, the document beside it
already has a line in it, and the window comes to the front after a
countdown. Press on the highlighted row, pull it across onto the page,
let go. The counter runs while you do it and the take ends on its own a
few seconds after the picture appears.

What it does, in order: stages a home with the photograph in it, raises
a D-Bus session of its own, starts one compositor in a window sized to
what the window manager will grant, launches the word processor *and*
Files into two groups side by side, checks both are wholly on screen,
types a line into the document, selects the photograph's row so the
first press is yours rather than spent focusing a window, counts down,
records off the compositor's own `snapshot` op, watches the session log
for the picture reaching the document, holds a few seconds so the
landing is on film, stops by itself, verifies the image really is in the
document and writes a 1920x1080 clip.

Re-runnable. Every run is its own directory, its own sockets and its own
timestamped video, so a bad take costs nothing but running it again.

  Nothing here is granted access to anything
  ------------------------------------------
  There is no file-access grant in this script and it does not need one,
  which is the whole point of the beat. Files reads its own file at the
  start of the gesture, because it has the authority to read what it is
  showing; the compositor brokers a pipe; the receiving toolkit spools
  the bytes into its own directory. The word processor is never given a
  path into the home, and could not resolve one if it were — it has its
  own mount namespace.

  How the payoff is detected
  --------------------------
  Off the compositor's own log, not off pixels. The word processor
  reports every change to its document as `[app] broadcast doc delta
  <N>B`, and an image is stored in the document as its own bytes under a
  BLAKE3 hash — so inserting the staged PNG produces a delta the size of
  the file where a keystroke produces ~100 bytes. Both measured; the
  threshold is derived from the file rather than written down.

  What capture does and does not cost
  -----------------------------------
  It does not cost input. Measured rather than assumed, because a whole
  round of debugging was spent suspecting it: with the camera stopped,
  17 synthetic keystrokes produced 17 document deltas; with the camera
  running at 11 frames/s, 16 produced 16, and the document read back
  `alphabravocharliedeltaechofoxtrot` — every character of both, in
  order. A snapshot blocks its session ~50 ms, and that is all it does.

  Windowed, not headless — and why
  -------------------------------
  A person cannot click on a headless compositor, and a drag is worth
  driving where it can also be done by hand. `--backend winit` puts the
  compositor in an ordinary window; because the session's
  XDG_RUNTIME_DIR is its own (the collab broker binds inside it), winit
  is pointed at X11, whose DISPLAY and XAUTHORITY are absolute and
  survive the isolation where WAYLAND_DISPLAY does not.

  The frames are *not* a screen grab. They come off the same `snapshot`
  op the nine-beat recorder uses — the compositor re-renders its own
  scene and hands back a PNG (`native.rs:992`) — so a frame is the
  compositor's own render at 1:1. Nothing is interpolated, no frame is
  repeated, and the host title bar is never in one.

  Why the window is never resized
  -------------------------------
  The winit backend panics when its surface is reconfigured with a frame
  in flight — `Surface::configure` finds a live `SurfaceOutput` and the
  present thread goes down, taking the session with it. Two runs died
  exactly there. So nothing here resizes anything: the window is
  *created* at the size the window manager will grant
  (`NIRIOS_WINIT_SIZE`), and is iconified and re-raised rather than
  resized while the rest is staged.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

# The nine-beat recorder, imported rather than copied: the session
# environment, the capture thread and the composer are all fiddly and all
# already right. Loaded by location because its filename has a hyphen in
# it. Importing it runs definitions only — `main` is behind the usual
# `__main__` guard.
_spec = importlib.util.spec_from_file_location(
    "yc_demo", os.path.join(HERE, "yc-demo.py"))
yc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(yc)

# The window winit gives itself (`backend/winit.rs:371`), which is how it
# is found on the X server.
WINDOW_TITLE = "nirios"

# Why the log rather than the widget registry: a file row belongs to
# Files, and a client's widgets are not in the compositor's registry, so
# there is no name to watch and no rect to read. Both processes narrate
# the gesture into the session log instead, and that is what
# `yc.LogTail` reads.

#: Height a window manager is assumed to add above the client area. Only
#: used where the work area cannot be read; the point is to ask for a
#: size that will be *granted*, since anything else arrives as a resize.
TITLE_BAR_ALLOWANCE = 48

# How long one unattended run will wait for a human before it gives up
# and writes whatever it filmed.
DEFAULT_TIMEOUT_S = 240.0
# Held after the image reaches the document, so its arrival is on film
# with room either side rather than one frame before the camera stopped.
DEFAULT_DWELL_S = 5.0


# --------------------------------------------------------------------
# a compositor in a window
# --------------------------------------------------------------------


def private_session_bus(run_dir: str,
                        runtime_dir: str) -> tuple[str, subprocess.Popen] | None:
    """A D-Bus session bus of this run's own.

    It keeps this session's `org.freedesktop.Notifications` off the
    desktop's bus, where it would otherwise be competing with GNOME for
    your notifications — and it keeps the compositor's portal backend
    from fighting whatever else on the machine has already claimed
    `org.freedesktop.impl.portal.desktop.qos`, since a bus name has
    exactly one owner and every later claimant just logs and gives up.

    Nothing in this take asks the portal for anything any more, so
    failing to raise one is no longer fatal — it costs you a couple of
    notifications on your own desktop.
    """
    # The bus must not inherit the real XDG_RUNTIME_DIR, and this is not a
    # tidiness point — it destroys the desktop's file access.
    #
    # A session bus activates services on demand, and the portal frontend
    # asks for `org.freedesktop.portal.Documents`. dbus-daemon starts
    # `xdg-document-portal` with *its own* environment, and that program
    # mounts a FUSE filesystem at `$XDG_RUNTIME_DIR/doc`. Inheriting
    # /run/user/1000 therefore mounts over the one the real portal is
    # already serving. When this run ends, its `fusermount3
    # --auto-unmount` takes the mount away — including the desktop's.
    #
    # The real xdg-document-portal stays `active` and never notices its
    # mount vanished, so it never rebuilds it. Every flatpak launch then
    # fails, because flatpak binds `$XDG_RUNTIME_DIR/doc/by-app/<id>`
    # into every sandbox:
    #
    #   bwrap: Can't find source path /run/user/1000/doc/by-app/... 
    #
    # and the person cannot start any application until they run
    # `systemctl --user restart xdg-document-portal.service`. Observed on
    # this machine after a filming run; only `doc` died while gvfs, in
    # the same directory, survived — the signature of a targeted unmount
    # rather than a recursive sweep.
    env = dict(os.environ, XDG_RUNTIME_DIR=runtime_dir)
    for daemon in ("/usr/bin/dbus-daemon", shutil.which("dbus-daemon")):
        if not daemon or not os.path.exists(daemon):
            continue
        proc = subprocess.Popen(
            [daemon, "--session", "--print-address", "--nofork"],
            env=env,
            stdout=subprocess.PIPE,
            stderr=open(os.path.join(run_dir, "dbus.log"), "w"),
            preexec_fn=yc.die_with_parent())
        yc.track(proc)
        address = proc.stdout.readline().decode(errors="replace").strip()
        if address:
            return address, proc
        proc.kill()
    return None




class WindowedSession(yc.Session):
    """A `yc.Session` that opens a window instead of running headless.

    The environment a session needs is forty lines of things that are
    easy to get subtly wrong (a relative XDG_CONFIG_HOME persists into
    the real `~/.config/qos`; a shared runtime dir is two sessions
    fighting over one broker socket), and it is already written. So the
    parent's `start` is used verbatim and only the two things that must
    differ are intercepted at the spawn: the backend flag, and putting
    DISPLAY back so winit has an X server to open on.

    WAYLAND_DISPLAY stays unset deliberately. The host's Wayland socket
    lives in the host's XDG_RUNTIME_DIR, and this session's runtime dir
    is its own, so winit would look for it in the wrong place; X11 is
    reached through DISPLAY and an absolute XAUTHORITY, neither of which
    the isolation touches. It also leaves the window addressable by
    `wmctrl`, which is what makes 1920x1080 automatic.
    """

    #: Set by `main` before `start`, when a private bus could be raised.
    bus_address: str | None = None
    #: `WxH` the host window is *created* at, never resized to. See
    #: `NIRIOS_WINIT_SIZE` in `backend/winit.rs`: a resize with a frame in
    #: flight panics the present thread, so the only safe way to record
    #: at the video's own resolution is to open the window at it.
    window_size: str | None = None

    def start(self) -> None:
        real_popen = subprocess.Popen

        def spawn(argv, **kw):
            argv = list(argv)
            if "--backend" in argv:
                argv[argv.index("--backend") + 1] = "winit"
            env = dict(kw.get("env") or os.environ)
            for var in ("DISPLAY", "XAUTHORITY"):
                if os.environ.get(var):
                    env[var] = os.environ[var]
            env["WINIT_UNIX_BACKEND"] = "x11"
            env.pop("WAYLAND_DISPLAY", None)
            if self.bus_address:
                env["DBUS_SESSION_BUS_ADDRESS"] = self.bus_address
            if self.window_size:
                env["NIRIOS_WINIT_SIZE"] = self.window_size
            kw["env"] = env
            return real_popen(argv, **kw)

        yc.subprocess.Popen = spawn
        try:
            super().start()
        finally:
            yc.subprocess.Popen = real_popen


# --------------------------------------------------------------------
# the X window
# --------------------------------------------------------------------


def _run(argv: list[str]) -> str:
    done = subprocess.run(argv, capture_output=True, text=True)
    return done.stdout.strip()


def find_window(pid: int, timeout: float = 60.0) -> str | None:
    """The compositor's X11 window id, once it has mapped one.

    By pid first, by title second: `_NET_WM_PID` is the reliable answer
    when it is set, and the title is the one that still works when it is
    not. Only the compositor has an X window — the apps it launches are
    Wayland clients *of it*, so nothing else can match.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        for argv in (["xdotool", "search", "--pid", str(pid)],
                     ["xdotool", "search", "--name", f"^{WINDOW_TITLE}$"]):
            for wid in _run(argv).split():
                if _run(["xdotool", "getwindowname", wid]) == WINDOW_TITLE:
                    return wid
        time.sleep(0.4)
    return None


def window_size(wid: str) -> tuple[int, int] | None:
    m = re.search(r"Geometry:\s*(\d+)x(\d+)",
                  _run(["xdotool", "getwindowgeometry", wid]))
    return (int(m.group(1)), int(m.group(2))) if m else None


def set_fullscreen(wid: str, on: bool) -> None:
    verb = "add" if on else "remove"
    subprocess.run(["wmctrl", "-i", "-r", hex(int(wid)),
                    "-b", f"{verb},fullscreen"], capture_output=True)


def work_area() -> tuple[int, int] | None:
    """The usable area of the screen, in pixels.

    Not the monitor size: a desktop with a top bar reserves part of it,
    and a window manager will *shrink* a window that asks for more —
    which is a resize, and a resize is what kills this backend. Asking
    for exactly what will be granted means nothing is ever resized.

    Read from `_NET_WORKAREA` via `wmctrl -d`, whose line looks like
    `0  * DG: 1920x1080  VP: 0,0  WA: 0,43 1920x1037  name`.
    """
    for line in _run(["wmctrl", "-d"]).splitlines():
        m = re.search(r"WA:\s*-?\d+,-?\d+\s+(\d+)x(\d+)", line)
        if m:
            return int(m.group(1)), int(m.group(2))
    # No panel to ask (a bare X server, a nested compositor). Fall back
    # to the screen less a title bar's worth: a window manager sizes the
    # *frame* to fit the screen, so a client that asks for the full
    # height is granted less than it asked for — which is a resize, and
    # resizes are what this is all avoiding.
    m = re.match(r"(\d+)\s+(\d+)", _run(["xdotool", "getdisplaygeometry"]))
    if m:
        return int(m.group(1)), max(400, int(m.group(2)) - TITLE_BAR_ALLOWANCE)
    return None


def active_window() -> str | None:
    """Whatever window is focused right now — your terminal, when this
    starts. Kept so it can be put back in front of the fullscreen
    compositor for the setup and the countdown."""
    wid = _run(["xdotool", "getactivewindow"])
    return wid if wid.isdigit() else None


def hide_window(wid: str) -> None:
    """Iconify, so the window is out of the way while this sets up.

    It has to *exist* early — the compositor opens it as it starts — but
    it does not have to be looked at. Iconified, the compositor keeps
    running and the app still launches into it; what it stops being is a
    window sitting on someone's desktop through a setup they have no part
    in. Iconifying changes no dimension, so it is not a resize.
    """
    subprocess.run(["xdotool", "windowminimize", wid], capture_output=True)


def raise_window(wid: str) -> None:
    """Put the window in front and give it the keyboard.

    Both, because they are separate things under a window manager: a
    raised window that is not focused takes the click that focuses it and
    swallows it, which on a five-second countdown reads as "the button
    did nothing".
    """
    subprocess.run(["wmctrl", "-i", "-a", hex(int(wid))], capture_output=True)
    subprocess.run(["xdotool", "windowactivate", wid], capture_output=True)


def notify(body: str, replace_id: int = 0, urgency: str = "normal") -> None:
    """A desktop notification, for the stretch when the fullscreen window
    is over the terminal and nothing printed can be read.

    Best-effort by design: a machine without a notification daemon still
    records perfectly well, and this must never be the thing that stops a
    take. Deliberately rare — GNOME draws banners at the top centre,
    which is exactly where the toolbar being clicked is.
    """
    if shutil.which("notify-send") is None:
        return
    argv = ["notify-send", "-a", "qOS demo", "-u", urgency,
            "-t", "6000", "qOS demo", body]
    if replace_id:
        argv[1:1] = ["-r", str(replace_id)]
    subprocess.run(argv, capture_output=True)


# --------------------------------------------------------------------
# reading what came out
# --------------------------------------------------------------------


def png_size(path: str) -> tuple[int, int] | None:
    """A PNG's pixel size, off its IHDR."""
    try:
        with open(path, "rb") as fh:
            head = fh.read(24)
    except OSError:
        return None
    if len(head) < 24 or head[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    return struct.unpack(">II", head[16:24])


def carries_bytes(blob: bytes, needle: bytes) -> bool:
    """Whether an Automerge save contains `needle` verbatim.

    The word processor stores an image as `ScalarValue::Bytes` under its
    BLAKE3 hash (`qdoc/src/model.rs:2005`), so the PNG's own bytes are in
    the document — that is what makes this readable without an Automerge
    binding. Columns over a size threshold are DEFLATE'd, so plausible
    compressed spans are inflated and searched too, the same way
    `yc-demo.py` searches for text.
    """
    if needle in blob:
        return True
    for start in range(min(len(blob), 1 << 20)):
        if blob[start] != 0x78 or start + 1 >= len(blob):
            continue
        if blob[start + 1] not in (0x01, 0x5E, 0x9C, 0xDA):
            continue
        try:
            if needle in zlib.decompressobj().decompress(blob[start:], 1 << 25):
                return True
        except zlib.error:
            continue
    return False


def image_is_in_the_document(session, room: str, png: bytes,
                             timeout: float = 20.0) -> tuple[bool, int]:
    """Ask the running word processor for its document, and look for the
    image in it.

    A late joiner on this session's broker announces the word processor's
    address and sends the one frame every toolkit answers with its whole
    document. What comes back was produced by the app out of the document
    it is holding, so this is the difference between "an image appeared
    on screen" and "the image is in the document" — which is the claim
    that matters, because it is what replicates.

    Deliberately after the camera has stopped: attaching announces a
    presence, and a phantom collaborator arriving mid-take would be a
    thing on screen that the demo did not do.
    """
    reader = None
    try:
        reader = yc.DocumentReader(session.collab_json_sock, room, record=None)
    except OSError:
        return False, 0
    try:
        deadline = time.time() + timeout
        newest = 0
        # The PNG's signature plus its IHDR: enough to be this file
        # rather than any file, short enough to survive being stored
        # across an Automerge column boundary.
        needle = png[:24]
        while time.time() < deadline:
            blobs = reader.documents() + reader.deltas()
            for blob in blobs:
                newest = max(newest, len(blob))
                if carries_bytes(blob, needle):
                    return True, len(blob)
            time.sleep(0.5)
        return False, newest
    finally:
        reader.close()


# --------------------------------------------------------------------
# the take
# --------------------------------------------------------------------


def countdown(seconds: int, instruction: str, quiet: bool = False) -> None:
    """The one line to read, then the seconds until the camera rolls.

    A banner goes out with it, short enough to have cleared before the
    take starts: the fullscreen window comes to the front at zero, and if
    the terminal did not manage to stay above it this is the only thing
    still readable.
    """
    print()
    print("  \033[1m" + instruction + "\033[0m")
    print()
    if not quiet and seconds:
        notify(f"On camera in {seconds}s. {instruction}", replace_id=8801)
    for n in range(seconds, 0, -1):
        sys.stdout.write(f"\r  on camera in {n}...   ")
        sys.stdout.flush()
        time.sleep(1.0)
    sys.stdout.write("\r  recording.            \n")
    sys.stdout.flush()


def app_tile(session, surface_id=None) -> list | None:
    """The rect of a client tile on screen, by surface.

    Read off `foreign_frames` — the placement the compositor is painting
    — rather than out of the widget registry. The registry files the same
    tile under two names, `foreign-surface-<id>` and
    `foreign-surface-<id>[<tab>]`, and which of them exists depends on
    whether the compositor knows the tab yet. This used to exclude any
    name ending in `]`, so on a run where only the tabbed spelling was
    registered it found nothing at all and the document was silently
    never seeded — the take then rolled on an empty page.
    """
    for sid, tile in yc.painted_tiles(session).items():
        if surface_id is None or sid == int(surface_id):
            return list(tile["rect"])
    return None


def focus_the_app(session, surface_id=None) -> bool:
    """Click once into the document, so his click is not the one that
    focuses the window.

    The first click on an unfocused client is spent on the focus change
    — `route_input` latches the grab, raises the group and syncs keyboard
    focus — and does not reach the app's own widgets. Measured, not
    assumed: a click on the toolbar's image button did nothing at all
    until something had focused the window first. Doing it here means the
    person's first click is a click on the button.
    """
    rect = app_tile(session, surface_id)
    if not rect:
        return False
    session.click_at(rect[0] + rect[2] * 0.45, rect[1] + rect[3] * 0.5)
    session.settle(1.2)
    return True


#: Typed into the document during setup, so the image lands *into* a
#: document rather than onto an empty page. An empty page says nothing
#: about where the picture is going.
#:
#: ASCII only, and typed with `type_prose` rather than `type`: a client
#: resolves each keycode through the seat's xkb state, so without a real
#: Shift held across them every capital here would arrive lowercase and
#: the line would read as a typo on camera.
OPENING_LINE = "Launch plan. The venue, the date, and a photo of the room."


def seed_the_document(session, surface_id=None) -> bool:
    """Put a line of real text in the document before the camera rolls.

    Typed through the same synthetic-input path the rest of the demo
    uses, into the running app, during setup — so what is on screen when
    the take starts is a document with something in it and a caret
    waiting at the end of it.
    """
    if not focus_the_app(session, surface_id):
        return False
    session.type_prose(OPENING_LINE)
    # And a blank line under it for the photograph to land on. The word
    # processor moves its caret to wherever the pointer was when the file
    # was released, so without an empty line to aim at the picture goes
    # in wherever along the sentence he happened to let go — which is the
    # middle of a word about as often as not.
    session.key("Enter")
    session.settle(1.0)
    return True


def pointer_button(session, pressed: bool) -> None:
    """Press or release the left button where the pointer already is.

    `click_at` bundles press and release together, which cannot express a
    drag: the source client has to still be *holding* the button when it
    calls `start_drag`, and the drop only happens on a release that
    arrives after the pointer has moved over another client. These two
    ops decompose that (`test_control.rs:198`).
    """
    session.input_ops += 1
    yc.ok(yc.call(session.sock, {"op": "pointer_button", "pressed": pressed}))


def drag(session, start: tuple[float, float], end: tuple[float, float],
         steps: int = 24, settle: float = 0.05) -> None:
    """Drag from one point to another, as a real gesture.

    Moved in steps rather than teleported: a drag that jumps from source
    to destination in one motion gives the compositor no chance to notice
    the pointer crossing out of the source tile, which is the moment it
    promotes a client drag. It also looks like nothing on camera.

    Measured, so this is not superstition about timing: with the camera
    running, 16 of 16 synthetic keystrokes landed and the app broadcast
    16 document deltas for them — capture does not cost input. The steps
    are for the compositor's sake and for the film, not for reliability.
    """
    x0, y0 = start
    x1, y1 = end
    session.pointer_move(x0, y0)
    time.sleep(0.2)
    pointer_button(session, True)
    time.sleep(0.2)
    for i in range(1, steps + 1):
        t = i / steps
        session.pointer_move(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
        time.sleep(settle)
    time.sleep(0.4)
    pointer_button(session, False)


def the_two_tiles(session) -> tuple[tuple | None, tuple | None]:
    """Where Files and the word processor are actually being painted.

    Off `foreign_frames`, so this is the placement the compositor is
    drawing rather than a name that might be registered twice or not at
    all.
    """
    files = word = None
    for t in yc.painted_tiles(session).values():
        if t["app_id"] == yc.FILES_APP:
            files = t["rect"]
        elif t["app_id"] == yc.WORD_PROCESSOR:
            word = t["rect"]
    return files, word


def select_the_photo(session, log) -> bool:
    """Leave the photograph's row selected and Files focused.

    Two things this buys, both of which the person would otherwise have
    to spend a gesture on. The first click on an unfocused client is
    consumed by the focus change and never reaches the app, so without
    this his press would select the row instead of starting the drag.
    And a highlighted row is the demo pointing at what to pick up.

    The row is found the same way the automated take finds it — by
    pressing and reading the app's own account of what it hit — so it
    cannot leave the wrong row selected and call it done.
    """
    files, _ = the_two_tiles(session)
    if not files:
        return False
    listing = yc.expected_listing(session.home)
    held = yc.press_the_photo_row(session, log, files, yc.PHOTO_NAME, listing)
    # A press with no travel is a click: the row is selected, and the
    # drag the app armed is disarmed by the release.
    pointer_button(session, False)
    session.settle(0.4)
    return held is not None


def drag_the_photo(session, log) -> str:
    """Perform the gesture this script exists to have performed by hand.

    For `--self-test` only. It is the same sequence the nine-beat film
    drives, and it is here so the whole rig — staging, capture, the watch
    loop, the teardown — can be proved without a person.
    """
    files, word = the_two_tiles(session)
    if not files or not word:
        return "no-windows"
    listing = yc.expected_listing(session.home)
    held = yc.press_the_photo_row(session, log, files, yc.PHOTO_NAME, listing)
    if held is None:
        pointer_button(session, False)
        return "no-row"
    hx, hy = held
    # Past the app's drag threshold slowly, while the button is down —
    # that is when Files opens its data source and the compositor takes
    # the gesture over.
    for step_px in (4, 10, 18, 30):
        session.pointer_move(hx + step_px, hy - step_px * 0.4)
        time.sleep(0.12)
    wx, wy, ww, wh = word
    # Below all the text, onto the blank line the seeding left there:
    # the word processor clamps the drop to the nearest line, and an
    # empty one has no character to land in the middle of. Left of
    # centre keeps it off the styles panel, which the drop handler
    # ignores entirely.
    drop = (wx + ww * 0.30, wy + wh * 0.55)
    x0, y0 = hx + 30, hy - 12
    for i in range(1, 27):
        t = i / 26
        session.pointer_move(x0 + (drop[0] - x0) * t, y0 + (drop[1] - y0) * t)
        time.sleep(0.05)
    time.sleep(0.4)
    pointer_button(session, False)
    return "dropped"


def self_test_drag(session, log_path: str) -> None:
    """Stand in for the hand this script cannot supply.

    On a thread of its own, so the watch loop is already counting while
    the gesture arrives — exactly as it would be with a person.
    """
    import threading

    # Its own reader: `read` consumes what it returns, so sharing one
    # with the watch loop would have the two stealing each other's lines.
    log = yc.LogTail(log_path)
    log.catch_up()

    def drive() -> None:
        time.sleep(1.5)
        outcome = drag_the_photo(session, log)
        if outcome != "dropped":
            print(f"\n  --self-test: the gesture did not start ({outcome})")

    threading.Thread(target=drive, daemon=True).start()


def wait_for_the_drop(log, recorder, timeout: float,
                      dwell: float, landed_bytes: int) -> str:
    """Watch until the photograph is in the document, then hold and stop.

    Three outcomes, kept apart because they are different facts: it
    landed; it was picked up but nothing arrived before the clock ran
    out; or it was never picked up at all. Each of them means a different
    thing went wrong, and saying "it did not work" for all three would
    send the next run looking in the wrong place.

    Reading a log file is not input: nothing in here touches the session,
    so the camera and the person have the compositor to themselves.
    """
    deadline = time.time() + timeout
    carried_at = None
    last_print = 0.0
    while time.time() < deadline:
        fresh = log.read()
        if carried_at is None and yc.LogTail.PROMOTED in fresh:
            carried_at = time.time()
            sys.stdout.write("\r  you are carrying it — let go over the "
                             "page                    \n")
            sys.stdout.flush()
        if log.biggest_delta(fresh) >= landed_bytes:
            print(f"\n  the photograph is in the document; holding "
                  f"{dwell:.0f}s so it is on film")
            time.sleep(dwell)
            return "landed"
        now = time.time()
        if now - last_print >= 1.0:
            last_print = now
            state = ("carrying it" if carried_at
                     else "waiting for you to pick it up")
            sys.stdout.write(
                f"\r  recording {now - recorder.started_at:5.0f}s   "
                f"{len(recorder.frames):5d} frames   {state}   "
                f"({deadline - now:.0f}s left)   ")
            sys.stdout.flush()
        time.sleep(0.3)
    print()
    return "no-drop" if carried_at else "no-drag"


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--shell", default=None,
                    help="compositor binary. Defaults to target/demo/shell, "
                         "then target/debug/shell. Must be a build with "
                         "debug-assertions on: QOS_TEST_CONTROL is compiled "
                         "out of --release, and this script has nothing to "
                         "talk to without it")
    ap.add_argument("--out", default=os.path.join(REPO, "target",
                                                  "yc-demo-insert"),
                    help="run directory for state, logs, frames")
    ap.add_argument("--video", default=None,
                    help="where the clip goes (default: a timestamped file "
                         "under .claude/demo-artifacts/, which is gitignored)")
    ap.add_argument("--windowed", action="store_true",
                    help="do not take the window fullscreen. Keeps your "
                         "terminal visible during the take, at the price of "
                         "recording at the window's size and letterboxing it "
                         "into the video instead of filling it 1:1")
    ap.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S,
                    metavar="S", help="how long to wait for you")
    ap.add_argument("--dwell", type=float, default=DEFAULT_DWELL_S,
                    metavar="S",
                    help="seconds held after the image reaches the "
                         "document, so its arrival is on film")
    ap.add_argument("--countdown", type=int, default=5, metavar="N",
                    help="seconds counted down before the camera rolls")
    ap.add_argument("--fps", type=int, default=30, metavar="N",
                    help="container frame rate of the clip. The capture rate "
                         "is whatever the compositor sustained; frames are "
                         "held, never interpolated")
    ap.add_argument("--max-hold", type=float, default=1.8, metavar="S",
                    help="longest one unchanged frame is shown for, which "
                         "trims the stretches where nothing moved because "
                         "nobody was doing anything. Never shows a frame for "
                         "longer than it was live. 0 keeps wall-clock time")
    ap.add_argument("--keep", action="store_true",
                    help="leave the session running afterwards")
    ap.add_argument("--self-test", action="store_true",
                    help="prove the rig with no hands: performs the drag "
                         "itself, aiming by pressing and reading back what "
                         "the app says it grabbed. Exercises the whole "
                         "chain; says nothing about how easy the row is to "
                         "grab with a mouse")
    ap.add_argument("--dry-run", action="store_true",
                    help="stage everything headless, check it, and stop. "
                         "No window, no X server, no recording — so it can "
                         "be run over ssh, on a busy machine, or just to "
                         "see whether the scene still composes before "
                         "committing a take to it")
    args = ap.parse_args()

    # ---- preflight ------------------------------------------------
    #
    # Every one of these is a thing that makes the run pointless rather
    # than merely worse, and every one of them is cheaper to say now than
    # to discover after a countdown.
    shell_bin = args.shell
    if shell_bin is None:
        for profile in ("demo", "debug"):
            candidate = os.path.join(REPO, "target", profile, "shell")
            if os.path.exists(candidate):
                shell_bin = candidate
                break
    if not shell_bin or not os.path.exists(shell_bin):
        print("no compositor binary. Build one:\n"
              "  cargo build --profile demo -p shell --bin shell",
              file=sys.stderr)
        return 2
    if not args.dry_run and not os.environ.get("DISPLAY"):
        print("no DISPLAY. This take needs a window you can click in, which "
              "needs an X server (XWayland counts). Run it from a terminal "
              "on your desktop rather than over ssh, or use --dry-run to "
              "check the staging without one.", file=sys.stderr)
        return 2
    missing = [] if args.dry_run else [
        t for t in ("xdotool", "wmctrl", "ffmpeg", "ffprobe")
        if shutil.which(t) is None]
    if missing:
        print(f"missing: {', '.join(missing)}", file=sys.stderr)
        return 2

    live = yc.registry_entries(os.path.join(yc.LIVE_APPS, "registry.toml"))
    entries = yc.registry_entries(os.path.join(yc.MACHINE_APPS, "registry.toml"))
    if yc.WORD_PROCESSOR in live:
        entries[yc.WORD_PROCESSOR] = live[yc.WORD_PROCESSOR]
        source = f"{yc.LIVE_APPS} (built from this tree)"
    elif yc.WORD_PROCESSOR in entries:
        source = f"{yc.MACHINE_APPS} (the machine's installed payload)"
    else:
        print("no word processor is installed anywhere this can see, so "
              "there is nothing to insert an image into.", file=sys.stderr)
        return 2

    args.out = os.path.abspath(args.out)
    # A clean slate every run. A session persists its workspaces, and a
    # re-run over the last run's state opens onto whatever the last run
    # left on screen.
    shutil.rmtree(args.out, ignore_errors=True)
    run_dir = os.path.join(args.out, "ada")
    apps_dir = os.path.join(run_dir, "qos-apps")
    yc.compose_registry(apps_dir, entries)
    # Under HOMES_ROOT, not under the run directory. Files draws the
    # directory it is showing component by component, and a home inside
    # the checkout renders on camera as
    # `… › quickdesktop › .claude › worktrees › yc-demo › target › …` —
    # which tells a viewer they are looking at a harness, and which
    # tooling built it. `HOMES_ROOT` reads as a machine's own.
    home = yc.stage_home(os.path.join(yc.HOMES_ROOT, "ada"))

    # The photograph, where `stage_home` puts it: at the top of the home,
    # which is the directory Files opens on, and in Pictures. It has to
    # be *visible in the window he is dragging out of*, and a file in a
    # folder he would have to navigate into first is not.
    #
    # This used to write its own copy into `home/Demo/`, a directory
    # `stage_home` does not create — so the run died with a
    # FileNotFoundError before a window ever appeared.
    photo = os.path.join(home, yc.PHOTO_NAME)
    png = open(photo, "rb").read()

    # The packaging chain, by path rather than off PATH, so the run uses
    # the binaries built for it. Without `qos-launcher` in particular an
    # app launch is refused outright — there is no sandbox to spawn the
    # payload into — and the take has nothing to click on.
    def tool(name: str) -> str | None:
        for profile in ("demo", "debug"):
            path = os.path.join(REPO, "target", profile, name)
            if os.path.exists(path):
                return path
        return None

    tools = {var: path for var, name in (
        ("QOS_CATALOGUE_BIN", "qos-catalogue"),
        ("QOS_INSTALL_BIN", "qos-install"),
        ("QOS_LAUNCHER_BIN", "qos-launcher"),
    ) if (path := tool(name))}
    if "QOS_LAUNCHER_BIN" not in tools:
        print("no qos-launcher in target/{demo,debug}: an app launch will be "
              "refused, so there would be no word processor to click in. "
              "Build it:\n  cargo build -p qos-launcher --bin qos-launcher",
              file=sys.stderr)
        return 2

    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    pid = os.getpid()
    yc.KEEP_SESSIONS = args.keep
    # A dry run is the same staging with no window in it: the plain
    # headless session, so it needs no X server and cannot put anything
    # on anybody's screen. Everything the take depends on — the two apps,
    # the layout, the seeded line, the selected row — is the same code
    # either way, which is what makes it worth running.
    session = (yc.Session if args.dry_run else WindowedSession)(
        "ada", "ada", run_dir,
        # Short: AF_UNIX caps sun_path near 108 bytes and a run directory
        # deep in a worktree blows straight past it.
        os.path.join(runtime, f"qos-ins-{pid}.sock"),
        shell_bin, "1600x900",
        os.path.join(runtime, f"qins{pid}"),
        home, apps_dir, tools)
    # Opened at the largest size the window manager will grant without
    # shrinking it — so it is never resized, and so the frames are as
    # close to the video's own resolution as this screen allows. Falls
    # back to the video size where there is no work area to read (which
    # is also the right answer on a bare X server with no panel).
    if args.dry_run:
        session.size = f"{yc.VIDEO_W}x{yc.VIDEO_H - yc.BAND_H}"
    elif args.windowed:
        session.window_size = None
    else:
        area = work_area() or (yc.VIDEO_W, yc.VIDEO_H)
        session.window_size = f"{area[0]}x{area[1]}"
    # No peer gossip. This take is one person and one machine; an explicit
    # demo peer or configured host would paint its cursor and name pill over
    # the document, which reads as an uninvited collaborator on camera.
    session.peer_sync = False
    yc.SESSIONS = [session]

    print(f"compositor:     {shell_bin}")
    print(f"word processor: {source}")
    print(f"run directory:  {args.out}")

    video_path = args.video or os.path.join(
        REPO, ".claude", "demo-artifacts",
        f"insert-image-{time.strftime('%Y%m%d-%H%M%S')}.mp4")
    video_path = os.path.abspath(video_path)
    os.makedirs(os.path.dirname(video_path), exist_ok=True)
    record_dir = os.path.join(args.out, "recording")
    os.makedirs(record_dir, exist_ok=True)

    # Yours, so it can be put back in front while this sets up.
    terminal = None if args.dry_run else active_window()

    recorder = None
    wid = None
    outcome = "not reached"
    rc = 0
    instruction = (f"Drag the highlighted {yc.PHOTO_NAME} onto the page "
                   f"on the right.")
    bus = None
    frontend = None
    try:
        # ---- stage ------------------------------------------------
        print("\nsetting up:")
        # A dry run puts nothing on screen, so it has no notifications to
        # keep off anybody's bus and no reason to raise one.
        raised = (None if args.dry_run
                  else private_session_bus(run_dir, session.runtime_dir))
        if args.dry_run:
            pass
        elif raised:
            session.bus_address, bus = raised
            print(f"  private session bus    pid {bus.pid} "
                  f"(its notifications stay off your desktop's bus)")
        else:
            print("  ! no dbus-daemon, so this session shares your desktop's "
                  "bus. Harmless here — nothing in this take asks the "
                  "portal for anything — but its notifications will land "
                  "on your desktop.")
        session.start()
        session.wait_ready(timeout=180.0)
        wid = None if args.dry_run else find_window(session.proc.pid)
        if wid is None and not args.dry_run:
            raise RuntimeError(
                "the compositor started but never mapped an X window. "
                "Without one there is nothing to click on; see "
                f"{session.log_path}")
        print(f"  compositor up          pid {session.proc.pid}, "
              + (f"window {window_size(wid)}" if wid is not None
                 else f"headless at {session.size}"))

        # Deliberately no fullscreen. The window was created at the size
        # the window manager will actually grant, so it is never resized
        # — and a resize is the one operation that reliably kills this
        # backend. Going fullscreen from a work-area-sized window is a
        # real size change, and it took a run down at exactly this point.
        #
        # It costs nothing worth having: the recording is the
        # compositor's own render of its scene, so the host title bar was
        # never in a frame to begin with.
        if session.proc.poll() is not None:
            raise RuntimeError(
                f"the compositor went down during startup; see "
                f"{session.log_path}")
        # Out of sight for the rest of the setup. It comes back — at the
        # size it already is — when the camera rolls.
        if wid is not None:
            hide_window(wid)
        # No portal frontend is raised any more. This take used to drive
        # the OS file chooser, which meant a whole D-Bus service had to
        # be up and had to win a well-known name before the app asked
        # for anything — three ways for the setup to fail before the
        # camera rolled. A drag asks nobody for anything.

        # Your terminal back in front, so the rest of the setup and the
        # countdown can be read. A raise, never a resize.
        if terminal:
            raise_window(terminal)

        room = session.active_workspace()["room"]
        # No grant, and deliberately none. The word processor is never
        # handed a path into this home — Files reads its own file when
        # the gesture starts and the bytes cross a pipe the compositor
        # brokers — so there is nothing here for a capability to permit.
        # The chooser route needed one; this is what replaced it.

        # Files first, so it is the left-hand window: the gesture reads
        # left to right, and `frame_both_windows` scrolls the strip back
        # by focusing the leftmost group.
        files = session.launch(yc.FILES_APP, search="files")
        if files is None:
            raise RuntimeError(
                "Files did not open, so there is nothing to drag *from*. "
                f"See {session.log_path}")
        print(f"  files up               surface "
              f"{files.get('surface_id')} at frame {files.get('frame_seq')}")

        # The document, in a group of its own. Without
        # `place_next_window` the placement rule tabs a new window into
        # the focused group, and a tab is not a second window on screen —
        # which is the whole precondition for dragging between them.
        session.place_next_window(yc.WORD_PROCESSOR)
        painted = session.launch(yc.WORD_PROCESSOR, search="word")
        if painted is None:
            raise RuntimeError(
                "the word processor did not open, so there is nothing to "
                f"drop a photograph into. See {session.log_path}")
        print(f"  word processor up      surface "
              f"{painted.get('surface_id')} at frame {painted.get('frame_seq')}")

        # Both windows being *painted*, before anything is staged against
        # them. `frame_seq` and a laid-out rect arrive seconds before the
        # shell composites the tile, and in that window the compositor
        # answers every question with plausible geometry while a snapshot
        # comes back as an empty desktop. A take rolled then would film
        # the backdrop.
        unpainted = yc.wait_until_painted(session)
        print("  windows painted        "
              + ("yes" if not unpainted
                 else f"NO — {unpainted} never presented"))

        # Even shares and the strip scrolled back, then checked: a window
        # hanging off an edge is one the gesture would cross out of, and
        # it is worth saying so here rather than discovering it on film.
        viewport = tuple(int(v) for v in session.size.lower().split("x"))
        yc.frame_both_windows(session, viewport)
        session.settle(0.8)
        clipped = yc.windows_off_screen(session, viewport)
        print("  both windows in frame  "
              + ("yes" if not clipped else "NO — " + "; ".join(clipped)))

        wp_surface = painted.get("surface_id")
        print(f"  document ready         "
              f"{'yes' if seed_the_document(session, wp_surface) else 'no'}")

        # The row he is going to drag, selected — which also leaves Files
        # focused. Both matter: the first click on an unfocused client is
        # spent on the focus change and never reaches the app, so without
        # this his press would select the row instead of picking it up;
        # and a highlighted row is the demo pointing at what to grab.
        staging_log = yc.LogTail(session.log_path)
        staging_log.catch_up()
        picked = select_the_photo(session, staging_log)
        print(f"  {yc.PHOTO_NAME + ' selected':<22} "
              + ("yes" if picked else
                 "NO — the row was not found; he will have to click it"))
        print(f"  photograph staged      {photo} ({len(png)} bytes)")

        # What the recording will actually be made of — the compositor's
        # own render, which is the thing that matters, not the X server's
        # opinion of the window.
        probe = session.snapshot(os.path.join(args.out, "probe.png"))
        rendered = png_size(probe) or (1600, 900)
        print(f"  rendering at           {rendered[0]}x{rendered[1]}"
              + ("" if rendered == (yc.VIDEO_W, yc.VIDEO_H) else
                 f"  (not {yc.VIDEO_W}x{yc.VIDEO_H}: frames get scaled and "
                 f"letterboxed into the video)"))

        if args.dry_run:
            # Everything a take depends on, checked and named, with no
            # window and no camera. What it cannot check is the one thing
            # only a hand can do, and it says so rather than implying the
            # gesture was tested.
            shot = session.snapshot(os.path.join(args.out, "staged.png"))
            files_rect, wp_rect = the_two_tiles(session)
            print("\nstaged, headless:")
            print(f"  files at               "
                  f"{[round(v) for v in files_rect] if files_rect else None}")
            print(f"  document at            "
                  f"{[round(v) for v in wp_rect] if wp_rect else None}")
            print(f"  home listed as         "
                  f"{yc.expected_listing(session.home)}")
            print(f"  frame                  {shot}")
            ready = bool(files_rect and wp_rect and not clipped
                         and not unpainted and picked)
            print(f"\n{'ready for a take' if ready else 'NOT ready'}: two "
                  f"windows{'' if ready else ' were not'} side by side and "
                  f"wholly on screen, a line in the document and "
                  f"{yc.PHOTO_NAME} selected in Files.")
            if not args.self_test:
                print("  the gesture itself is not exercised here — add "
                      "--self-test to drive it, or just run it and drag.")
                outcome = "dry-run"
                return 0 if ready else 1

            # `--dry-run --self-test`: the whole chain except the window
            # and the camera. It drives the same gesture and runs the
            # same watch loop the take does, so the parts a person cannot
            # be asked to re-test — the aim, the promotion, the payoff
            # detector, the read-back — are exercised on every machine
            # that can run a compositor, including one with no X server.
            print("\ndriving the gesture:")
            watch = yc.LogTail(session.log_path)
            watch.catch_up()

            class _NoCamera:
                """The counter's shape, with nothing behind it."""
                started_at = time.time()
                frames: list = []

            self_test_drag(session, session.log_path)
            outcome = wait_for_the_drop(watch, _NoCamera(), args.timeout,
                                        1.0, max(4000, len(png) // 2))
            session.snapshot(os.path.join(args.out, "dropped.png"))
            print(f"  outcome: {outcome}")
            if outcome == "landed":
                carried, size_bytes = yc.image_is_in_the_document(
                    session, room, png)
                print(f"  in the document: {carried}"
                      + (f" ({size_bytes} bytes)" if size_bytes else ""))
                ready = ready and carried
            else:
                ready = False
            return 0 if ready else 1

        # ---- countdown, while the terminal is still in front -------
        if args.self_test:
            print("\n  --self-test: no hands. The gesture is driven from "
                  "here, aimed by pressing and reading back what the app "
                  "says it grabbed, so it cannot claim a row it did not "
                  "hit.")
        countdown(max(0, args.countdown), instruction,
                  quiet=args.self_test)

        # ---- roll -------------------------------------------------
        if wid is not None:
            raise_window(wid)
        if not args.self_test:
            notify(f"Recording. {instruction}")
        recorder = yc.Recorder([session], record_dir, interval=0.0,
                               dwell=0.0, cut_dwell=0.0,
                               max_hold=args.max_hold)
        yc.RECORDER = recorder
        # Start reading the log from here, so nothing the setup logged is
        # mistaken for something he did.
        watch = yc.LogTail(session.log_path)
        watch.catch_up()
        recorder.start(session.name)
        if args.self_test:
            self_test_drag(session, session.log_path)
        # Half the photograph's own size: a keystroke's delta is a few
        # hundred bytes and the file is ~150 KB, so there is nothing in
        # between for this to confuse.
        outcome = wait_for_the_drop(watch, recorder, args.timeout,
                                    args.dwell, max(4000, len(png) // 2))
        recorder.stop()
        if not args.self_test:
            notify("Take finished. Writing the clip.", urgency="low")

        # ---- was it really in the document? -----------------------
        if outcome == "landed":
            carried, size_bytes = image_is_in_the_document(session, room, png)
            if carried:
                print(f"  proved: the app answered with a {size_bytes} byte "
                      f"document carrying the image's own bytes")
            elif size_bytes:
                print(f"  the app answered with {size_bytes} bytes and the "
                      f"image's bytes are not among them")
            else:
                print("  the app did not answer the late-join handshake, so "
                      "the log is the only account of the image landing")
    except Exception as exc:  # noqa: BLE001 — the report is the product
        import traceback
        traceback.print_exc()
        outcome = f"{type(exc).__name__}: {exc}"
        rc = 1
    finally:
        if recorder is not None:
            recorder.stop()
        if args.keep:
            print(f"\n--keep: session left running (pid "
                  f"{session.proc.pid if session.proc else '?'}), "
                  f"QOS_TEST_CONTROL={session.sock}")
            if not args.windowed:
                print("  its window is fullscreen; close it with the pid "
                      "above when you are done. It is deliberately not "
                      "resized back — a resize is what crashes the winit "
                      "backend, and a run that filmed something should not "
                      "risk anything on the way out.")
        else:
            # The window goes with the process, so your desktop comes
            # back without anything being resized.
            session.stop()
            try:
                os.unlink(session.sock)
            except FileNotFoundError:
                pass
            for helper in (frontend, bus):
                if helper is not None and helper.poll() is None:
                    helper.terminate()
        if terminal:
            raise_window(terminal)

    # ---- the clip -------------------------------------------------
    print()
    video = None
    if recorder is not None and recorder.frames:
        recorder.report()
        first = png_size(os.path.join(record_dir, recorder.frames[0][1]))
        last = png_size(os.path.join(record_dir, recorder.frames[-1][1]))
        if first and last and first != last:
            print(f"  ! the window changed size mid-take ({first} to {last}); "
                  f"the clip will be built at {first[0]}x{first[1]} and the "
                  f"rest will not line up. Re-run without touching the window.")
        source_size = f"{first[0]}x{first[1]}" if first else "1600x900"
        try:
            print(f"\ncomposing {video_path}")
            video = recorder.compose(video_path, args.fps, source_size)
        except Exception as exc:  # noqa: BLE001
            print(f"  composing failed: {exc}", file=sys.stderr)
            print(f"  the frames are still on disk: {record_dir}/frames",
                  file=sys.stderr)
            rc = 1

    print("\n" + "=" * 70)
    print(f"outcome: {outcome}")
    if outcome == "no-drag":
        print(f"  nothing was ever picked up, so nothing was filmed but two "
              f"windows sitting there. The row to press on is the "
              f"highlighted {yc.PHOTO_NAME} in the left window; press on it "
              f"and keep the button down while you move, or it is a click "
              f"and the row is only selected again. Run it again.")
        rc = 1
    elif outcome == "no-drop":
        print(f"  it was picked up — the chip with the file's name was on "
              f"screen — but nothing reached the document before the "
              f"{args.timeout:.0f}s ran out. Either it was let go outside "
              f"the page, or it was let go over the styles panel on the "
              f"right rather than the paper. Run it again; --timeout takes "
              f"a bigger number if you just needed longer.")
        rc = 1
    if video:
        print(f"video:   {video['path']}")
        print(f"         {video['width']}x{video['height']} {video['codec']} "
              f"{video['fps']}  {video['duration']:.1f}s  "
              f"{video['frames']} frames  "
              f"{video['bytes'] / 1024 / 1024:.1f} MiB")
        mean_ms, worst_ms = recorder.snapshot_ms()
        print(f"         captured at {recorder.rate():.2f} frames/s; one "
              f"snapshot cost {mean_ms:.0f} ms mean / {worst_ms:.0f} ms worst")
        print("         every frame is a real render of that session, in "
              "capture order; none repeated, none interpolated")
    return rc


if __name__ == "__main__":
    sys.exit(main())
