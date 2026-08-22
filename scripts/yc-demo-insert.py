#!/usr/bin/env python3
"""One hands-on take: an image going into the document, clicked by a person.

Every other beat of the nine-beat film is driven over the compositor's
control socket. This one cannot be. The Insert-image control lives inside
the *word processor's* widget tree — the app carries it as
`a11y-insert-image` (`apps/word-processor/src/widget.rs:4231`) — and a
client's insides are not in the compositor's widget registry, so there is
no name to aim `click_named` at and a swept grid of coordinates lands on
inert chrome.

So a person clicks it, and this script does everything else.

    ./scripts/yc-demo-insert.py

What it does, in order: stages a home with an image in it, raises a D-Bus
session and a portal frontend of its own so file dialogs reach qOS at
all, starts one compositor **in a window on your desktop** so a real
mouse reaches it, sizes that window to 1920x1080, launches the word
processor, waits for it to paint and focuses it so your first click is
not spent on the focus, counts down in the terminal, brings the window to
the front, records, watches the compositor's own log for the chooser
opening and then for the image reaching the document, holds a few seconds
so the landing is on film, stops by itself, gives you your desktop back,
asks the running app whether the image is really in its document, and
writes the clip.

Re-runnable. Every run is its own directory, its own sockets and its own
timestamped video, so a bad take costs nothing but running it again.

  Windowed, not headless — and why
  -------------------------------
  A person cannot click on a headless compositor. `--backend winit` puts
  the compositor in an ordinary window, and because the session's
  XDG_RUNTIME_DIR is its own (it has to be: the collab broker binds
  inside it), winit is pointed at X11 rather than at the host's Wayland
  socket — DISPLAY and XAUTHORITY are absolute and survive the isolation
  where WAYLAND_DISPLAY does not. It also leaves the window addressable
  by `wmctrl` and `xdotool`, which is how it is put out of the way while
  this sets up and brought back when the camera rolls.

  The frames are *not* a screen grab. They come off the same `snapshot`
  op the nine-beat recorder uses — the compositor re-renders its own
  scene and hands back a PNG (`native.rs:992`, explicitly written to work
  for every backend) — so a frame here is the compositor's own render at
  1:1, not a capture of a window with somebody's panel in the corner.
  Nothing is interpolated and no frame is repeated.

  What that costs: the compositor draws no pointer cursor into its own
  scene, so **your mouse pointer is not in the video**. The clicks are
  legible from what the UI does — the chooser opening, the row
  highlighting, the image appearing — but there is no arrow on screen.

  Why the window is never resized
  -------------------------------
  The winit backend panics when its surface is reconfigured with a frame
  in flight — `Surface::configure` finds a live `SurfaceOutput` and the
  present thread goes down, taking the session with it. Twice, on the way
  to this script, a run died exactly there. So nothing here resizes
  anything: the window is *created* at 1920x1080 (`NIRIOS_WINIT_SIZE`),
  it is shown fullscreen only to drop the title bar — which changes no
  dimension — and it is iconified and re-raised rather than resized while
  the rest is staged. At the end the session is stopped and the window
  goes with it.

  Proving it without a person
  ---------------------------
  `--self-test` runs the whole rig and, instead of waiting for you,
  sweeps the toolbar row for the image button, stopping the moment the
  compositor's log says a file dialog opened — so it cannot report a hit
  it did not get — then picks the file. It exercises every part of this
  script end to end. What it cannot tell you is how easy the button is to
  hit with a mouse, which is the one thing a person is for.
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

# The chooser is the shell's own `ui::widget::FilePicker`, mounted into
# the workspace overlay by `overlay_host.rs`. It carries a `file_picker`
# test id, but the overlay host draws it without registering a named
# target, so it never appears in `list_targets` — verified by diffing the
# registry either side of a dialog opening: 44 names before, the same 44
# after. What this script watches instead is the compositor's own log;
# see `SessionLog`.

# How long one unattended run will wait for a human before it gives up
# and writes whatever it filmed.
DEFAULT_TIMEOUT_S = 240.0
# Held after the image reaches the document, so its arrival is on film
# with room either side rather than one frame before the camera stopped.
DEFAULT_DWELL_S = 5.0


# --------------------------------------------------------------------
# a compositor in a window
# --------------------------------------------------------------------


def private_session_bus(run_dir: str) -> tuple[str, subprocess.Popen] | None:
    """A D-Bus session bus of this run's own.

    Not a nicety. The compositor serves the file-chooser portal by taking
    the well-known name `org.freedesktop.impl.portal.desktop.qos`, and a
    bus name has exactly one owner: the *first* compositor on that bus
    wins it and every later one logs

        portal: another process already owns
        org.freedesktop.impl.portal.desktop.qos — this session's file
        dialog will never be reached

    and then silently does nothing when an app asks for a file. On a
    machine where anyone has left a qOS session running — another agent's
    test, a demo from an hour ago — the take would fail at exactly the
    click it exists to film, with no error anywhere the person clicking
    can see. A bus of its own makes the run independent of whatever else
    is on the machine.

    It also keeps this session's `org.freedesktop.Notifications` off the
    desktop's bus, where it would otherwise be competing with GNOME for
    your notifications.
    """
    for daemon in ("/usr/bin/dbus-daemon", shutil.which("dbus-daemon")):
        if not daemon or not os.path.exists(daemon):
            continue
        proc = subprocess.Popen(
            [daemon, "--session", "--print-address", "--nofork"],
            stdout=subprocess.PIPE,
            stderr=open(os.path.join(run_dir, "dbus.log"), "w"),
            preexec_fn=yc.die_with_parent())
        yc.track(proc)
        address = proc.stdout.readline().decode(errors="replace").strip()
        if address:
            return address, proc
        proc.kill()
    return None


#: Where the OS image registers the compositor as a portal backend. Copied
#: into the run rather than read from `/usr/share`, which on a development
#: host carries gnome's and gtk's registrations and not this one.
QOS_PORTAL_FILE = os.path.join(REPO, "os", "usr", "share",
                               "xdg-desktop-portal", "portals", "qos.portal")
PORTAL_FRONTEND = "/usr/libexec/xdg-desktop-portal"
#: The desktop name `qos.portal` says its backend may be used in.
PORTAL_DESKTOP = "nirios"


def portal_frontend(run_dir: str, bus_address: str,
                    runtime_dir: str) -> subprocess.Popen | None:
    """Run an `xdg-desktop-portal` that routes file dialogs to qOS.

    An app does not ask the compositor for a file. It asks
    `org.freedesktop.portal.Desktop` — the portal *frontend* — which
    looks at the registered backends, picks one for the current desktop,
    and forwards. The compositor is only the backend
    (`org.freedesktop.impl.portal.desktop.qos`).

    On the real system `os/usr/share/xdg-desktop-portal/portals/
    qos.portal` is what tells the frontend that backend exists and may be
    used in `nirios`. A development host has never installed it: its
    frontend knows about gnome and gtk, so the word processor's Insert
    image would either fail or open a *GTK* dialog in a window of its
    own — outside the compositor, outside the recording, and not the
    thing the demo is about.

    So the run brings its own frontend up on its own bus, with a data dir
    holding exactly one registration — this one. Nothing else can be
    chosen, and the dialog that opens is the compositor's own chrome,
    inside the window being filmed.
    """
    if not (os.path.exists(PORTAL_FRONTEND) and os.path.exists(QOS_PORTAL_FILE)):
        return None
    data_dir = os.path.join(run_dir, "portal-data")
    portals = os.path.join(data_dir, "xdg-desktop-portal", "portals")
    os.makedirs(portals, exist_ok=True)
    shutil.copyfile(QOS_PORTAL_FILE, os.path.join(portals, "qos.portal"))

    # Name the backend outright rather than leaving it to be inferred.
    # `qos.portal` carries the deprecated `UseIn=` key, and the system
    # data dirs have to stay on the path (the frontend hard-fails with
    # "No GSettings schemas are installed" without them) — which puts
    # gtk.portal and gnome.portal back in view. This says which one wins
    # for every interface, so nothing here depends on how a particular
    # xdg-desktop-portal breaks a tie.
    config_home = os.path.join(run_dir, "portal-config")
    os.makedirs(os.path.join(config_home, "xdg-desktop-portal"), exist_ok=True)
    with open(os.path.join(config_home, "xdg-desktop-portal",
                           "portals.conf"), "w") as fh:
        fh.write("[preferred]\ndefault=qos\n"
                 "org.freedesktop.impl.portal.FileChooser=qos\n")

    system_data = os.environ.get(
        "XDG_DATA_DIRS", "/usr/local/share:/usr/share")
    env = dict(os.environ)
    env.update(
        DBUS_SESSION_BUS_ADDRESS=bus_address,
        XDG_CURRENT_DESKTOP=PORTAL_DESKTOP,
        XDG_DATA_DIRS=f"{data_dir}:{system_data}",
        XDG_CONFIG_HOME=config_home,
        XDG_RUNTIME_DIR=runtime_dir,
        G_MESSAGES_DEBUG="",
    )
    env.pop("WAYLAND_DISPLAY", None)
    proc = subprocess.Popen(
        [PORTAL_FRONTEND, "--replace"], env=env,
        stdout=open(os.path.join(run_dir, "portal.log"), "w"),
        stderr=subprocess.STDOUT, preexec_fn=yc.die_with_parent())
    yc.track(proc)
    return proc


def owns_name(bus_address: str, name: str, timeout: float = 20.0) -> bool:
    """Whether `name` has an owner on that bus, within `timeout`.

    The frontend takes a moment to claim its name, and a request made
    before it does is a request that fails with nothing on screen.
    """
    env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=bus_address)
    deadline = time.time() + timeout
    while time.time() < deadline:
        done = subprocess.run(
            ["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus",
             "--object-path", "/org/freedesktop/DBus", "--method",
             "org.freedesktop.DBus.NameHasOwner", name],
            capture_output=True, text=True, env=env)
        if "true" in done.stdout:
            return True
        time.sleep(0.4)
    return False


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


def app_tile(session) -> list | None:
    """The rect of the client tile on screen, if one is laid out."""
    name = next((n for n in session.target_names()
                 if "foreign-surface-" in n and not n.endswith("]")), None)
    return session.named_rect(name) if name else None


def focus_the_app(session) -> bool:
    """Click once into the document, so his click is not the one that
    focuses the window.

    The first click on an unfocused client is spent on the focus change
    — `route_input` latches the grab, raises the group and syncs keyboard
    focus — and does not reach the app's own widgets. Measured, not
    assumed: a click on the toolbar's image button did nothing at all
    until something had focused the window first. Doing it here means the
    person's first click is a click on the button.
    """
    rect = app_tile(session)
    if not rect:
        return False
    session.click_at(rect[0] + rect[2] * 0.45, rect[1] + rect[3] * 0.5)
    session.settle(1.2)
    return True


def self_test_click(session, log_path: str) -> None:
    """Stand in for the hand this script cannot supply.

    Aims at the toolbar's image button the only way anything can: at
    pixels. A client's widgets are not in the compositor's registry, so
    there is no name — the button is swept for along the toolbar row and
    the sweep stops the moment the compositor's own log says a file
    dialog opened. Self-validating: it cannot report a hit it did not get.

    The offsets come from captured frames — the toolbar row sits ~36px
    under the tile's top edge, and the image button is the one control on
    it that opens a file dialog, so nothing else on the row can produce a
    false positive. Other buttons it passes over on the way are
    formatting toggles; in a self-test that is noise, not damage.

    It runs on a thread of its own so the watch loop is already counting
    while these arrive, exactly as it would be with a person.
    """
    import threading

    # Its own reader: `SessionLog.read` consumes what it returns, so
    # sharing one with the watch loop would have the two threads stealing
    # each other's events.
    log = SessionLog(log_path)
    log.read()

    def drive() -> None:
        time.sleep(1.5)
        rect = app_tile(session)
        if not rect:
            return
        x0, y0, w, _h = rect
        for dy in (36, 30, 43, 24):
            for frac in [i / 40 for i in range(10, 33)]:
                session.click_at(x0 + w * frac, y0 + dy)
                time.sleep(0.5)
                log.read()
                if log.OPENED in log.text:
                    # The chooser is up. Pick the file the way a person
                    # would: the first row, then Open.
                    time.sleep(1.5)
                    session.click_at(422, 237)
                    time.sleep(0.8)
                    session.click_at(422, 237)
                    time.sleep(0.8)
                    session.key("Enter")
                    return
    threading.Thread(target=drive, daemon=True).start()


class SessionLog:
    """The compositor's own log, read as it is written.

    Two events in it are exactly what this script needs to watch, and
    both are the session reporting on itself rather than this script
    inferring anything from pixels:

    - `portal: file dialog opened` — the overlay host mounting the
      chooser (`overlay_host.rs:2986`). That is the click having landed.
    - `[app] broadcast doc delta <N>B` — the *word processor* saying its
      own document changed by N bytes. An image is stored in the document
      as its own bytes under a BLAKE3 hash, so inserting a 98 KB PNG
      makes a delta two orders of magnitude larger than a keystroke's.

    Watching the log rather than the widget registry is not a shortcut:
    the chooser does not appear in `list_targets` at all (verified — it
    is drawn by the overlay host without registering a named target), so
    the registry cannot see it, and a screenshot can only be looked at by
    a person. This is the compositor's own account of what it did.
    """

    OPENED = "portal: file dialog opened"
    DELTA = re.compile(r"broadcast doc delta (\d+)B")

    def __init__(self, path: str) -> None:
        self.path = path
        self.at = 0
        self.text = ""

    def read(self) -> str:
        """Everything appended since the last read."""
        try:
            with open(self.path, "rb") as fh:
                fh.seek(self.at)
                chunk = fh.read()
                self.at = fh.tell()
        except OSError:
            return ""
        fresh = chunk.decode(errors="replace")
        self.text += fresh
        return fresh

    def dialog_opened(self, fresh: str) -> bool:
        return self.OPENED in fresh

    def biggest_delta(self, fresh: str) -> int:
        return max((int(m) for m in self.DELTA.findall(fresh)), default=0)


def wait_for_the_click(log: SessionLog, recorder, timeout: float,
                       dwell: float, landed_bytes: int) -> str:
    """Watch until the image is in the document, then hold and stop.

    Three outcomes, kept apart because they are different facts: the
    image landed, the chooser opened but nothing arrived before the clock
    ran out (cancelled, or the pick did not take), or the chooser never
    opened at all and the click never landed.

    Reading a log file is not input: nothing in here touches the session,
    so the camera and the person have the compositor to themselves.
    """
    deadline = time.time() + timeout
    opened_at = None
    last_print = 0.0
    while time.time() < deadline:
        fresh = log.read()
        if opened_at is None and log.dialog_opened(fresh):
            opened_at = time.time()
            sys.stdout.write("\r  the file chooser is up — pick photo.png, "
                             "then Open            \n")
            sys.stdout.flush()
        if opened_at is not None and log.biggest_delta(fresh) >= landed_bytes:
            print(f"\n  the image is in the document; holding {dwell:.0f}s "
                  f"so it is on film")
            time.sleep(dwell)
            return "landed"
        now = time.time()
        if now - last_print >= 1.0:
            last_print = now
            state = ("picking the file" if opened_at
                     else "waiting for your click")
            sys.stdout.write(
                f"\r  recording {now - recorder.started_at:5.0f}s   "
                f"{len(recorder.frames):5d} frames   {state}   "
                f"({deadline - now:.0f}s left)   ")
            sys.stdout.flush()
        time.sleep(0.3)
    print()
    return "no-image" if opened_at else "no-click"


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
                    help="prove the rig with no hands: sweeps the "
                         "toolbar for the image button, stopping when the "
                         "compositor's log says a dialog opened, then picks "
                         "the file. Exercises the whole chain; only says "
                         "nothing about how easy the button is to hit")
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
    if not os.environ.get("DISPLAY"):
        print("no DISPLAY. This take needs a window you can click in, which "
              "needs an X server (XWayland counts). Run it from a terminal "
              "on your desktop rather than over ssh.", file=sys.stderr)
        return 2
    missing = [t for t in ("xdotool", "wmctrl", "ffmpeg", "ffprobe")
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
    home = yc.stage_home(os.path.join(run_dir, "home"))

    # The image, twice: once in the folder the story is about and once at
    # the top of the home, so it is in front of him whichever directory
    # the chooser opens in. Bigger than the nine-beat film's, because
    # here it has to read on screen from across a room.
    png = yc.a_real_png(640, 400)
    photo = os.path.join(home, "Demo", "photo.png")
    for path in (photo, os.path.join(home, "photo.png")):
        with open(path, "wb") as fh:
            fh.write(png)

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
    session = WindowedSession(
        "ada", "ada", run_dir,
        # Short: AF_UNIX caps sun_path near 108 bytes and a run directory
        # deep in a worktree blows straight past it.
        os.path.join(runtime, f"qos-ins-{pid}.sock"),
        shell_bin, "1600x900",
        os.path.join(runtime, f"qins{pid}"),
        home, apps_dir, tools)
    # Opened at the video's own size, so every frame goes into the clip
    # 1:1 and nothing is ever resampled — and so the window never has to
    # be resized, which is what crashes this backend.
    session.window_size = (None if args.windowed
                           else f"{yc.VIDEO_W}x{yc.VIDEO_H}")
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
    terminal = active_window()

    recorder = None
    wid = None
    outcome = "not reached"
    rc = 0
    instruction = ("Click the picture icon in the toolbar, "
                   "pick photo.png, press Open.")
    bus = None
    frontend = None
    try:
        # ---- stage ------------------------------------------------
        print("\nsetting up:")
        raised = private_session_bus(run_dir)
        if raised:
            session.bus_address, bus = raised
            print(f"  private session bus    pid {bus.pid} "
                  f"(the file-chooser portal has no rival on it)")
        else:
            print("  ! no dbus-daemon, so this session shares your desktop's "
                  "bus. If anything else already owns the qOS portal name, "
                  "the file chooser will never open — see the compositor log "
                  "for 'another process already owns'.")
        session.start()
        session.wait_ready(timeout=180.0)
        wid = find_window(session.proc.pid)
        if wid is None:
            raise RuntimeError(
                "the compositor started but never mapped an X window. "
                "Without one there is nothing to click on; see "
                f"{session.log_path}")
        print(f"  compositor up          pid {session.proc.pid}, "
              f"window {window_size(wid)}")

        # Fullscreen only to drop the title bar — the window was *created*
        # at the video's size, so this changes no dimension and the
        # backend now ignores a configure that changes nothing. Nothing
        # here resizes anything; that is the whole point.
        if not args.windowed:
            set_fullscreen(wid, True)
            time.sleep(1.0)
            if session.proc.poll() is not None:
                raise RuntimeError(
                    "the compositor went down while its window was being "
                    f"shown fullscreen; see {session.log_path}. --windowed "
                    "records without touching the window at all.")
        # Out of sight for the rest of the setup. It comes back — at the
        # size it already is — when the camera rolls.
        hide_window(wid)
        # The frontend, now the compositor owns the backend name it will
        # be routed to. Before the app launches, so the first thing it
        # asks for is answered.
        if session.bus_address:
            frontend = portal_frontend(run_dir, session.bus_address,
                                       session.runtime_dir)
            if frontend is None:
                print("  ! no xdg-desktop-portal frontend to run "
                      f"({PORTAL_FRONTEND}), so the app's Insert image has "
                      "nothing to ask. The chooser will not open.")
            elif owns_name(session.bus_address,
                           "org.freedesktop.portal.Desktop"):
                print(f"  file dialogs routed    to this compositor "
                      f"(frontend pid {frontend.pid})")
            else:
                print("  ! the portal frontend never claimed "
                      "org.freedesktop.portal.Desktop; the chooser will not "
                      f"open. See {os.path.join(run_dir, 'portal.log')}")

        # Your terminal back in front, so the rest of the setup and the
        # countdown can be read. A raise, never a resize.
        if terminal:
            raise_window(terminal)

        room = session.active_workspace()["room"]
        # The app has to be able to *read* what it is handed. The portal
        # gives a sandboxed caller a host path — the compositor says so
        # itself at startup, "the document store cannot reach sandboxed
        # apps" — and the word processor runs under bwrap, so without a
        # grant the pick completes and the app then logs "could not read
        # image ...: No such file or directory" and nothing arrives in
        # the document. Measured, not guessed: that is exactly what
        # happened until this line existed.
        #
        # Granted before the launch because the sandbox is built when the
        # app spawns; a grant made afterwards binds nothing. It is the
        # same production path a person's "allow" goes through
        # (`Workspace::grant_as`), scoped to the one staged folder.
        granted = session.grant(f"app:{yc.WORD_PROCESSOR}", f"fs:{home}",
                                "read")
        if "err" in granted:
            print(f"  ! the folder grant was refused ({granted['err']}); the "
                  f"app will not be able to read the file it is handed")
        else:
            print(f"  folder readable by it  {home}")
        painted = session.launch(yc.WORD_PROCESSOR, search="word")
        if painted is None:
            raise RuntimeError(
                "the word processor did not open, so there is no toolbar to "
                f"click. See {session.log_path}")
        print(f"  word processor up      surface "
              f"{painted.get('surface_id')} at frame {painted.get('frame_seq')}")
        # A window sharing the strip is a small target on camera. One
        # group, the full width of it.
        session.set_group_width(1000)
        session.settle(0.8)
        # Focused here, not by him: the click that focuses a window is
        # spent doing that and never reaches the app, so without this his
        # first click on the button would silently do nothing.
        print(f"  document focused       {focus_the_app(session)}")
        print(f"  image staged           {photo}")

        # What the recording will actually be made of — the compositor's
        # own render, which is the thing that matters, not the X server's
        # opinion of the window.
        probe = session.snapshot(os.path.join(args.out, "probe.png"))
        rendered = png_size(probe) or (1600, 900)
        print(f"  rendering at           {rendered[0]}x{rendered[1]}"
              + ("" if rendered == (yc.VIDEO_W, yc.VIDEO_H) else
                 f"  (not {yc.VIDEO_W}x{yc.VIDEO_H}: frames get scaled and "
                 f"letterboxed into the video)"))

        # ---- countdown, while the terminal is still in front -------
        if args.self_test:
            print("\n  --self-test: no hands. The toolbar row is swept for "
                  "the image button and the sweep stops when the log says a "
                  "dialog opened, so it cannot claim a hit it did not get.")
        countdown(max(0, args.countdown), instruction,
                  quiet=args.self_test)

        # ---- roll -------------------------------------------------
        raise_window(wid)
        if not args.self_test:
            notify(f"Recording. {instruction}")
        recorder = yc.Recorder([session], record_dir, interval=0.0,
                               dwell=0.0, cut_dwell=0.0,
                               max_hold=args.max_hold)
        yc.RECORDER = recorder
        # Start reading the log from here, so nothing the setup logged is
        # mistaken for something he did.
        watch = SessionLog(session.log_path)
        watch.read()
        recorder.start(session.name)
        if args.self_test:
            self_test_click(session, session.log_path)
        # Half the image's own size: a keystroke's delta is a few hundred
        # bytes, and the image is ~100 KB, so there is nothing in between
        # for this to confuse.
        outcome = wait_for_the_click(watch, recorder, args.timeout,
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
    if outcome == "no-click":
        print("  the file chooser never opened, so nothing was filmed but a "
              "document sitting there. The control is the one toolbar "
              "button with no text on it — a small photo icon, between the "
              "table control and the styles control. Run it again.")
        rc = 1
    elif outcome == "no-image":
        print(f"  the chooser opened but no image reached the document "
              f"before the {args.timeout:.0f}s ran out — cancelled, or the "
              f"pick did not take. Run it again; --timeout takes a bigger "
              f"number if you just needed longer.")
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
