#!/usr/bin/env python3
"""Search for an app that is not installed, press Enter, watch it install.

The palette beat of the demo (plan/yc-demo.md, beat 3), driven headlessly
against the real compositor, the real local registry and the real
`qos-install`:

  1. `scripts/demo-registry.sh up` must be serving (it configures
     registries.toml and makes sure the word processor is NOT installed);
  2. this script spawns `shell --backend headless`, opens the machine-wide
     palette (Super+K), types "word", and asserts the top row is
     "Install & open Word Processor";
  3. Enter → an "Installing Word Processor…" notification appears;
  4. the download + `qos-install install` finish on a worker thread, the
     chrome drains the event, and `qos-install list` now shows the package;
  5. the chrome then launches it exactly as the Applications row would.
     Headless has a Wayland socket, so the launch either starts (a
     `shell: launched …` line) or reports why it could not — both are
     read back and printed; only the install is asserted.

Usage:
  ./scripts/headless-examples/install-from-palette.py [socket]

Exits non-zero if any step fails. Snapshots go to /tmp/install-from-palette-*.png.
"""
import os
import pathlib
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from qos_client import NiriosClient  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
QOS_INSTALL = REPO / "target" / "debug" / "qos-install"
PKG_ID = "dev.qos.word-processor"
ROW = "Install & open Word Processor"


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def installed() -> bool:
    out = subprocess.run([str(QOS_INSTALL), "list"], capture_output=True, text=True)
    return PKG_ID in out.stdout


def chrome(c: NiriosClient) -> dict:
    return c.state_dump()["chrome"]


def notifications(c: NiriosClient) -> list[dict]:
    return chrome(c)["notifications"]


def wait_for(pred, timeout: float, every: float = 0.5):
    deadline = time.time() + timeout
    while time.time() < deadline:
        got = pred()
        if got:
            return got
        time.sleep(every)
    return None


def main() -> None:
    sock = sys.argv[1] if len(sys.argv) > 1 else "/tmp/nirios-install.sock"
    if installed():
        fail(f"{PKG_ID} is already installed — run scripts/demo-registry.sh reset first")
    with NiriosClient.spawn_headless(sock) as c:
        # The catalog refresher fetches /entries on its own thread right
        # after boot; give it a moment before asking the palette.
        time.sleep(1.5)
        c.modifiers(logo=True)
        c.key("k")
        c.modifiers()
        time.sleep(0.4)
        c.type_text("word")
        time.sleep(0.8)
        pal = chrome(c)["palette"]
        labels = [r["label"] for r in pal["rows"]]
        print("palette rows:", labels[:6])
        top = next((r for r in pal["rows"] if r["id"] == pal["selected_id"]), None)
        c.snapshot("/tmp/install-from-palette-row.png")
        if top is None or not top["label"].startswith(ROW):
            fail(f"top row is {top!r}, expected {ROW!r} (rows: {labels[:6]})")
        print(f"OK  top row: {top['label']}")

        c.key("Enter")
        started = wait_for(
            lambda: [n for n in notifications(c) if n["title"].startswith("Installing Word Processor")],
            timeout=5,
        )
        if not started:
            fail(f"no 'Installing…' notification; have {notifications(c)}")
        print(f"OK  {started[0]['title']} — {started[0]['body']}")

        # Download (loopback) + verify + extract of a debug build is a few
        # hundred MB; allow for it.
        done = wait_for(installed, timeout=180, every=1.0)
        c.snapshot("/tmp/install-from-palette-done.png")
        if not done:
            fail(f"{PKG_ID} never appeared in `qos-install list`; notifications: {notifications(c)}")
        print(f"OK  {PKG_ID} is installed")

        # The launch follows on the next frame. Report what the chrome says
        # about it without asserting — headless has no GPU for the client.
        time.sleep(3.0)
        for n in notifications(c):
            print(f"    notification: {n['title']} — {n['body']} [{n['severity']}]")
        bad = [n for n in notifications(c) if n["title"].startswith("Couldn't install")]
        if bad:
            fail(f"install reported failure: {bad}")
    print("PASS")


if __name__ == "__main__":
    main()
