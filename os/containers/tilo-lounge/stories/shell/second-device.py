#!/usr/bin/env python3
"""Pair Ada's new Tilo device from a local workspace with no web host."""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import PROFILE, Failed, Story, until, walk  # noqa: E402
from real_apps import BUILD  # noqa: E402

SHELL = "so.tilo.session"
SHOW_PAIR_DEVICE = "session:invite\u001fshow-pair-device"
SESSION = BUILD / "bin/tilo-session"
APPS = "chat tasks notes file-explorer settings activity"


def steps(story: Story) -> None:
    laptop = story.person("Ada’s laptop")
    phone = story.person("Ada’s phone")

    def laptop_opens():
        took = laptop.start(apps=APPS)
        laptop.name_self("Ada")
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        return {"tookMs": took["tookMs"]}

    story.step("Ada opens Tilo on her laptop", laptop_opens, people=[laptop])

    link: str

    def asks():
        nonlocal link
        laptop.session.invoke(SHELL, SHOW_PAIR_DEVICE, {})
        laptop.press("Copy link", "Copy link in Add a device", deadline=12)
        link = until("the Add a device link on the clipboard",
                     lambda: (laptop.session.state_dump() or {}).get("clipboard"), 3, 0.1)
        if not link.startswith("tilo://pair-device?"):
            raise Failed("Add a device did not put a native device link on the clipboard")
        story.note("pair-sheet-controls", laptop.names())

    story.step("Ada asks her laptop for a way to add a new device", asks, people=[laptop])

    # Hand the same link to the production requester on a device with a blank
    # identity. Keep the link private and out of the story's logs and arguments.
    phone.prepare(apps=APPS)
    link_file = phone.state / "device-link"
    joining: subprocess.Popen | None = None
    log_path = phone.logs / "native-enrollment.log"

    def shown_code() -> str | None:
        said = log_path.read_text(errors="replace")
        if joining is not None and joining.poll() not in (None, 0):
            raise Failed(f"The phone could not pair: {said.strip()}")
        found = re.search(r"^Pairing code: (\d{6})$", said, re.MULTILINE)
        return found.group(1) if found else None

    def adds_phone():
        nonlocal joining
        began = time.monotonic()
        link_file.touch(mode=0o600)
        link_file.write_text(link)
        with log_path.open("w") as log:
            joining = subprocess.Popen([
                str(SESSION), "--pair-device-link-file", str(link_file),
            ], stdout=log, stderr=subprocess.STDOUT, env={
                **os.environ,
                "HOSTNAME": "ada-phone",
                "XDG_CONFIG_HOME": str(phone.state / "config"),
                "TILO_IDENTITY_ROOT": str(phone.state / "identity"),
            })
        code = until("the phone to show a pairing code", shown_code, 5, 0.1)
        approval = f"Approve ada-phone with code {code}"
        def inline_request():
            names = laptop.names()
            if approval in names and "Deny ada-phone" in names:
                return names
            return None

        names = until("the phone's request in Add a device", inline_request, 12, 0.15)
        if {f"Approve {code}", "Device approval", "Waiting for a device to ask."} & set(names):
            raise Failed("the live request appeared beside another approval or an empty state")
        if (phone.state / "identity/device/cert.bin").exists():
            raise Failed("the phone was enrolled before Ada approved")
        story.note("pairing-code-matched", True)
        laptop.snapshot(story.out / "shots/03-pairing-approval-ada-s-laptop.png")

        laptop.press(approval, "Approve the phone in Add a device", deadline=5)
        if joining is None or joining.wait(timeout=10) != 0:
            raise Failed(f"The native requester did not complete: {log_path.read_text(errors='replace')}")
        laptop_identity = laptop.state / "identity"
        phone_identity = phone.state / "identity"
        root = "user-identity/pubkey.bin"
        cert = "device/cert.bin"
        if (phone_identity / root).read_bytes() != (laptop_identity / root).read_bytes():
            raise Failed("the phone joined a different person")
        if (phone_identity / cert).read_bytes() == (laptop_identity / cert).read_bytes():
            raise Failed("the phone reused the laptop’s certificate")
        elapsed = (time.monotonic() - began) * 1000
        took = phone.start(apps=APPS)
        if (phone_identity / root).read_bytes() != (laptop_identity / root).read_bytes():
            raise Failed("the phone’s shell opened under a different person")
        until("Ada’s name on the phone", lambda: phone.shows(f"Ada{PROFILE}"), 5, 0.1)
        return {"tookMs": elapsed + took["tookMs"]}

    try:
        story.step("Ada adds her phone as one of her devices", adds_phone, people=[laptop, phone])
    finally:
        if joining is not None and joining.poll() is None:
            joining.kill()
            joining.wait()
        link_file.unlink(missing_ok=True)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: second-device.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("second-device", Path(sys.argv[1]), steps))
