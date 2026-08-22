#!/usr/bin/env python3
"""Three qOS sessions in one room, and an agent that drives one of them.

Starts three real compositors — separate state directories, separate
libp2p keypairs, separate user identities, separate control sockets:

  ada  hosts the room and owns it
  bob    joins it by invite
  agent    joins it too, and from then on is driven *only* by the action
           API: semantic invocation by `(provider, id)`, never by a
           click, a keystroke, or a pixel

Three beats, about a minute:

  1. one room, three sessions   — proven by reading ada's room back
                                  out of bob's and the agent's own state
  2. the agent sees the surface — the shell exports a typed, addressable
                                  action catalogue, and enumerating it
                                  provably changes nothing
  3. the agent acts             — it invokes an action by address and the
                                  effect lands, with a counter proving no
                                  synthetic input was used

Every claim is read back from a session that did **not** make the change.
Where that is impossible with today's compositor the transcript prints a
GAP and moves on; it never prints a success it did not verify.

    scripts/yc-demo.py
    scripts/yc-demo.py --keep

Runs against target/debug/shell. There is no mock and no fixture path:
if the binary is missing the script exits rather than simulating it.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The action the agent invokes, as a stable `(provider, id)` address from
# the shell's own action provider — the same registry that feeds the
# command palette, reached with no palette and no keystroke.
AGENT_PROVIDER = "actions"
AGENT_ACTION_ID = "room.activity.open"
# Its effect, as a key under `chrome.overlays` in `state_dump`.
AGENT_ACTION_EFFECT = "room_activity"

# How long to wait for a change to cross between sessions.
SYNC_TIMEOUT_S = 25.0


# --------------------------------------------------------------------
# control socket
# --------------------------------------------------------------------


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """Send one command, read one reply."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("compositor closed the connection mid-reply")
            buf += chunk
        return json.loads(buf.split(b"\n", 1)[0])
    finally:
        s.close()


def ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor refused: {resp['err']}")
    return resp


