#!/usr/bin/env python3
"""Reach a running app's own operations over the control socket.

`action-api-demo.py` proves the shell's own chrome is drivable by address.
This proves the harder half: that an *app's* published resources — the
document it has open, the directory it is showing — are reachable the same
way, by a caller that never sees the screen and synthesises no input.

Usage:
  ./app-action-door.py [/tmp/nirios-app-door.sock]

What it proves, in order:

  1. A live session exports the focused app's own resources, addressed by
     the app's ids rather than by anything positional.
  2. Every app address is machine-readable: it parses back into
     (resource, action), and says what an invocation must carry.
  3. Enumeration is side-effect free — chrome state is byte-identical
     before and after enumerating the app surface repeatedly.
  4. Invoking one app address by name changes what the next enumeration
     reports, with no click and no keystroke.
  5. An app address that does not exist is refused with `unknown_action`
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

# How many tabs to cycle through looking for one that hosts an app with a
# resource tree. The door serves the focused app, and a fresh session may
# open on a terminal, which exports nothing.
MAX_TAB_HOPS = 6

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def chrome_state() -> dict:
    resp = call(SOCKET, {"op": "state_dump"})
    return copy.deepcopy(resp["state"]["chrome"])


def app_rows() -> list[dict]:
    """Every row the focused app currently exports."""
    resp = call(SOCKET, {"op": "actions_enumerate", "query": ""})
    if "err" in resp:
        return []
    return [a for a in resp["answer"]["actions"] if a["provider"] == APP_PROVIDER]


def next_tab() -> None:
    """Move focus to the next tab, the way a person would."""
    call(SOCKET, {"op": "modifiers", "ctrl": True, "shift": False, "alt": False})
    call(SOCKET, {"op": "key", "key": "Tab"})
    call(SOCKET, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False})
    time.sleep(0.3)


def main() -> int:
    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass

    # A throwaway home, for the reason `action-api-demo.py` spells out: the
    # capability gate resolves against the *active* workspace, and a run
    # that loads someone else's persisted workspace would correctly show
    # almost nothing while looking like a failure of the door.
    state_dir = tempfile.mkdtemp(prefix="qos-app-door-")
    env = {
        **os.environ,
        "QOS_TEST_CONTROL": SOCKET,
        "QOS_STATE_PATH": os.path.join(state_dir, "state.json"),
        "XDG_DATA_HOME": os.path.join(state_dir, "data"),
        "XDG_CACHE_HOME": os.path.join(state_dir, "cache"),
        "XDG_STATE_HOME": os.path.join(state_dir, "state"),
        "XDG_CONFIG_HOME": os.path.join(state_dir, "config"),
    }
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    proc = subprocess.Popen(
        [str(BINARY), "--backend", "headless"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        preexec_fn=die_with_parent,
    )

    try:
        deadline = time.time() + 20.0
        while time.time() < deadline:
            if os.path.exists(SOCKET):
                break
            time.sleep(0.2)
        else:
            print("FAIL: socket did not appear within 20s", file=sys.stderr)
            return 1
        time.sleep(0.5)  # let the first paint settle

        # 1. Find a focused app that publishes something, then assert on it.
        print("\n[1] a running app's own resources are exported")
        rows = app_rows()
        hops = 0
        while not rows and hops < MAX_TAB_HOPS:
            next_tab()
            rows = app_rows()
            hops += 1
        if not check(
            "the focused app exports addressable operations",
            bool(rows),
            f"{len(rows)} exported after {hops} tab hop(s)",
        ):
            # Without a surface there is nothing for the rest to assert
            # against, and passing the remaining checks vacuously would be
            # worse than stopping.
            return 1
        for row in rows[:10]:
            print(f"        {row['id']:<52} {row['title']}")
        if len(rows) > 10:
            print(f"        … and {len(rows) - 10} more")

        # 2. Addresses are the app's own ids, and inert.
        print("\n[2] addresses are machine-readable and carry no handle")
        parsed_ok = all("#" in r["id"] and not r["id"].startswith("#") for r in rows)
        check(
            "every address splits into (resource, action)",
            parsed_ok,
            "all parse" if parsed_ok else "some do not",
        )
        inert = all(
            set(r.keys()) <= {"provider", "id", "kind", "title", "subtitle", "score", "params"}
            for r in rows
        )
        check(
            "an enumerated row carries no callback or token",
            inert,
            "data only" if inert else "unexpected field present",
        )
        described = [r for r in rows if r.get("params")]
        print(
            f"        {len(described)} of {len(rows)} declare an argument; "
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

        # 4. Invoking by address changes what the app reports next.
        #
        # Read back through a fresh enumeration rather than from the invoke
        # reply: the reply says what was asked for, and only a re-read says
        # what happened.
        print("\n[4] invoke an app address, with no synthetic input")
        target = next((r for r in rows if not r.get("params")), None)
        if target is None:
            check("a nullary app action to invoke", False, "every row wants an argument")
        else:
            resp = call(
                SOCKET,
                {"op": "actions_invoke", "provider": APP_PROVIDER, "id": target["id"]},
            )
            if "err" in resp:
                check(f"invoke {target['id']}", False, f"refused: {resp['err']}")
            else:
                check(f"invoke {target['id']} accepted", True, resp["answer"].get("title", ""))
                time.sleep(0.4)  # the executor runs after the response is written
                after_rows = app_rows()
                changed = [r["id"] for r in after_rows] != [r["id"] for r in rows]
                check(
                    "the app's exported surface reflects the change",
                    changed,
                    f"{len(rows)} -> {len(after_rows)} addresses",
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
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        try:
            os.unlink(SOCKET)
        except FileNotFoundError:
            pass

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}", file=sys.stderr)
        return 1
    print("All checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
