#!/usr/bin/python3
# Pinned to /usr/bin/python3 on purpose: `gi` ships with Fedora's system
# python3-gobject and is not present in linuxbrew/pyenv pythons that may
# appear earlier on PATH.
"""Repeatable screenshot of a native Wayland client window on GNOME.

Why this exists: on GNOME Wayland, neither `wmctrl`/`xdotool` (XWayland only)
nor `org.gnome.Shell.Introspect.GetWindows` (access-denied for user clients)
can locate a Wayland window. The `xdg-desktop-portal` Screenshot captures the
full screen, not a specific window — GNOME's `ScreenshotWindow` is also
access-denied for D-Bus callers. The one API that works is the
`Window Calls Extended` GNOME Shell extension (org.gnome.Shell.Extensions.Windows),
which exposes `List` + `GetFrameRect` per-window.

This script:
  1. Optionally launches the target binary.
  2. Polls `Windows.List` until a window whose title contains --title is found.
  3. Fetches its frame rect via `GetFrameRect`.
  4. Captures the full screen via `xdg-desktop-portal` (non-interactive).
  5. Crops the PNG to the frame rect with ImageMagick.
  6. Writes the crop to --out and cleans up.

Required Fedora packages:
  - python3-gobject (for `gi` — system python only; linuxbrew's python lacks it)
  - ImageMagick (`magick` CLI)
  - GNOME Shell extension "Window Calls Extended" by hseliger
    (install: gnome-extensions install or from extensions.gnome.org,
     ID: window-calls-extended@hseliger.eu)

Usage:
    scripts/screenshot-app.py --launch ./target/debug/shell \
        --title "QuickUI App" --out /tmp/qd-shot.png
"""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from typing import Optional

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402


SHELL_DEST = "org.gnome.Shell"
WINDOWS_PATH = "/org/gnome/Shell/Extensions/Windows"
WINDOWS_IFACE = "org.gnome.Shell.Extensions.Windows"

PORTAL_DEST = "org.freedesktop.portal.Desktop"
PORTAL_PATH = "/org/freedesktop/portal/desktop"
PORTAL_SCREENSHOT_IFACE = "org.freedesktop.portal.Screenshot"
PORTAL_REQUEST_IFACE = "org.freedesktop.portal.Request"