class Session:
    """One running compositor, addressed by its control socket."""

    def __init__(self, name: str, user: str, run_dir: str, sock: str, binary: str,
                 size: str, runtime_dir: str):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        # Each session needs its own XDG_RUNTIME_DIR: the compositor
        # binds its collab broker at <runtime>/qos/services/collab.sock,
        # and two sessions sharing one runtime dir are two sessions
        # contending for one socket. Keep it short — sun_path is ~108 bytes.
        self.runtime_dir = runtime_dir
        self.proc: subprocess.Popen | None = None
        self.log_path = os.path.join(run_dir, "compositor.log")
        self._log = None
        # Every synthetic-input command this session has been sent. The
        # agent's whole claim is that it never uses one, and a counter is
        # a mechanical proof of that where a promise is not.
        self.input_ops = 0

    @property
    def collab_sock(self) -> str:
        return os.path.join(self.runtime_dir, "qos", "services", "collab.sock")

    # -- lifecycle ---------------------------------------------------

    def start(self) -> None:
        os.makedirs(os.path.join(self.dir, "config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "keys"), exist_ok=True)
        os.makedirs(self.runtime_dir, mode=0o700, exist_ok=True)
        env = dict(os.environ)
        # No display, and nothing inherited that would make the three
        # sessions share state.
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("QUICK_COLLAB_SOCK", None)
        env.pop("QUICK_COLLAB_CAPNP_SOCK", None)
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            QOS_USER_ID=self.user,
            QOS_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE=self.size,
            RUST_LOG=env.get("RUST_LOG", "info"),
        )
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--backend", "headless", "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
            # Its own session, so `--keep` actually keeps: otherwise the
            # children share this process's group and die with it the
            # moment the script returns, which made "left running" a
            # message printed about three processes that were already
            # gone. `stop()` still reaches them by pid.
            start_new_session=True,
        )

    def wait_ready(self, timeout: float = 90.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}")
            if os.path.exists(self.sock):
                # Bound is not the same as answering.
                try:
                    self.state()
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.2)
        raise RuntimeError(f"{self.name} never answered on {self.sock}")

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log:
            self._log.close()
            self._log = None

    # -- reading (never counts as input) -----------------------------

    def state(self) -> dict:
        return ok(call(self.sock, {"op": "state_dump"}))["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def active_workspace(self) -> dict:
        tree = self.tree()
        active = tree["active"]
        for ws in tree["workspaces"]:
            if ws["id"] == active:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace in the tree")

    def overlays(self) -> dict:
        return self.chrome()["overlays"]

    def palette(self) -> dict:
        return self.chrome()["palette"]

    # -- the action API (semantic; no input, no pixels) --------------

    def enumerate_actions(self, query: str = "") -> dict:
        """The exported action surface. Read-only by construction."""
        return ok(call(self.sock, {"op": "actions_enumerate", "query": query}))["answer"]

    def invoke_action(self, provider: str, action_id: str) -> dict:
        """Invoke one action by its stable address. Raw response, so a
        refusal is a result rather than an exception."""
        return call(self.sock, {"op": "actions_invoke",
                                "provider": provider, "id": action_id})

    # -- synthetic input (counted) -----------------------------------

    def key(self, name: str) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        self.input_ops += 1
        # One command per few hundred characters keeps each request under
        # the socket's line cap while still arriving as ordinary keys.
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    # -- painting ----------------------------------------------------

    def snapshot(self, path: str) -> str:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ok(call(self.sock, {"op": "snapshot", "path": path}))
        return path

    def settle(self, seconds: float = 0.5) -> None:
        """Let the compositor paint."""
        time.sleep(seconds)

    # -- palette (people only) ---------------------------------------

    def run_palette_command(self, query: str, expect_id: str | None = None) -> None:
        """Open the palette, type `query`, activate the top row."""
        self.modifiers(logo=True)
        self.key("k")
        self.modifiers()
        self.settle(0.35)
        self.type_text(query)
        self.settle(0.6)
        selected = self.palette()["selected_id"]
        if expect_id is not None and selected != expect_id:
            rows = [r["id"] for r in self.palette()["rows"]][:6]
            raise RuntimeError(
                f"{self.name}: palette query {query!r} selected {selected!r}, "
                f"expected {expect_id!r} (rows: {rows})")
        self.key("Enter")
        self.settle()


# --------------------------------------------------------------------
# transcript
# --------------------------------------------------------------------

FAILURES: list[str] = []
GAPS: list[str] = []

RULE = "-" * 70


def banner(title: str) -> None:
    print(f"\n{RULE}\n  {title}\n{RULE}", flush=True)


def step(text: str) -> None:
    print(f"\n  {text}", flush=True)


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    print(f"    [{'PASS' if good else 'FAIL'}] {description}", flush=True)
    if not good:
        print(f"           expected {expected!r}", flush=True)
        print(f"           observed {actual!r}", flush=True)
        FAILURES.append(description)


def gap(description: str) -> None:
    """Something this run could not verify. Its own mark, so it can never
    be mistaken for a pass."""
    print(f"    [GAP ] {description}", flush=True)
    GAPS.append(description)


def note(text: str) -> None:
    print(f"     .   {text}", flush=True)


def wait_for(fn, timeout: float = SYNC_TIMEOUT_S, interval: float = 0.25):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = fn()
        if last:
            return last
        time.sleep(interval)
    return last


def peer_id_of(s: Session) -> str:
    try:
        with open(s.log_path) as f:
            for line in f:
                # tracing paints field names and separators, so the
                # escapes come off before the split.
                plain = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if "local_peer_id=" in plain:
                    return plain.split("local_peer_id=")[1].split()[0].strip()
    except OSError:
        pass
    return "<unknown>"


def mint_invite(host: Session) -> str | None:
    host.run_palette_command("invite peer", expect_id="result:0")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=20)
    # Dismiss the card: it is a modal over the middle of the screen.
    host.key("Escape")
    host.settle(0.3)
    return token


