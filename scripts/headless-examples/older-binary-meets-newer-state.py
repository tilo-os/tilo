#!/usr/bin/env python3
"""Show what this build does when it finds a state file a newer one wrote.

    ./scripts/headless-examples/older-binary-meets-newer-state.py [OUTDIR]

Runs the real compositor binary, headless, against a real state file whose
`schema_version` is one ahead of what it understands — which is exactly the
situation after a rollback across a schema change. Asserts, and leaves the
evidence in OUTDIR:

  1. the session comes up (refusing to boot a desktop over a file would be
     worse than starting one),
  2. the file on disk is byte-for-byte unchanged,
  3. an explanation is left beside it,
  4. and the refusal is ON SCREEN — a persistent error notification naming the
     file — not only in the journal.

(4) is the point. "Your workspaces are gone" and "your workspaces are on the
disk and this build declined to open them" look identical from the desk, and
only one of them is worth doing anything about. Before this, the compositor
logged a line and handed the person a bootstrapped demo tree.

Exit code 0 only if every assertion holds. A step that cannot look — no
compositor binary, no control socket, no screenshot — fails rather than being
skipped.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _control import call, require_ok, runtime_socket_path  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / "target" / "debug" / "shell"
OUT = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/newer-state").resolve()
# NOT inside OUT. AF_UNIX paths cap at ~108 bytes and the compositor refuses a
# longer one with "path must be shorter than SUN_LEN" — while carrying on
# running, so the failure looks like a compositor that never came up.
SOCK = os.environ.get("TILO_NEWER_STATE_SOCK") or runtime_socket_path("newer-state.sock")

failures = []


def ok(msg):
    print(f"ok    {msg}")


def bad(msg):
    print(f"FAIL  {msg}", file=sys.stderr)
    failures.append(msg)


def main():
    if not BINARY.exists():
        bad(f"no compositor at {BINARY} — run: cargo build -p shell --bin shell")
        return 1

    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)

    # A state file this build would otherwise load happily, with one field
    # changed. Derived from the shape the writer produces rather than
    # hand-rolled, so it stays a *loadable* file as the struct grows — a
    # fixture that silently stopped parsing would take the "unreadable" branch
    # and prove nothing about versions.
    state = OUT / "state.json"
    state.write_text(
        json.dumps(
            {
                "schema_version": 99,
                "workspaces": [],
                "active": 0,
                "active_strip": {},
                "active_group": {},
                "collaborators": [],
                "room_authority": {},
            },
            indent=2,
        )
    )
    before = state.read_bytes()

    env = dict(os.environ)
    env["TILO_TEST_CONTROL"] = SOCK
    env["TILO_STATE_PATH"] = str(state)
    # A session that writes into the real ~/.config/tilo would be testing the
    # developer's desktop, not this.
    env["TILO_FS_ACL_PATH"] = str(OUT / "fs-capabilities.json")
    # Its own runtime directory: the desktop this runs beside keeps its
    # sockets and session files.
    env["XDG_RUNTIME_DIR"] = tempfile.mkdtemp(prefix="tilo-rt-")
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)

    # A stale socket from a previous run would be "already there" and every
    # assertion below would be about a compositor that is not running.
    if os.path.exists(SOCK):
        os.unlink(SOCK)
    log = open(OUT / "compositor.log", "wb")
    proc = subprocess.Popen(
        [str(BINARY), "--backend", "headless"], env=env, stdout=log, stderr=log
    )
    try:
        for _ in range(60):
            if os.path.exists(SOCK):
                break
            if proc.poll() is not None:
                bad(
                    "the compositor exited before its control socket appeared; "
                    f"log in {OUT / 'compositor.log'}"
                )
                return 1
            time.sleep(0.5)
        else:
            bad("no control socket after 30s — nothing below can be observed")
            return 1
        ok("the session came up on a state file it declined to open")

        # 2. The file is untouched.
        if state.read_bytes() == before:
            ok("the state file is byte-for-byte what it was")
        else:
            bad("THE STATE FILE WAS MODIFIED — this is the data loss, not the fix")

        # 3. The explanation is beside it.
        beside = Path(str(state) + ".not-opened")
        if beside.exists():
            text = beside.read_text()
            if "99" in text:
                ok(f"an explanation naming layout 99 is at {beside.name}")
            else:
                bad(f"{beside.name} exists but does not say what happened: {text!r}")
        else:
            bad(f"no explanation left at {beside}")

        # 4. And it is on screen. This is the assertion that separates this
        #    change from the behaviour it replaces.
        dump = call(SOCK, {"op": "state_dump"}, timeout=10.0)
        chrome = dump.get("state", {}).get("chrome", {})
        (OUT / "chrome-state.json").write_text(json.dumps(dump, indent=2))
        notes = chrome.get("notifications")
        if notes is None:
            bad(
                "the chrome dump carries no `notifications` key, so whether "
                "anything is on the screen cannot be observed from here — and "
                "an assertion that cannot see its subject must not pass"
            )
        else:
            hit = [
                n for n in notes
                if "not opened" in n.get("title", "").lower()
                or "newer version" in n.get("body", "").lower()
            ]
            if not hit:
                bad(
                    "nothing on screen says the state file was refused. The "
                    f"person would see a bootstrapped desktop. Notifications: {notes}"
                )
            else:
                n = hit[0]
                ok(f"on screen: {n['title']!r}")
                if n.get("severity") == "Error":
                    ok("shown as an error, not as chatter")
                else:
                    bad(f"severity is {n.get('severity')!r}, not Error")
                if n.get("persistent"):
                    ok("and it waits for the person instead of sliding away")
                else:
                    bad("the notification auto-dismisses; a refusal has to persist")
                body = n.get("body", "")
                # A toast is 320x48 and does not grow, so the on-screen line
                # has to fit one. The path and the version numbers are in the
                # file left beside the state file, checked above.
                if len(body) < 80:
                    ok(f"the on-screen line fits a toast ({len(body)} chars)")
                else:
                    bad(
                        f"the body is {len(body)} chars and would spill off the "
                        f"card: {body!r}"
                    )
                if "nothing was changed" in body.lower():
                    ok("and it says the file is intact, which is the whole point")
                else:
                    bad(f"the screen does not say the file is intact: {body!r}")

        shot = OUT / "refusal.png"
        require_ok(call(SOCK, {"op": "snapshot", "path": str(shot)}, timeout=20.0))
        if shot.exists() and shot.stat().st_size > 0:
            ok(f"screenshot of what the person sees: {shot}")
        else:
            bad("the snapshot produced no image, so nothing here shows the screen")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()

    print()
    if failures:
        print(f"{len(failures)} assertion(s) failed", file=sys.stderr)
        return 1
    print(f"ALL ASSERTIONS PASSED (evidence in {OUT})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
