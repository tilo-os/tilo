#!/usr/bin/env python3
"""Ask a live session why a window is still showing its placeholder.

Run it while the spinner is on screen:

    python3 scripts/why-is-it-loading.py

A window stays a placeholder until the compositor marks it *presented*,
and `ForeignFrameStore::mark_presented` refuses to do that until the
client's buffer **fills** its tile -- with a ten second backstop that
force-presents and logs a warning. So a permanent spinner is one of
three things, and the store can tell them apart where a screenshot
cannot:

  presented=false, buffer != rect   the client has not redrawn at the
                                    size the strip gave it. Opening a
                                    second window re-layouts the strip
                                    and resizes everything already in
                                    it, which is why this shows up on
                                    the second application and not the
                                    first.
  presented=false, buffer == rect   the fill test passes and it still
                                    has not latched -- nothing is
                                    querying `presented()`, so the
                                    backstop never runs either.
  presented=true                    the store is happy and whatever is
                                    on screen is a different bug.

`last_error` is printed because a client that failed to attach a buffer
is a fourth case that looks identical from the outside.
"""
import glob
import json
import os
import socket
import sys


def sessions():
    return sorted(glob.glob("/run/user/1000/qos-lv-*.sock"),
                  key=os.path.getmtime, reverse=True)


def ask(path, op):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(8)
    s.connect(path)
    s.sendall((json.dumps({"op": op}) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    return json.loads(buf.decode().splitlines()[0])


socks = sessions()
if not socks:
    sys.exit("no live qOS session found")

for path in socks:
    who = os.path.basename(path)
    try:
        got = ask(path, "foreign_frames")
    except Exception as exc:  # noqa: BLE001
        print(f"{who}: not answering ({exc})")
        continue
    frames = (got.get("foreign_frames") or {}).get("frames")
    if frames is None:
        print(f"{who}: {json.dumps(got)[:120]}")
        continue
    print(f"\n=== {who}: {len(frames)} window(s) ===")
    for fr in frames:
        b, r = fr.get("buffer") or {}, fr.get("rect") or {}
        bw, bh = b.get("w"), b.get("h")
        rw, rh = round(r.get("w", 0)), round(r.get("h", 0))
        fits = (bw, bh) == (rw, rh)
        state = ("presented" if fr.get("presented")
                 else ("OPENING (placeholder on screen)"
                       if fr.get("opening") else "not presented"))
        print(f"  {fr.get('app_id')}")
        print(f"    {state}   frames drawn: {fr.get('frame_seq')}")
        print(f"    buffer {bw}x{bh}   tile {rw}x{rh}"
              f"   {'match' if fits else '<-- MISMATCH'}")
        if fr.get("last_error"):
            print(f"    last_error: {fr['last_error']}")
        if not fr.get("presented"):
            print("    -> " + (
                "the client has not redrawn at the size the strip gave it"
                if not fits else
                "the fill test passes but it never latched"))
