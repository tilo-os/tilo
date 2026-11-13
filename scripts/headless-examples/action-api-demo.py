#!/usr/bin/env python3
"""Drive the shell through its action surface, with nothing rendered.

This exercises `actions_enumerate` / `actions_invoke` against a live
compositor and the production provider registry, without synthetic input.
Unit tests cover dispatcher contracts against a fixture; this checks the
live action surface and state changes.

Usage:
  ./action-api-demo.py [SOCKET]   (default: a socket in the session's own runtime
                                   directory, on a scratch profile)

What it proves, in order:

  1. A live session exports a non-empty action surface.
  2. Enumeration is side-effect free — chrome state is byte-identical
     before and after enumerating.
  3. Invoking an address by `(provider, id)` changes chrome state,
     with no click and no keystroke.
  4. An address that does not exist is refused with `unknown_action`
     rather than silently doing nothing.

Exits non-zero if any of those fail. Every assertion reads state back
from the compositor, never from the request that made the change.
"""
import copy
import json
import os
import pathlib
import sys
import time

from real_apps import Failed, Refused, ScriptedSession

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = (SCRIPT_DIR / ".." / ".." / "target" / "debug" / "tilo-session").resolve()
SOCKET = sys.argv[1] if len(sys.argv) > 1 else None

# An overlay-opening action: the chrome flag it flips is exposed in
# `state_dump`, so the effect is observable from outside the process
# rather than inferred from a return value.
INVOKE_PROVIDER = "actions"
INVOKE_ID = "collaborators.open"
OVERLAY_FLAG = "collaborators"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def chrome_state(call) -> dict:
    """The chrome state a refusal-free `state_dump` answers."""
    return copy.deepcopy(call({"op": "state_dump"})["state"]["chrome"])


def main() -> int:
    try:
        session = ScriptedSession(BINARY, socket=SOCKET).start()
    except Failed as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("      run: cargo build -p tilo-session --bin tilo-session", file=sys.stderr)
        return 1

    def call(payload: dict) -> dict:
        """One control request; a refusal is an answer with an `err`, not a raise."""
        try:
            return session.control(payload)
        except Refused as error:
            return {"err": {"code": error.code, "message": error.message}}

    time.sleep(0.5)  # let the first paint settle

    try:
        # 1. The surface exists and is non-trivial.
        print("\n[1] enumerate the exported surface")
        resp = call({"op": "actions_enumerate", "query": ""})
        if "err" in resp:
            print(f"  FAIL  enumerate errored: {resp['err']}", file=sys.stderr)
            return 1
        answer = resp["answer"]
        actions = answer["actions"]
        check("a live session exports actions", len(actions) > 0, f"{len(actions)} exported")
        check(
            "count matches the returned list",
            answer["count"] == len(actions),
            f"count={answer['count']} len={len(actions)}",
        )

        # The surface must contain acts, not just things that can be
        # shown. Without this the check cannot tell a working gate from
        # one that hid everything an owner is entitled to do — which is
        # precisely what a session pointed at someone else's workspace
        # looks like.
        addresses = {f"{a['provider']}:{a['id']}" for a in actions}
        gated = "actions:window.new.terminal"
        check(
            "a gated act is visible to the workspace owner",
            gated in addresses,
            f"{gated} present" if gated in addresses else f"{gated} MISSING",
        )

        providers = sorted({a["provider"] for a in actions})
        print(f"        providers: {', '.join(providers)}")
        for entry in actions[:12]:
            addr = f"{entry['provider']}:{entry['id']}"
            print(f"        {addr:<44} {entry['kind']:<14} {entry['title']}")
        if len(actions) > 12:
            print(f"        … and {len(actions) - 12} more")

        # 2. Looking must not mutate.
        print("\n[2] enumeration is side-effect free")
        before = chrome_state(call)
        for _ in range(3):
            call({"op": "actions_enumerate", "query": ""})
            call({"op": "actions_enumerate", "query": "settings"})
        after = chrome_state(call)
        check(
            "chrome state unchanged after 6 enumerations",
            before == after,
            "identical" if before == after else "STATE MOVED",
        )

        # 3. Invoking by address moves real state — no input synthesised.
        print("\n[3] invoke by address, with no synthetic input")
        addr = f"{INVOKE_PROVIDER}:{INVOKE_ID}"
        pre = chrome_state(call)
        pre_flag = pre.get("overlays", {}).get(OVERLAY_FLAG)
        resp = call(
            SOCKET,
            {"op": "actions_invoke", "provider": INVOKE_PROVIDER, "id": INVOKE_ID},
        )
        if "err" in resp:
            check(f"invoke {addr}", False, f"refused: {resp['err']}")
        else:
            check(f"invoke {addr} accepted", True, resp["answer"].get("title", ""))
            time.sleep(0.4)  # the executor runs after the response is written
            post = chrome_state(call)
            post_flag = post.get("overlays", {}).get(OVERLAY_FLAG)
            check(
                f"chrome.overlays.{OVERLAY_FLAG} changed",
                pre_flag != post_flag,
                f"{pre_flag} -> {post_flag}",
            )

        # 4. An address that does not exist is refused, not ignored.
        print("\n[4] an unknown address is refused")
        resp = call(
            SOCKET,
            {
                "op": "actions_invoke",
                "provider": INVOKE_PROVIDER,
                "id": "definitely.not.a.real.action",
            },
        )
        err = resp.get("err") or {}
        check(
            "unknown address returns unknown_action",
            err.get("code") == "unknown_action",
            json.dumps(resp)[:120],
        )

        print()
        if failures:
            print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
            return 1
        print("All action-API checks passed against a live compositor.")
        return 0
    finally:
        session.stop()


if __name__ == "__main__":
    sys.exit(main())
