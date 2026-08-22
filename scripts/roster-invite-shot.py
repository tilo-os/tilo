#!/usr/bin/env python3
"""Photograph the top bar before and after a real peer joins by invite.

Two live compositor sessions. The host mints a signed invite through its
own command palette; the guest accepts it through its own. Nothing seeds
a roster and nothing is injected -- the only thing that changes between
the two photographs of the guest is that it accepted a token the host
signed.

No application is launched. An app is what makes a *cursor* cross, and a
debug-build app's first frame is minutes of shader compile; the roster
entry an accepted invite produces does not wait on any of that.

    roster-invite-shot.py --out DIR
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPTS = pathlib.Path(REPO) / "scripts"


def _load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


collab = _load("two_shell_app_collab", "two-shell-app-collab.py")
demo_home = _load("demo_home", "demo-home.py")
Session = collab.Session
step, claim, note = collab.step, collab.claim, collab.note


def roster_of(s) -> list:
    """Whatever the session reports as the active workspace's roster."""
    ws = s.active_workspace() or {}
    for key in ("collaborators", "roster", "peers"):
        if key in ws:
            return ws[key]
    return []


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--size", default="1600x900")
    ap.add_argument("--vulkan-icd", default=None)
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    out = args.out
    os.makedirs(out, exist_ok=True)
    shell_bin = os.path.join(REPO, "target/debug/shell")
    staged = demo_home.stage(pathlib.Path(out) / "home")
    keep = {k: v for k, v in staged.items() if not k.startswith("XDG_")}
    if args.vulkan_icd:
        keep["VK_DRIVER_FILES"] = args.vulkan_icd
        keep["VK_ICD_FILENAMES"] = args.vulkan_icd

    pid = os.getpid()
    host = Session("host", "alice", os.path.join(out, "host"),
                   os.path.join(collab.RUNTIME_BASE, f"ri-h{pid}"),
                   os.path.join(collab.RUNTIME_BASE, f"ri-h{pid}.sock"),
                   shell_bin, args.size)
    guest = Session("guest", "bob", os.path.join(out, "guest"),
                    os.path.join(collab.RUNTIME_BASE, f"ri-g{pid}"),
                    os.path.join(collab.RUNTIME_BASE, f"ri-g{pid}.sock"),
                    shell_bin, args.size)
    sessions = [host, guest]
    for s in sessions:
        inner = s.base_env
        s.base_env = (lambda inner=inner: {**inner(), **keep})

    try:
        step("1", "start two sessions")
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            claim(f"{s.name}'s broker is available",
                  s.chrome()["collab"]["available"], True)

        step("2", "photograph both top bars before anybody has joined")
        for s in sessions:
            r = roster_of(s)
            note(f"{s.name} roster before = {json.dumps(r)}")
            claim(f"{s.name} starts with an empty roster", r, [])
            shot = os.path.join(out, f"{s.name}-before-join.png")
            s.snapshot(shot)
            note(f"wrote {shot}")

        step("3", "the host mints a signed invite; the guest accepts it")
        host.run_palette_command("invite peer", expect_id="result:0")
        token = None
        deadline = time.time() + 60
        while time.time() < deadline:
            token = host.chrome().get("peer_sync", {}).get("invite_token")
            if token:
                break
            time.sleep(0.5)
        claim("the host minted an invite", bool(token), True)
        if not token:
            raise RuntimeError("no invite token; see the host log")
        host.key("Escape")

        guest.run_palette_command("connect to peer", expect_id="result:0")
        guest.type_text(token)
        guest.key("Enter")
        joined = False
        deadline = time.time() + 90
        while time.time() < deadline:
            st = guest.chrome().get("peer_sync", {}).get("status") or ""
            if st.startswith("Joined"):
                joined = True
                break
            time.sleep(0.5)
        claim("the guest joined", joined, True)
        room = host.active_workspace()["room"]
        claim("the guest is in the host's room",
              guest.active_workspace()["room"], room)
        note(f"room = {room}")

        step("4", "photograph the guest's top bar now")
        for s in sessions:
            s.key("Escape")
        time.sleep(1.5)
        after = roster_of(guest)
        note(f"guest roster after = {json.dumps(after)}")
        claim("the guest's roster has exactly the peer it joined",
              len(after), 1)
        shot = os.path.join(out, "guest-after-join.png")
        guest.snapshot(shot)
        note(f"wrote {shot}")
        host.snapshot(os.path.join(out, "host-after-join.png"))
    finally:
        if not args.keep:
            for s in sessions:
                s.stop()
            for s in sessions:
                shutil.rmtree(s.runtime_dir, ignore_errors=True)

    print()
    if collab.FAILURES:
        print(f"{len(collab.FAILURES)} claim(s) failed:")
        for f in collab.FAILURES:
            print(f"  - {f}")
        return 1
    print("every claim held.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
