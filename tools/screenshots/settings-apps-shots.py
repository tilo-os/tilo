#!/usr/bin/env python3
"""Shoot Settings' Apps page beside a real session, in light and dark.

The page lists each app's declared outside capabilities with a switch, and it reads them from the
session (`workspace/access`): this starts the nested-session build's own session, installs Settings
and Mail from this tree, opens the page and writes a PNG per theme, so the shot shows what a person
sees with Mail's `smtp.send` listed.

It runs where the session build and the tilolang tool are, as an app-walk does:

  t exec --nested --needs nested-session --needs font-seed --fetch target/settings-shots \
    -- env TILO_NESTED_SESSION_BUILD=/target/nested-session \
           TILO_FONT_ASSET_CACHE=/target/font-seed TILO_FONT_ASSETS_OFFLINE=1 \
           python3 -I -B tools/screenshots/settings-apps-shots.py

`--build` makes the session build in the exec's own image, through t's compiler cache, instead of
needing the nested-session check's image chain:

  t exec --nested --needs font-seed --fetch target/settings-shots \
    -- env TILO_FONT_ASSET_CACHE=/target/font-seed TILO_FONT_ASSETS_OFFLINE=1 \
           python3 -I -B tools/screenshots/settings-apps-shots.py --build
"""

import json
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts/headless-examples"))

import real_apps  # noqa: E402
from real_apps import Session, address, prepare, tl_packages, until  # noqa: E402

BUILD = Path(os.environ.get("TILO_NESTED_SESSION_BUILD") or (REPO / "target/nested-session"))
real_apps.BUILD = BUILD
OUT = REPO / "target/settings-shots"
STATE = REPO / "target/settings-shots-state"
OUT.mkdir(parents=True, exist_ok=True)

# `tl_packages` looks for the tool beside the checkout; the session build makes one.
tool = BUILD / "bin/tilolang"
dest = REPO / "target/debug/tilolang"
dest.parent.mkdir(parents=True, exist_ok=True)
if tool.is_file() and not dest.exists():
    dest.symlink_to(tool)

prepare(STATE, apps="terminal", built="--build" in sys.argv[1:])
tapps = tl_packages(STATE, OUT, ["settings", "mail"])

# A cold profile founds the person through the device daemon before the session answers ready,
# which a loaded build node can take minutes over.
answered_until = real_apps.until


def patient_until(what: str, check, deadline: float = 60.0, step: float = 0.25):
    if what == "the session to start":
        deadline = 900
    return answered_until(what, check, deadline, step)


real_apps.until = patient_until
until = patient_until

session = Session(STATE).start(apps="terminal", fresh=True, tapps=tapps)
try:
    # A headless session rasterizes frames as it draws, so its control answers can wait behind a
    # frame on a first start: give every request the room a cold desktop needs.
    control = session.control
    session.control = lambda payload, timeout=120.0: control(payload, timeout)

    # The ready handshake comes before the daemon and shell a fresh home starts, and the action
    # catalogue answers a little after the desktop's controls do: ask again until it does.
    def rows(needle: str):
        try:
            return session.actions(needle)
        except Exception:  # noqa: BLE001 - the dispatch is not up yet, asked again
            return None

    def targets() -> list:
        try:
            return session.targets()
        except Exception:  # noqa: BLE001 - not up yet, asked again
            return []

    def windows() -> list:
        try:
            return session.windows()
        except Exception:  # noqa: BLE001 - not up yet, asked again
            return []

    def offered(needle: str, resource: str | None, action: str):
        def row():
            for found in rows(needle) or []:
                where = address(found)
                if where is not None and where[1] == action and (resource is None or where[0] == resource):
                    return found
            return None

        return until(f"the session to offer {action}", row, 180)

    until("the desktop's catalogue", lambda: rows("so.tilo.settings") is not None, 180)
    session.invoke_row(
        offered("so.tilo.settings", "app.so.tilo.settings", "open")
    )
    until(
        "Settings' window",
        lambda: any(window.get("app_id") == "so.tilo.settings" for window in windows()),
        180,
    )
    session.invoke_row(offered("open-pane", "settings.root", "open-pane"), {"page": "apps"})
    until(
        "the Apps page's capabilities",
        lambda: any(
            "Send email" in target.get("name", "") or "Allowed" in target.get("name", "")
            for target in targets()
        ),
        180,
    )
    for dark in (False, True):
        session.invoke_row(offered("set-dark-mode", None, "set-dark-mode"), {"on": dark})
        # A snapshot waits for the frame the session paints: the page is shot after the restyle.
        session.snapshot(OUT / f"settings-apps-{'dark' if dark else 'light'}.png")
finally:
    session.stop()

print(json.dumps({"pictures": sorted(str(path) for path in OUT.glob("*.png"))}))
