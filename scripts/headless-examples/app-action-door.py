#!/usr/bin/env python3
"""Reach a running app's own operations over the control socket.

`action-api-demo.py` proves the shell's own chrome is drivable by address.
This proves the harder half: that an *app's* published resources — the
document it has open, the directory it is showing — are reachable the same
way, by a caller that never sees the screen and synthesises no input.

Usage:
  ./app-action-door.py [/tmp/nirios-app-door.sock]

Runs the same checks against two apps in turn, because a door that only
carries documents is not a door. The word processor answers with a
document and nullary actions; the file explorer answers with a directory
tree whose entries are addressed by absolute path and whose action
collects an argument.

What it proves, per app:

  1. A live session exports the focused app's own resources, addressed by
     the app's own ids — never by a screen position.
  2. Every address is machine-readable, and each row says what an
     invocation must carry. Nothing in a row can be fired.
  3. Enumeration is side-effect free — chrome state is byte-identical
     after enumerating repeatedly.
  4. Invoking one address by name is accepted, with no click and no
     keystroke, and an invocation that does not satisfy the declared
     parameter is refused before anything runs.
  5. An address that does not exist is refused with `unknown_action`
     rather than silently doing nothing.

Exits non-zero if any of those fail. Every assertion reads state back
from the compositor, never from the request that made the change.
"""
import copy
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent

from _control import call

SOCKET = sys.argv[1] if len(sys.argv) > 1 else "/tmp/nirios-app-door.sock"
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = (SCRIPT_DIR / ".." / ".." / "target" / "debug" / "shell").resolve()

# The provider every app-exported operation is addressed under. Its rows
# carry the app's own resource ids, so this is the only thing the script
# needs to know about the shell's side of the arrangement.
APP_PROVIDER = "app"

# What an enumerated row is allowed to contain. The point of the check is
# that it is all inert: no callback, no token, no handle, so surveying the
# surface cannot cause an effect even by mistake.
ROW_FIELDS = {"provider", "id", "kind", "title", "subtitle", "score", "params"}

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def chrome_state() -> dict:
    return copy.deepcopy(call(SOCKET, {"op": "state_dump"})["state"]["chrome"])


def app_rows() -> list[dict]:
    """Every row the focused app currently exports."""
    resp = call(SOCKET, {"op": "actions_enumerate", "query": ""})
    if "err" in resp:
        return []
    return [a for a in resp["answer"]["actions"] if a["provider"] == APP_PROVIDER]


def seeded_state(kind, owner: str) -> dict:
    """A session with one window group holding one tab of `kind`.

    A compositor started against a fresh state directory opens an empty
    workspace, and an empty workspace has no focused app — so without this
    the run would be measuring "nothing is open" and reporting it as
    "nothing is reachable". `owner` has to be the local user or the
    capability gate will correctly withhold the whole surface.
    """
    return {
        "schema_version": 1,
        "active": 3,
        "active_strip": {"3": 4},
        "active_group": {"4": 5},
        "collaborators": [],
        "room_authority": {},
        "room_peer_names": {},
        "room_share_floor": {},
        "workspaces": [
            {
                "id": 3,
                "name": "Home",
                "owner": owner,
                "room": "d4e986b0-882b-431b-8009-0da6a8b45da1",
                "collaborators": [],
                "shared_surfaces": {},
                "capabilities": [
                    {
                        "effect": "Grant",
                        "expiry": "Never",
                        "granted_by": None,
                        "id": "owner-3",
                        "scope": {"Room": "3"},
                        "subject": {"Principal": {"Person": owner}},
                        "verb": "Admin",
                    }
                ],
                "strips": [
                    {
                        "id": 4,
                        "name": "Main",
                        "window_groups": [
                            {
                                "id": 5,
                                "tabs": [
                                    {
                                        "id": 71,
                                        "title": "Open",
                                        "tile": {
                                            "AppPlaceholder": {
                                                "kind": kind,
                                                "label": "Open",
                                            }
                                        },
                                    }
                                ],
                                "active_tab": 71,
                                "width": "Normal",
                                "maximized": False,
                            }
                        ],
                    }
                ],
            }
        ],
    }


