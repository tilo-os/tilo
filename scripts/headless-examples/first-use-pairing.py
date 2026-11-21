#!/usr/bin/env python3
"""Walk native device-link pairing through the first-use screen on scratch profiles."""

from __future__ import annotations

import json
import re
import subprocess
import sys
import time
from contextlib import suppress
from pathlib import Path
from tempfile import TemporaryDirectory

from real_apps import Failed, ScriptedSession, until

IDENTITY = Path("config/tilo/identity")
ROOT_KEY = Path("identity/user-identity/pubkey.bin")
CERTIFICATE = Path("identity/device/cert.bin")
OUTCOMES = ("refuse", "cancel", "back", "approve")


def target(session, name):
    return next((item for item in session.control({"op": "list_targets"})["targets"]
                 if item["name"] == name), None)


def click(session, name):
    last = None

    def settled():
        nonlocal last
        control = target(session, name)
        rect = control and control["rect"]
        ready = rect if rect == last else None
        last = rect
        return ready

    x, y, width, height = until(f"{name} to settle", settled, 15, 0.1)
    session.control({"op": "click_at", "x": x + width / 2, "y": y + height / 2})


def shot(session, output, name):
    session.control({"op": "snapshot", "path": str(output / (name + ".png"))})


def request_link(owner):
    owner.control({"op": "actions_invoke", "provider": "so.tilo.session",
                   "id": "session:invite\u001fshow-pair-device", "wait_ms": 20000})
    click(owner, "Copy link")
    link = until("a native link on the owner's clipboard",
                 lambda: owner.control({"op": "state_dump"})["state"].get("clipboard"), 5)
    if not link.startswith("tilo://pair-device?"):
        raise Failed("the owner did not copy a native pairing link")
    return link


def submit_link(phone, link, output, outcome):
    click(phone, "Use my existing Tilo identity")
    click(phone, "Device pairing link")
    if outcome == "approve":
        shot(phone, output, "pairing-field")
    phone.control({"op": "type", "clipboard": True, "text": link})
    phone.control({"op": "modifiers", "ctrl": True})
    try:
        phone.control({"op": "key_down", "key": "v"})
        phone.control({"op": "key_up", "key": "v"})
    finally:
        phone.control({"op": "modifiers"})
    click(phone, "Use pairing link")


def progress(path, state):
    if not path.exists():
        return None
    result = json.loads(path.read_text())
    if result["state"] == "failed" and state != "failed":
        detail = re.sub(r"tilo://\S+", "[pairing link]", result.get("error", ""))
        raise Failed(f"the requester failed: {detail}")
    return result if result["state"] == state else None


def requester_progress(phone, status, state):
    if phone.proc.poll() is not None:
        raise Failed("the first-use session exited")
    return progress(status, state)


def helper(phone):
    task = Path(f"/proc/{phone.proc.pid}/task/{phone.proc.pid}/children")
    for pid in task.read_text().split():
        try:
            args = (Path("/proc") / pid / "cmdline").read_bytes().split(b"\0")
        except FileNotFoundError:
            continue
        if b"--pair-device-link-stdin" in args:
            if any(b"tilo://pair-device?" in arg for arg in args):
                raise Failed("the pairing link reached the requester command line")
            return pid
    return None


def no_identity(profile):
    for path in (ROOT_KEY, CERTIFICATE, IDENTITY / "primary.json"):
        if (profile / path).exists():
            raise Failed(f"the first-use device installed {path} without approval")


def verify(owner, phone, outcome, output):
    link = request_link(owner)
    submit_link(phone, link, output, outcome)
    status = phone.profile / IDENTITY / ".nearby-enrollment-status.json"
    waiting = until("the private requester to report a verification code",
                    lambda: requester_progress(phone, status, "waiting_for_approval"), 15)
    code = waiting["verification_code"]
    if not re.fullmatch(r"\d{6}", code):
        raise Failed("the requester did not report six digits")
    approve = f"Approve {code}"
    until("the owner to show the same code", lambda: target(owner, approve), 15)
    child = until("the leased requester process", lambda: helper(phone), 5)
    no_identity(phone.profile)
    shot(phone, output, outcome + "-waiting")

    if outcome == "approve":
        click(owner, approve)
        until("the requester to report a completed install",
              lambda: requester_progress(phone, status, "complete"), 15)
        until("the ready screen to offer entry to the desktop",
              lambda: target(phone, "Continue to desktop"), 10)
        if (phone.profile / ROOT_KEY).read_bytes() != (owner.profile / ROOT_KEY).read_bytes():
            raise Failed("the device installed a different person's public key")
        if (phone.profile / CERTIFICATE).read_bytes() == (owner.profile / CERTIFICATE).read_bytes():
            raise Failed("the device retained the owner's device certificate")
    elif outcome == "refuse":
        click(owner, "Deny")
        until("the requester to report a refusal",
              lambda: requester_progress(phone, status, "failed"), 15)
        until("the refused requester to exit", lambda: not Path(f"/proc/{child}").exists(), 10)
        until("setup to clear the waiting controls", lambda: not target(phone, "Cancel"), 10)
        # The failure banner enters after the requester has been reaped.
        time.sleep(0.5)
        shot(phone, output, "refused")
        click(phone, "Back")
        until("Back to reopen the pairing field", lambda: target(phone, "Device pairing link"), 10)
        no_identity(phone.profile)
    else:
        if outcome == "cancel":
            click(phone, "Cancel")
            until("Cancel to revoke the requester", lambda: not Path(f"/proc/{child}").exists(), 10)
            shot(phone, output, "cancelled")
        click(phone, "Back")
        until("Back to revoke the requester", lambda: not Path(f"/proc/{child}").exists(), 10)
        until("Back to reopen the pairing field", lambda: target(phone, "Device pairing link"), 10)
        click(owner, approve)
        until("the late answer to dismiss the owner's question",
              lambda: not target(owner, "Deny"), 10)
        no_identity(phone.profile)
    shot(phone, output, outcome)
    print(f"PASS first-use device pairing: {outcome}", flush=True)


def main(binary, output, outcomes):
    if any(outcome not in OUTCOMES for outcome in outcomes):
        raise Failed("expected refuse, cancel, back or approve")
    output.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix="tl-pair-") as directory:
        root = Path(directory)
        env = {"TILO_IDENTITY_FILE_SECRETS": "1"}
        for outcome in outcomes:
            owner = ScriptedSession(binary, profile=root / (outcome + "-owner"),
                                    env=env, log=output / (outcome + "-owner.log"))
            subprocess.run([str(binary), "--provision-identity"], env=owner.env(), check=True)
            phone = ScriptedSession(binary, profile=root / outcome,
                                    env=env, log=output / (outcome + ".log"))
            with owner, phone:
                try:
                    verify(owner, phone, outcome, output)
                except Exception:
                    with suppress(OSError, Failed):
                        shot(phone, output, outcome + "-failed")
                    raise


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), sys.argv[3:] or OUTCOMES)
