#!/usr/bin/env python3
"""Three or more real sessions in one room, photographed together.

`two-shell-app-collab.py` proves a peer's cursor crosses between two
windows. It runs two sessions, so the watching session ever sees exactly
one other person — and one peer is the case where the overlay cannot go
wrong. Two names cannot collide when there is only one, a colour cannot
be confused with another when there is only one, and a pill cannot be
pushed off the window by a neighbour that does not exist.

This runs N sessions (default three), joins them all into the host's
workspace by signed invite, launches a real application into each, and
then parks each guest's pointer somewhere chosen to break the overlay:

  * two guests a handful of pixels apart, so their name pills want the
    same rectangle;
  * a guest hard against the right edge, where a pill anchored to their
    right would leave the window;
  * a guest low against the bottom edge, same thing downward.

Every peer on screen came off the wire from another process. Nothing
here seeds presence.

    scripts/many-peer-cursors.py
    scripts/many-peer-cursors.py --peers 4 --keep

Requires `cargo build -p shell -p word-processor`. No display.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import pathlib
import shutil
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPTS = pathlib.Path(REPO) / "scripts"


def _load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# The two-session harness already knows how to start a compositor, wait
# for its control socket, drive the palette and take a snapshot. Reuse it
# rather than growing a second copy that drifts.
collab = _load("two_shell_app_collab", "two-shell-app-collab.py")
demo_home = _load("demo_home", "demo-home.py")

Session = collab.Session
APPS = collab.APPS
step, claim, note = collab.step, collab.claim, collab.note

# Names are what end up on the pills, so pick a set that says something
# about the overlay: two that differ by a single letter, since a hash of
# a name is exactly where two people used to end up the same colour.
NAMES = ["alice", "bob", "dana", "danah", "carol", "erin"]


def guest_positions(rect: dict, n: int) -> list[tuple[float, float]]:
    """Where each guest parks, in scene coordinates.

    Constrained to `rect`, the guest's *own* application window, because
    a session only republishes a pointer when one of its local apps
    reports one. Park a guest over bare desktop and that guest publishes
    nothing and simply never appears -- which looks like the overlay
    dropping them rather than like the pointer being nowhere.

    Ordered so the first two guests are the crowding case and the ones
    after them go to the window's own edges.
    """
    x, y, w, h = rect["x"], rect["y"], rect["w"], rect["h"]
    cx, cy = x + w / 2.0, y + h / 2.0
    spots = [
        (cx, cy),                      # first guest: middle of the page
        (cx + 6.0, cy + 4.0),          # second: right on top of the first
        (x + w - 10.0, cy - 40.0),     # third: hard against the right edge
        (cx - 30.0, y + h - 10.0),     # fourth: against the bottom
        (cx + 12.0, cy - 8.0),         # fifth: back into the pile
    ]
    while len(spots) < n:
        k = len(spots)
        spots.append((cx + 20.0 * k, cy + 14.0 * k))
    return spots[:n]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--peers", type=int, default=3,
                    help="total sessions, host included (minimum 3)")
    ap.add_argument("--app", default="word-processor", choices=sorted(APPS))
    ap.add_argument("--out", default=None)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--size", default="1280x800")
    ap.add_argument("--host-app", action="store_true",
                    help="give the watching session an app too (it does not "
                         "need one to see its peers)")
    ap.add_argument("--vulkan-icd", default=None,
                    help="path to a single Vulkan ICD manifest to pin every "
                         "process at, e.g. /usr/share/vulkan/icd.d/"
                         "radeon_icd.x86_64.json")
    ap.add_argument("--frame-timeout", type=float, default=900.0,
                    help="seconds to wait for every app's first real frame")
    ap.add_argument("--shell-bin", default=os.path.join(REPO, "target/debug/shell"))
    args = ap.parse_args()

    if args.peers < 3:
        print("--peers must be at least 3; use two-shell-app-collab.py for two",
              file=sys.stderr)
        return 2
    if args.peers > len(NAMES):
        print(f"--peers caps at {len(NAMES)}", file=sys.stderr)
        return 2
    if not os.path.exists(args.shell_bin):
        print(f"missing {args.shell_bin}; run: cargo build -p shell", file=sys.stderr)
        return 2
    app_bin = os.path.join(REPO, "target", "debug", APPS[args.app]["bin"])
    if not os.path.exists(app_bin):
        print(f"missing {app_bin}; run: cargo build -p {APPS[args.app]['bin']}",
              file=sys.stderr)
        return 2

    width, height = (float(v) for v in args.size.split("x"))
    out = args.out or os.path.join(REPO, "target", f"many-peers-{os.getpid()}")
    os.makedirs(out, exist_ok=True)
    print(f"artefacts: {out}")

    # A staged HOME so the palette and the file provider index a small
    # known corpus rather than whoever is running this. XDG_DATA_DIRS is
    # deliberately left alone -- the Vulkan loader resolves its ICD
    # manifests through it, and a staged-only value leaves wgpu with no
    # adapter and the compositor unable to draw.
    staged = demo_home.stage(pathlib.Path(out) / "home")
    note(f"staged HOME at {staged['HOME']}")

    pid = os.getpid()
    sessions: list[Session] = []
    for i in range(args.peers):
        tag = "h" if i == 0 else f"g{i}"
        sessions.append(Session(
            NAMES[i] if i else "host",
            NAMES[i],
            os.path.join(out, NAMES[i]),
            # Short: a Unix socket address caps at ~108 bytes and the
            # broker lives at <runtime>/tilo/services/collab.sock. A
            # runtime dir under the repo overflows that, the broker fails
            # to bind, and every app then starts with collaboration
            # silently off.
            os.path.join(collab.RUNTIME_BASE, f"mp{tag}{pid}"),
            os.path.join(collab.RUNTIME_BASE, f"mp{tag}{pid}.sock"),
            args.shell_bin, args.size,
        ))
    host, guests = sessions[0], sessions[1:]

    # Every session opens the same document: the path is the record id,
    # which is the collaboration address.
    doc = None
    if APPS[args.app]["needs_doc"]:
        doc = os.path.join(out, "shared-report.md")
        with open(doc, "w") as f:
            f.write("Shared document.\n")

    # Fold the staged HOME into what each session is launched with. Only
    # HOME and the locale: the XDG dirs stay per-session, or the sessions
    # would share config and cease to be separate machines.
    keep = {k: v for k, v in staged.items() if not k.startswith("XDG_")}
    if args.vulkan_icd:
        # Pin the Vulkan loader at one driver, the way the launcher pins
        # an app it starts for real. Without it an app enumerates every
        # ICD on the box, and a machine with a second GPU whose userspace
        # is broken can leave one stalling in adapter enumeration --
        # which looks exactly like a slow first frame and is not one.
        keep["VK_DRIVER_FILES"] = args.vulkan_icd
        keep["VK_ICD_FILENAMES"] = args.vulkan_icd  # pre-1.3.207 loaders
        note(f"pinning the Vulkan loader at {args.vulkan_icd}")
    for s in sessions:
        inner = s.base_env
        s.base_env = (lambda inner=inner: {**inner(), **keep})

    try:
        step("1", f"start {len(sessions)} sessions")
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            c = s.chrome()["collab"]
            claim(f"{s.name}'s broker is available", c["available"], True)

        step("2", "join every guest to the host's workspace")
        for g in guests:
            token = None
            host.run_palette_command("invite peer", expect_id="result:0")
            deadline = time.time() + 30
            while time.time() < deadline:
                token = host.chrome().get("peer_sync", {}).get("invite_token")
                if token:
                    break
                time.sleep(0.5)
            # An invite names a room and an address and is signed; it is
            # not a one-shot nonce, so the host handing the same string to
            # a second guest is a legitimate outcome rather than a bug.
            claim(f"the host minted an invite for {g.name}", bool(token), True)
            if not token:
                raise RuntimeError("no invite token; see the host log")
            host.key("Escape")
            g.run_palette_command("connect to peer", expect_id="result:0")
            g.type_text(token)
            g.key("Enter")
            deadline = time.time() + 60
            joined = False
            while time.time() < deadline:
                st = g.chrome().get("peer_sync", {}).get("status") or ""
                if st.startswith("Joined"):
                    joined = True
                    break
                time.sleep(0.5)
            claim(f"{g.name} joined", joined, True)

        room = host.active_workspace()["room"]
        for g in guests:
            claim(f"{g.name} is in the host's room",
                  g.active_workspace()["room"], room)
        note(f"room = {room}")

        # Clear the invite panel off every screen while the chrome still
        # has keyboard focus. Once an app is clicked into, Escape goes to
        # the app and the panel stays up -- over the middle of the screen,
        # which is exactly where the peers are about to be.
        for s in sessions:
            s.key("Escape")
        time.sleep(0.5)
        for s in sessions:
            up = [k for k, v in (s.chrome().get("overlays") or {}).items() if v is True]
            claim(f"{s.name} has no overlay covering its windows", up, [])

        step("3", f"launch a real {args.app} into each guest's session")
        # Only the guests need an application. A session republishes a
        # pointer when one of *its* apps reports one, so an app is what
        # makes a guest visible; the host is only watching, and every
        # process it does not have to start is a debug-build shader
        # compile it does not have to wait through. Pass --host-app to
        # give the host one too.
        app_sessions = sessions if args.host_app else guests
        for s in app_sessions:
            s.launch_app(args.app, s.user, room, doc, out)
        # Two separate events, and both are required.
        #
        # Adoption is fast: the compositor takes the surface and lays it
        # into a tab within seconds. Presenting the first frame is not --
        # a debug build on a loaded machine can hold a window for many
        # minutes before it paints.
        #
        # Waiting only for the window is tempting and wrong. A surface
        # that has never committed a buffer is not mapped, so the pointer
        # never enters it, so the app never publishes, so the session has
        # nothing to republish and the host sees nobody. Measured: with
        # three adopted-but-unpainted windows the host's peer list came
        # back empty. So the paint is the thing to wait for; the window
        # is reported on the way past because it separates "the app never
        # started" from "the app started and is still compiling".
        deadline = time.time() + args.frame_timeout
        windowed: set[str] = set()
        pending = list(app_sessions)
        while pending and time.time() < deadline:
            for s in list(pending):
                frames = s.surfaces()
                if s.name not in windowed and any(f.get("rect") for f in frames):
                    windowed.add(s.name)
                    note(f"{s.name}'s app has a window; waiting for it to paint")
                if any(f.get("frame_seq", 0) > 0 for f in frames):
                    pending.remove(s)
            if pending:
                time.sleep(2.0)
        for s in app_sessions:
            frames = s.surfaces()
            claim(f"{s.name}'s app has a window on screen",
                  any(f.get("rect") for f in frames), True)
            claim(f"{s.name}'s app painted a real frame",
                  any(f.get("frame_seq", 0) > 0 for f in frames), True)
        if pending:
            raise RuntimeError(
                f"an app never painted within {args.frame_timeout}s -- an unpainted "
                f"window takes no pointer, so no peer would appear. Logs in {out}")

        step("4", "park each guest's pointer somewhere awkward")
        # The window every guest is pointing into. All the sessions lay
        # out the same single group, so one rect describes them all --
        # say so out loud rather than assuming it, because a peer's
        # pointer is published in the publisher's own scene space and two
        # sessions that disagree put a cursor over the wrong thing.
        rects = []
        for g in guests:
            r = next((s["rect"] for s in g.surfaces()
                      if s.get("app_id") == APPS[args.app]["package"] and s.get("rect")), None)
            claim(f"{g.name} knows where its own window is", bool(r), True)
            rects.append(r)
        if any(r is None for r in rects):
            raise RuntimeError("a guest has no window rect; see foreign_frames")
        agree = all(abs(r["x"] - rects[0]["x"]) < 40 and abs(r["w"] - rects[0]["w"]) < 40
                    for r in rects)
        claim("every guest has its window in the same place", agree, True)
        note(f"window rect = {rects[0]}")

        spots = guest_positions(rects[0], len(guests))
        for g, (x, y) in zip(guests, spots):
            note(f"{g.name} -> ({x:.0f}, {y:.0f})")
            # A pointer that does not move is not a move, and an app with
            # nothing to report says nothing -- so jitter into place and
            # let the gossip round trip land.
            for dx in (2.0, 0.0):
                g.pointer_move(x + dx, y)
                time.sleep(0.6)
        time.sleep(3.0)

        step("5", "ask the host who it would paint")
        seen = [p for p in host.peers() if p["room"] == room]
        users = sorted({p["user"] for p in seen})
        note(f"the host sees: {users}")
        claim("the host sees every guest at once", len(users), len(guests))
        for g in guests:
            claim(f"{g.name} is named, not a session handle",
                  any(u == g.user for u in users), True)
        for p in seen:
            claim(f"{p['user']} is in the shared room", p["room"], room)
        inside = [p for p in seen
                  if 0 <= p["x"] <= width and 0 <= p["y"] <= height]
        claim("every peer the host sees is on its screen", len(inside), len(seen))

        step("6", "photograph the host, and each guest for comparison")
        shots = []
        for s in sessions:
            shot = os.path.join(out, f"{s.name}-sees-{len(sessions) - 1}-peers.png")
            s.snapshot(shot)
            shots.append(shot)
            note(f"wrote {shot}")

        step("7", "walk one guest across the others and film it")
        frames_dir = os.path.join(out, "frames")
        os.makedirs(frames_dir, exist_ok=True)
        mover = guests[-1]
        start = spots[-1]
        finish = spots[0]
        n = 16
        for i in range(n + 1):
            t = i / float(n)
            mover.pointer_move(start[0] + (finish[0] - start[0]) * t,
                               start[1] + (finish[1] - start[1]) * t)
            time.sleep(0.4)
            host.snapshot(os.path.join(frames_dir, f"frame-{i:03d}.png"))
        video = os.path.join(out, "many-peer-cursors.h264")
        enc = subprocess.run(
            ["ffmpeg", "-y", "-framerate", "4", "-pattern_type", "glob",
             "-i", os.path.join(frames_dir, "frame-*.png"),
             "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20",
             "-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2", video],
            capture_output=True)
        if enc.returncode == 0:
            note(f"wrote {video}")
        else:
            note("ffmpeg failed: " + enc.stderr.decode()[-300:])
        # A peer that vanished mid-walk is the failure this films.
        end_users = sorted({p["user"] for p in host.peers() if p["room"] == room})
        claim("every peer is still there after the walk", len(end_users), len(guests))

    finally:
        if not args.keep:
            for s in sessions:
                s.stop()
            for s in sessions:
                shutil.rmtree(s.runtime_dir, ignore_errors=True)
        else:
            print("\n--keep: sessions left running. Control sockets:")
            for s in sessions:
                print(f"  {s.name}: {s.sock}")

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