def join_room(guest: Session, token: str, by_action_api: bool) -> bool:
    """Put `guest` into the room `token` names.

    `by_action_api` opens the connect prompt by invoking its stable
    address rather than by driving the palette. The token itself still
    has to be typed either way — the compositor has no semantic
    "join this room" op, which the caller records as a GAP.
    """
    if by_action_api:
        resp = guest.invoke_action(AGENT_PROVIDER, "peer.connect")
        if "err" in resp:
            raise RuntimeError(f"{guest.name}: peer.connect refused: {resp['err']}")
    else:
        guest.run_palette_command("connect to peer", expect_id="result:0")
    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=15):
        return False
    guest.type_text(token)
    guest.key("Enter")
    joined = wait_for(
        lambda: (guest.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=15)
    guest.key("Escape")
    guest.settle(0.3)
    return bool(joined)


# --------------------------------------------------------------------
# the demonstration
# --------------------------------------------------------------------


def beat_one(ada: Session, bob: Session, agent: Session, shots: str) -> dict:
    banner("1.  one room, three sessions")

    ids = {s.name: peer_id_of(s) for s in (ada, bob, agent)}
    for s in (ada, bob, agent):
        note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer {ids[s.name]}")
    claim("three separate processes",
          len({s.proc.pid for s in (ada, bob, agent)}), 3)
    claim("three separate libp2p identities", len(set(ids.values())), 3)

    host_ws = ada.active_workspace()
    note(f"ada's room {host_ws['room']}  named {host_ws['name']!r}")
    claim("ada owns his room", host_ws["owner"], ada.user)

    step("bob joins by invite")
    token = mint_invite(ada)
    claim("ada minted an invite token", bool(token), True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    claim("bob joined", join_room(bob, token, by_action_api=False), True)

    step("the agent joins by invite")
    token2 = mint_invite(ada)
    claim("ada minted a second token", bool(token2), True)
    if not token2:
        raise SystemExit("no second token: the agent cannot join")
    claim("the agent joined", join_room(agent, token2, by_action_api=True), True)
    # The connect *prompt* was opened by address; the token itself had to
    # be typed, because the control socket has no semantic join op.
    gap("joining has no semantic op — the invite token had to be typed "
        "(only the prompt was opened by address)")

    step("read ada's room back out of the other two sessions")
    # The verification that matters: neither of these is ada's own
    # state. Each guest independently reports the room it now holds.
    claim("bob's own state holds ada's room",
          bob.active_workspace()["room"], host_ws["room"])
    claim("the agent's own state holds ada's room",
          agent.active_workspace()["room"], host_ws["room"])
    claim("bob's own copy names ada as owner",
          bob.active_workspace()["owner"], ada.user)
    claim("the agent's own copy names ada as owner",
          agent.active_workspace()["owner"], ada.user)
    claim("bob's own copy carries ada's name for the room",
          bob.active_workspace()["name"], host_ws["name"])
    claim("the agent's own copy carries it too",
          agent.active_workspace()["name"], host_ws["name"])
    ada.snapshot(os.path.join(shots, "01-ada.png"))
    bob.snapshot(os.path.join(shots, "02-bob.png"))
    return host_ws


def beat_two(agent: Session, shots: str) -> list[dict]:
    banner("2.  the agent sees a typed, addressable surface")

    step("enumerate everything this session exports")
    surface = agent.enumerate_actions("")
    actions = surface["actions"]
    providers: dict[str, int] = {}
    for a in actions:
        providers[a["provider"]] = providers.get(a["provider"], 0) + 1
    note(f"{surface['count']} addressable actions across "
         f"{len(providers)} providers")
    for p, n in sorted(providers.items(), key=lambda kv: -kv[1]):
        note(f"  {p:20} {n}")
    claim("the catalogue is not empty", surface["count"] > 0, True)
    claim("count matches the rows returned", surface["count"], len(actions))

    step("the shell's own actions, as the agent receives them")
    shell_actions = [a for a in actions if a["provider"] == AGENT_PROVIDER]
    wi = max([len("ID")] + [len(a["id"]) for a in shell_actions])
    print(f"     {'ID'.ljust(wi)}  TITLE", flush=True)
    print(f"     {'-' * wi}  {'-' * 34}", flush=True)
    for a in shell_actions:
        print(f"     {a['id'].ljust(wi)}  {a['title']}", flush=True)

    step("every entry is addressed by (provider, id), never by position")
    claim("every entry carries provider, id, kind and title",
          all(isinstance(a.get(k), str) and a[k]
              for a in actions for k in ("provider", "id", "kind", "title")), True)
    addresses = [(a["provider"], a["id"]) for a in actions]
    claim("every address is unique", len(set(addresses)), len(addresses))
    claim("no entry carries a screen position",
          any(k in a for a in actions for k in ("rect", "x", "y")), False)

    step("enumerating is read-only: surveying cannot fire a side-effect")
    # `revision` is the chrome's own change counter — anything that
    # mutates chrome bumps it. If enumerating moved it, then asking
    # "what can I do here" would itself be doing something.
    before = agent.chrome()
    for query in ("", "room", "terminal"):
        agent.enumerate_actions(query)
    after = agent.chrome()
    claim("the chrome revision did not move", after["revision"], before["revision"])
    claim("no overlay opened", after["overlays"], before["overlays"])
    claim("the palette did not open", after["palette"]["visible"], False)
    agent.snapshot(os.path.join(shots, "03-agent-before.png"))
    return actions


def beat_three(ada: Session, bob: Session, agent: Session,
               actions: list[dict], shots: str) -> None:
    banner("3.  the agent acts, by address")

    baseline = agent.input_ops
    note(f"synthetic-input commands sent to the agent before this beat: {baseline}")

    step("pick one action out of the catalogue, by id")
    picked = next((a for a in actions if a["provider"] == AGENT_PROVIDER
                   and a["id"] == AGENT_ACTION_ID), None)
    claim(f"the catalogue offers ({AGENT_PROVIDER}, {AGENT_ACTION_ID})",
          picked is not None, True)
    if picked is None:
        raise SystemExit("the agent's chosen action is not exported")
    note(f"picked {picked['title']!r}  [{picked['kind']}]")

    step("invoke it — no click, no keystroke, no pixel")
    before_q, before_x = ada.overlays(), bob.overlays()
    claim("the effect is absent beforehand",
          agent.overlays()[AGENT_ACTION_EFFECT], False)
    resp = agent.invoke_action(AGENT_PROVIDER, AGENT_ACTION_ID)
    note(f"actions_invoke -> {json.dumps(resp)}")
    claim("the invocation was accepted", "answer" in resp, True)
    claim("the reply names the address that ran",
          (resp.get("answer", {}).get("provider"), resp.get("answer", {}).get("id")),
          (AGENT_PROVIDER, AGENT_ACTION_ID))
    claim("the reply names what it ran",
          resp.get("answer", {}).get("title"), picked["title"])

    step("the effect is real")
    landed = wait_for(lambda: agent.overlays()[AGENT_ACTION_EFFECT], timeout=8)
    claim("the action changed live state", bool(landed), True)
    agent.snapshot(os.path.join(shots, "04-agent-after.png"))

    step("no synthetic input was used to do it")
    claim("no keystroke, click or modifier reached the agent",
          agent.input_ops, baseline)
    note("the address (provider, id) is the entire input to the invocation")

    step("read the other two sessions back")
    # Cross-session verification of the agent's action. What is checkable
    # today is containment: the agent acting did not move ada's or
    # bob's screen, and did not touch the room they share.
    room = agent.active_workspace()
    claim("ada's overlays are untouched", ada.overlays(), before_q)
    claim("bob's overlays are untouched", bob.overlays(), before_x)
    claim("ada's copy of the room is unchanged",
          (ada.active_workspace()["name"], ada.active_workspace()["owner"]),
          (room["name"], room["owner"]))
    claim("bob's copy of the room is unchanged",
          (bob.active_workspace()["name"], bob.active_workspace()["owner"]),
          (room["name"], room["owner"]))
    # Say plainly what could not be shown, rather than dressing
    # containment up as propagation.
    note("the replicated room document carries name, owner and "
         "capabilities; window layout and chrome are per-session")
    gap("no enumerable action writes the replicated room document today, "
        "so an agent invocation cannot yet be shown *landing* in "
        "ada's or bob's session — only as contained to its own")


def run(ada: Session, bob: Session, agent: Session, shots: str) -> None:
    beat_one(ada, bob, agent, shots)
    actions = beat_two(agent, shots)
    beat_three(ada, bob, agent, actions, shots)


# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "yc-demo"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell", default=os.path.join(REPO, "target", "debug", "shell"),
                    help="path to the compositor binary")
    ap.add_argument("--size", default="1280x800", help="headless viewport")
    ap.add_argument("--keep", action="store_true",
                    help="leave the three sessions running afterwards, so they "
                         "can be driven by hand on camera")
    args = ap.parse_args()

    # No fallback and no fixture: this demonstration is only meaningful
    # against the real compositor, so a missing binary is an exit, never
    # a substitute.
    if not os.path.exists(args.shell):
        print(f"no compositor at {args.shell}\n"
              f"build it first:  cargo build -p shell --bin shell", file=sys.stderr)
        return 2

    # Always from a clean slate. Sessions persist their workspaces, so a
    # re-run over a previous run's state starts with rooms already joined
    # and a catalogue that no longer offers `peer.connect` — the demo
    # then fails on a stale precondition rather than on anything real.
    # `--keep` is about teardown, never about reusing state.
    shutil.rmtree(args.out, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    # Unix socket paths are capped near 108 bytes, so they go in the
    # runtime dir rather than next to the rest of the run.
    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    pid = os.getpid()
    sessions = [
        Session(name, name,
                os.path.join(args.out, name),
                os.path.join(runtime, f"qos-yc-{short}-{pid}.sock"),
                args.shell, args.size,
                # A private runtime dir per session, standing in for the
                # per-user one logind would give each real login.
                os.path.join(runtime, f"qyc-{short}{pid}"))
        for short, name in zip("qxa", ("ada", "bob", "agent"))
    ]
    ada, bob, agent = sessions

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")
    started = time.time()
    rc = 0
    try:
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            print(f"{s.name} ready (pid {s.proc.pid}, user {s.user})")
        run(ada, bob, agent, shots)
    except Exception as exc:  # noqa: BLE001 — the report is the product
        FAILURES.append(f"{type(exc).__name__}: {exc}")
        import traceback
        traceback.print_exc()
    finally:
        if args.keep:
            print("\n--keep: leaving the three sessions running.")
            for s in sessions:
                print(f"  {s.name:8} QOS_TEST_CONTROL={s.sock}  "
                      f"(pid {s.proc.pid if s.proc else '?'})")
        else:
            for s in sessions:
                s.stop()
                try:
                    os.unlink(s.sock)
                except FileNotFoundError:
                    pass

    print("\n" + "=" * 70)
    print(f"elapsed: {time.time() - started:.1f}s")
    if GAPS:
        print(f"{len(GAPS)} thing(s) this run could not verify:")
        for g in GAPS:
            print(f"  - {g}")
    if FAILURES:
        rc = 1
        print(f"{len(FAILURES)} claim(s) did not hold:")
        for f in FAILURES:
            print(f"  - {f}")
    else:
        print("every claim held.")
    print(f"screenshots: {shots}")
    return rc


if __name__ == "__main__":
    sys.exit(main())
