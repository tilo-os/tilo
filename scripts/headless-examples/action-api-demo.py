#!/usr/bin/env python3
"""Drive the shell through its action surface, with nothing rendered.

This exercises `actions_enumerate` / `actions_invoke` against a live
compositor and the production provider registry, without synthetic input.
Unit tests cover dispatcher contracts against a fixture; this checks the
live action surface and state changes.

Usage:
  ./action-api-demo.py [SOCKET]   (default $XDG_RUNTIME_DIR/tilo-session/action-api.sock)

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
import shutil
import subprocess
import sys
import tempfile
import time

from _control import call, runtime_socket_path

SOCKET = sys.argv[1] if len(sys.argv) > 1 else runtime_socket_path("action-api.sock")
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = (SCRIPT_DIR / ".." / ".." / "target" / "debug" / "shell").resolve()

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


def chrome_state() -> dict:
    resp = call(SOCKET, {"op": "state_dump"})
    return copy.deepcopy(resp["state"]["chrome"])


def main() -> int:
    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass

    # A throwaway home, so the run does not depend on whatever this
    # machine happens to have persisted. That matters more than it looks:
    # the capability gate resolves against the *active* workspace, and a
    # session that loads a workspace owned by someone else will correctly
    # show only the handful of resources anyone may see. Against leftover
    # state this check would then "pass" while measuring something else.
    #
    # `TILO_STATE_PATH` alone is not enough. It moves the workspace tree;
    # record documents live under `XDG_DATA_HOME` (see
    # `crates/session/shell/src/shell/room_records.rs`), so isolating only the former
    # leaves every run sharing one set of documents.
    state_dir = tempfile.mkdtemp(prefix="tilo-action-proof-")
    env = {
        **os.environ,
        "TILO_TEST_CONTROL": SOCKET,
        "TILO_STATE_PATH": os.path.join(state_dir, "state.json"),
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

        # 1. The surface exists and is non-trivial.
        print("\n[1] enumerate the exported surface")
        resp = call(SOCKET, {"op": "actions_enumerate", "query": ""})
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
        before = chrome_state()
        for _ in range(3):
            call(SOCKET, {"op": "actions_enumerate", "query": ""})
            call(SOCKET, {"op": "actions_enumerate", "query": "settings"})
        after = chrome_state()
        check(
            "chrome state unchanged after 6 enumerations",
            before == after,
            "identical" if before == after else "STATE MOVED",
        )

        # 3. Invoking by address moves real state — no input synthesised.
        print("\n[3] invoke by address, with no synthetic input")
        addr = f"{INVOKE_PROVIDER}:{INVOKE_ID}"
        pre = chrome_state()
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
            post = chrome_state()
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
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        try:
            os.unlink(SOCKET)
        except FileNotFoundError:
            pass
        shutil.rmtree(state_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