def find_window(bus: Gio.DBusConnection, title_substr: str) -> Optional[dict]:
    """Return the first window whose title contains `title_substr`, or None."""
    try:
        result = bus.call_sync(
            SHELL_DEST,
            WINDOWS_PATH,
            WINDOWS_IFACE,
            "List",
            None,
            None,
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
    except GLib.Error as e:
        sys.stderr.write(
            f"Windows.List failed: {e.message}\n"
            "Is the 'Window Calls Extended' GNOME extension installed and enabled?\n"
        )
        return None
    (payload,) = result.unpack()
    for w in json.loads(payload):
        if title_substr in w.get("title", ""):
            return w
    return None


def get_frame_rect(bus: Gio.DBusConnection, win_id: int) -> dict:
    result = bus.call_sync(
        SHELL_DEST,
        WINDOWS_PATH,
        WINDOWS_IFACE,
        "GetFrameRect",
        GLib.Variant("(u)", (win_id,)),
        None,
        Gio.DBusCallFlags.NONE,
        -1,
        None,
    )
    (payload,) = result.unpack()
    return json.loads(payload)


def take_full_screenshot(bus: Gio.DBusConnection, timeout_ms: int = 15000) -> str:
    """Call the portal Screenshot method and return the resulting file URI."""
    sender = bus.get_unique_name().replace(":", "").replace(".", "_")
    token = "qd_shot_" + os.urandom(4).hex()
    request_path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"

    loop = GLib.MainLoop()
    result: dict = {}

    def on_response(_conn, _sender, _obj, _iface, _signal, params):
        response, vardict = params.unpack()
        if response == 0:
            result["uri"] = vardict.get("uri")
        else:
            result["error"] = f"response={response} vardict={vardict}"
        loop.quit()

    subscription = bus.signal_subscribe(
        PORTAL_DEST,
        PORTAL_REQUEST_IFACE,
        "Response",
        request_path,
        None,
        Gio.DBusSignalFlags.NONE,
        on_response,
    )

    try:
        bus.call_sync(
            PORTAL_DEST,
            PORTAL_PATH,
            PORTAL_SCREENSHOT_IFACE,
            "Screenshot",
            GLib.Variant(
                "(sa{sv})",
                (
                    "",
                    {
                        "handle_token": GLib.Variant("s", token),
                        "interactive": GLib.Variant("b", False),
                    },
                ),
            ),
            None,
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
        GLib.timeout_add(timeout_ms, lambda: (loop.quit(), False)[1])
        loop.run()
    finally:
        bus.signal_unsubscribe(subscription)

    if "error" in result:
        raise RuntimeError(f"portal Screenshot failed: {result['error']}")
    if "uri" not in result:
        raise RuntimeError("portal Screenshot timed out")
    return result["uri"]


def uri_to_path(uri: str) -> str:
    prefix = "file://"
    if not uri.startswith(prefix):
        raise RuntimeError(f"unexpected URI scheme: {uri}")
    return uri[len(prefix) :]


def crop(src: str, rect: dict, out: str) -> None:
    magick = shutil.which("magick") or shutil.which("convert")
    if not magick:
        raise RuntimeError("ImageMagick `magick` not found on PATH")
    geometry = f'{rect["width"]}x{rect["height"]}+{rect["x"]}+{rect["y"]}'
    subprocess.run(
        [magick, src, "-crop", geometry, "+repage", out],
        check=True,
    )


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Capture a native Wayland client window on GNOME."
    )
    ap.add_argument(
        "--launch",
        help="Command to launch before capturing. Pass the full command as one "
        "shell-quoted string, e.g. --launch './target/debug/shell'.",
    )
    ap.add_argument(
        "--title",
        default="QuickUI App",
        help="Substring of the window title to match (default: 'QuickUI App').",
    )
    ap.add_argument(
        "--out",
        default="/tmp/qd-shot.png",
        help="Output PNG path (default: /tmp/qd-shot.png).",
    )
    ap.add_argument(
        "--timeout",
        type=float,
        default=10.0,
        help="Seconds to wait for the window to appear (default: 10).",
    )
    ap.add_argument(
        "--settle",
        type=float,
        default=0.3,
        help="Extra seconds to wait after the window is found, to let it paint "
        "its first frame (default: 0.3).",
    )
    ap.add_argument(
        "--keep",
        action="store_true",
        help="Do not kill the launched process after capturing.",
    )
    args = ap.parse_args()

    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    proc: Optional[subprocess.Popen] = None
    if args.launch:
        proc = subprocess.Popen(
            args.launch,
            shell=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            preexec_fn=os.setsid,
        )

    try:
        deadline = time.monotonic() + args.timeout
        win = None
        while time.monotonic() < deadline:
            win = find_window(bus, args.title)
            if win:
                break
            time.sleep(0.15)
        if not win:
            sys.stderr.write(
                f"timeout: no window with title containing {args.title!r} within "
                f"{args.timeout:g}s\n"
            )
            return 2

        # Wait for the window to have non-zero geometry. On first launch
        # the window appears in List before it has been mapped & sized.
        rect = {"width": 0, "height": 0, "x": 0, "y": 0}
        while time.monotonic() < deadline:
            rect = get_frame_rect(bus, win["id"])
            if rect.get("width", 0) > 0 and rect.get("height", 0) > 0:
                break
            time.sleep(0.1)
        if rect["width"] == 0 or rect["height"] == 0:
            sys.stderr.write(
                f"window appeared but frame rect stayed 0x0 within {args.timeout:g}s\n"
            )
            return 3

        if args.settle > 0:
            time.sleep(args.settle)
        uri = take_full_screenshot(bus)
        src = uri_to_path(uri)
        crop(src, rect, args.out)

        print(f"wrote {args.out} ({rect['width']}x{rect['height']} @ "
              f"{rect['x']},{rect['y']}) from {src}")
        return 0
    finally:
        if proc and not args.keep:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
                proc.wait(timeout=3)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    sys.exit(main())
