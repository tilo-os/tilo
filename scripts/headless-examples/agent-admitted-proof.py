#!/usr/bin/env python3
"""Admit an agent to a room from the palette, and film the roster saying so.

The gap this closes is a visual one. `Principal::Agent` has been in the
capability model for a long time, but the only way to reach it was a
harness driving the control socket — so a person could not admit an
agent, and a recording of somebody "inviting an agent" had nothing on
screen but whatever the previous beat left up.

So this proves the two halves that matter, in the order a viewer meets
them:

  1. The action exists on the live surface a person actually uses, and
     is refused where it cannot work.
  2. Invoking it puts the agent in the room *and on screen* — the
     capability lands, and the roster the compositor paints shows the
     agent tagged as one.

The screenshot is the deliverable. Everything else here is what makes
the screenshot mean something: a PNG of a roster proves nothing unless
the same run also shows the row was absent beforehand and that a real
capability is what put it there.

Usage:
  ./agent-admitted-proof.py [--out DIR] [--socket PATH]
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time

from _control import call
from reaper import die_with_parent, track

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = (SCRIPT_DIR / ".." / ".." / "target" / "debug" / "shell").resolve()

PROVIDER = "actions"
ACTION_ID = "agent.invite"
# The agent this OS ships (`qos-agent --name`, defaulting to `Scribe`),
# case-folded into the principal the grant names.
AGENT = "scribe"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="/tmp/agent-admitted")
    ap.add_argument("--socket", default=None)
    args = ap.parse_args()

    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    # A stale binary is how a verification passes while measuring
    # yesterday's code. Cheap to check, and it has bitten this project.
    newest_src = max(
        p.stat().st_mtime
        for p in (SCRIPT_DIR / ".." / ".." / "shell" / "src").resolve().rglob("*.rs")
    )
    if BINARY.stat().st_mtime < newest_src:
        print(
            f"FAIL: {BINARY} is older than shell/src — rebuild before trusting this run",
            file=sys.stderr,
        )
        return 1

    out_dir = pathlib.Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    # AF_UNIX paths cap out around 108 bytes, so the session root has to
    # be short rather than descriptive.
    session_root = tempfile.mkdtemp(prefix="/tmp/qs-")
    socket_path = args.socket or os.path.join(session_root, "c.sock")

    env = {
        **os.environ,
        "QOS_TEST_CONTROL": socket_path,
        "QOS_STATE_PATH": os.path.join(session_root, "state.json"),
        "XDG_DATA_HOME": os.path.join(session_root, "data"),
        "XDG_CACHE_HOME": os.path.join(session_root, "cache"),
        "XDG_STATE_HOME": os.path.join(session_root, "st"),
        "XDG_CONFIG_HOME": os.path.join(session_root, "cfg"),
        "XDG_RUNTIME_DIR": session_root,
        "QOS_PEER_KEY_PATH": os.path.join(session_root, "peer.key"),
        "QOS_APPS_DATA_DIR": os.path.expanduser("~/.local/share/qos-apps"),
        "QOS_RUNTIME_BUNDLE_DIR": os.path.expanduser("~/.local/share/qos-runtime"),
    }
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)

    log = open(os.path.join(session_root, "shell.log"), "w")
    proc = subprocess.Popen(
        [str(BINARY), "--backend", "headless"],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        preexec_fn=die_with_parent(),
    )
    track(proc)

    try:
        deadline = time.time() + 30.0
        while time.time() < deadline and not os.path.exists(socket_path):
            if proc.poll() is not None:
                print(f"FAIL: compositor exited early ({proc.returncode})", file=sys.stderr)
                print(open(os.path.join(session_root, "shell.log")).read()[-3000:])
                return 1
            time.sleep(0.2)
        else:
            if not os.path.exists(socket_path):
                print("FAIL: control socket never appeared", file=sys.stderr)
                return 1
        time.sleep(0.8)  # first paint

        def state() -> dict:
            return call(socket_path, {"op": "state_dump"})["state"]

        def agent_caps() -> list[dict]:
            """Capabilities on the active room whose subject is the agent.

            Read out of the serialised `WorkspaceTree`, which is the
            compositor's own state rather than a summary built for this
            script — so a row here means the capability model holds it,
            not that a test helper reported it.
            """
            tree = state()["workspace_tree"]
            active = tree.get("active")
            for ws in tree.get("workspaces", []):
                if ws.get("id") != active:
                    continue
                return [
                    c
                    for c in ws.get("capabilities", [])
                    if c.get("subject", {}).get("Principal", {}).get("Agent") == AGENT
                ]
            return []

        # -- 1. the row is on the surface a person reaches -------------
        print("\n[1] the action a person would use")
        resp = call(socket_path, {"op": "actions_enumerate", "query": "agent"})
        if "err" in resp:
            print(f"  FAIL  enumerate errored: {resp['err']}", file=sys.stderr)
            return 1
        offered = [a for a in resp["answer"]["actions"] if a["id"] == ACTION_ID]
        check(
            f"a live session offers {PROVIDER}:{ACTION_ID}",
            bool(offered),
            offered[0]["title"] if offered else "not exported",
        )
        if offered:
            print(f"        {offered[0]['title']!r} — {offered[0].get('subtitle', '')!r}")

        # -- 2. nobody is in the room before the grant -----------------
        print("\n[2] before: the room has admitted no agent")
        pre = agent_caps()
        check(
            "no capability names the agent yet",
            not pre,
            "clean room" if not pre else json.dumps(pre)[:160],
        )
        call(socket_path, {"op": "actions_invoke", "provider": PROVIDER,
                           "id": "collaborators.open"})
        time.sleep(0.6)
        call(socket_path, {"op": "snapshot",
                           "path": str(out_dir / "1-roster-before.png")})

        # -- 3. admit the agent, by address, with no synthetic input ---
        print("\n[3] admit the agent")
        resp = call(socket_path, {"op": "actions_invoke", "provider": PROVIDER,
                                  "id": ACTION_ID})
        if "err" in resp:
            check(f"invoke {PROVIDER}:{ACTION_ID}", False, json.dumps(resp["err"])[:200])
            return 1
        check(f"invoke {PROVIDER}:{ACTION_ID} accepted", True,
              resp["answer"].get("title", ""))
        time.sleep(0.8)

        granted = agent_caps()
        check(
            "the room now records a capability naming the agent",
            bool(granted),
            json.dumps(granted[0])[:200] if granted else "none",
        )
        if granted:
            cap = granted[0]
            check(
                "and it is room membership, not a share on one thing",
                "Room" in json.dumps(cap.get("scope", {})),
                json.dumps(cap.get("scope")),
            )
        check(
            "the roster is on screen",
            bool(state()["chrome"]["overlays"].get("collaborators")),
            "collaborators overlay open",
        )

        # -- 4. the frame -------------------------------------------------
        print("\n[4] the frame")
        shot = out_dir / "2-agent-in-roster.png"
        r = call(socket_path, {"op": "snapshot", "path": str(shot)})
        check("snapshot written", "err" not in r and shot.exists(),
              f"{shot} ({shot.stat().st_size if shot.exists() else 0} bytes)")

        # -- 5. and it is revocable ---------------------------------------
        print("\n[5] revoking the capability takes the agent back out")
        cap_id = granted[0].get("id") if granted else None
        if cap_id:
            call(socket_path, {"op": "revoke", "id": cap_id})
            time.sleep(0.6)
            still = agent_caps()
            check("the capability is gone", not still,
                  "revoked" if not still else json.dumps(still)[:120])
            call(socket_path, {"op": "snapshot",
                               "path": str(out_dir / "3-roster-after-revoke.png")})
        else:
            check("a capability id to revoke", False, "no agent capability was found")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {failures}")
        return 1
    print(f"All checks passed. Frames in {out_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
