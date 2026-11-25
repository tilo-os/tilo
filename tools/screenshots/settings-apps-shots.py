#!/usr/bin/env python3
"""Shoot Settings' Apps page beside a real session, in light and dark.

The page lists each app's declared outside capabilities with a switch, and it reads them from the
session (`workspace/access`): this starts the nested-session build's own session, installs Settings
and Mail from this tree, opens the page and writes a PNG per theme, so the shot shows what a person
sees with Mail's `smtp.send` listed.

It runs where the session build and the tilolang tool are, as an app-walk does:

  t exec --nested --needs nested-session --needs font-seed \
    --fetch target/settings-shots \
    -- env TILO_NESTED_SESSION_BUILD=/target/nested-session \
           TILO_FONT_ASSET_CACHE=/target/font-seed TILO_FONT_ASSETS_OFFLINE=1 \
           python3 -I -B tools/screenshots/settings-apps-shots.py
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

prepare(STATE, apps="terminal")
tapps = tl_packages(STATE, OUT, ["settings", "mail"])

session = Session(STATE).start(apps="terminal", fresh=True, tapps=tapps)
try:
    session.launch("so.tilo.settings")
    row = until(
        "the App page's own action",
        lambda: next(
            (
                row
                for row in session.actions("open-pane")
                if address(row) == ("settings.root", "open-pane")
            ),
            None,
        ),
        60,
    )
    session.invoke_row(row, {"page": "apps"})
    until(
        "the Apps page's capabilities",
        lambda: any(
            "Send email" in target.get("name", "") or "Allowed" in target.get("name", "")
            for target in session.targets()
        ),
        90,
    )
    for dark in (False, True):
        mode = until(
            "the appearance mode's own action",
            lambda: next(
                (
                    row
                    for row in session.actions("set-dark-mode")
                    if address(row) is not None and address(row)[1] == "set-dark-mode"
                ),
                None,
            ),
            30,
        )
        session.invoke_row(mode, {"on": dark})
        # A snapshot waits for the frame the session paints: the page is shot after the restyle.
        session.snapshot(OUT / f"settings-apps-{'dark' if dark else 'light'}.png")
finally:
    session.stop()

print(json.dumps({"pictures": sorted(str(path) for path in OUT.glob("*.png"))}))