def start(kind) -> subprocess.Popen:
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass
    # A throwaway home, for the reason `action-api-demo.py` spells out: the
    # capability gate resolves against the *active* workspace, so a run
    # that loaded someone else's persisted workspace would correctly show
    # almost nothing while looking like a failure of the door.
    state_dir = tempfile.mkdtemp(prefix="qos-app-door-")
    state_path = os.path.join(state_dir, "state.json")
    with open(state_path, "w") as f:
        json.dump(seeded_state(kind, os.environ.get("USER", "quinten")), f)
    env = {
        **os.environ,
        "QOS_TEST_CONTROL": SOCKET,
        "QOS_STATE_PATH": state_path,
        "XDG_DATA_HOME": os.path.join(state_dir, "data"),
        "XDG_CACHE_HOME": os.path.join(state_dir, "cache"),
        "XDG_STATE_HOME": os.path.join(state_dir, "state"),
        "XDG_CONFIG_HOME": os.path.join(state_dir, "config"),
    }
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    return subprocess.Popen(
        [str(BINARY), "--backend", "headless"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        preexec_fn=die_with_parent,
    )


def stop(proc: subprocess.Popen) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass


def exercise(app_label: str, kind) -> None:
    print(f"\n=== {app_label} ===")
    proc = start(kind)
    try:
        deadline = time.time() + 20.0
        while time.time() < deadline:
            if os.path.exists(SOCKET):
                break
            time.sleep(0.2)
        else:
            check(f"{app_label}: socket appeared", False, "not within 20s")
            return
        time.sleep(1.0)  # let the first paint settle and the app seed its doc

        # 1. The focused app's own resources, by the app's own ids.
        print("\n[1] the running app's resources are exported")
        rows = app_rows()
        if not check(
            "the focused app exports addressable operations",
            bool(rows),
            f"{len(rows)} exported",
        ):
            # Nothing to assert against; passing the rest vacuously would
            # be worse than stopping here.
            return
        for row in rows[:5]:
            print(f"        {row['id']:<52} {row['title']}")
        if len(rows) > 5:
            print(f"        … and {len(rows) - 5} more")

        # 2. Addresses parse, and rows are inert.
        print("\n[2] addresses are machine-readable and carry no handle")
        parses = all(
            "#" in r["id"] and r["id"].split("#")[0] and r["id"].rsplit("#", 1)[1]
            for r in rows
        )
        check(
            "every address splits into (resource, action)",
            parses,
            "all parse" if parses else "some do not",
        )
        inert = all(set(r.keys()) <= ROW_FIELDS for r in rows)
        check(
            "an enumerated row carries no callback or token",
            inert,
            "data only" if inert else "unexpected field present",
        )
        wants_text = [r for r in rows if r.get("params")]
        print(
            f"        {len(wants_text)} of {len(rows)} declare an argument; "
            "the rest are nullary"
        )

        # 3. Looking must not mutate.
        print("\n[3] enumerating the app surface is side-effect free")
        before = chrome_state()
        for _ in range(4):
            app_rows()
        after = chrome_state()
        check(
            "chrome state unchanged after 4 enumerations",
            before == after,
            "identical" if before == after else "STATE MOVED",
        )

        # 4. Invoke by address, and refuse an invocation that does not fit.
        print("\n[4] invoke by address, with no synthetic input")
        target = rows[0]
        args = {"text": "renamed-by-the-door"} if target.get("params") else None
        req = {"op": "actions_invoke", "provider": APP_PROVIDER, "id": target["id"]}
        if args:
            req["args"] = args
        resp = call(SOCKET, req)
        if "err" in resp:
            check(f"invoke {target['id']}", False, f"refused: {resp['err']}")
        else:
            check(f"invoke {target['id']} accepted", True, resp["answer"].get("title", ""))

        if target.get("params"):
            bare = call(
                SOCKET,
                {"op": "actions_invoke", "provider": APP_PROVIDER, "id": target["id"]},
            )
            err = bare.get("err") or {}
            check(
                "an invocation missing the declared argument is refused",
                err.get("code") == "invalid_arguments",
                json.dumps(err) if err else "it was accepted",
            )

        # 5. An address that does not exist is refused, not ignored.
        print("\n[5] an unknown app address is refused")
        resp = call(
            SOCKET,
            {
                "op": "actions_invoke",
                "provider": APP_PROVIDER,
                "id": "not-an-app.nothing#never",
            },
        )
        err = resp.get("err") or {}
        check(
            "unknown address returns unknown_action",
            err.get("code") == "unknown_action",
            json.dumps(err) if err else "no error returned",
        )
    finally:
        stop(proc)


def main() -> int:
    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    # A document, then a directory tree. If anything in the door had
    # quietly assumed rich text, the second one is where it would show.
    exercise("word processor — a document", {"Other": "Word"})
    exercise("file explorer — a directory tree", "FileExplorer")

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}", file=sys.stderr)
        return 1
    print("All checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
