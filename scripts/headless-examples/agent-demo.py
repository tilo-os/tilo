#!/usr/bin/env python3
"""An agent edits a document someone has open, live, through the real stack.

The demo's agent beat (plan/yc-demo.md, beat 6), driven end to end:

  1. a headless compositor with a private runtime dir (its own broker,
     Wayland socket and control socket);
  2. the real word processor opened on `brief.md` — a heading, three
     bullets, and the line `Scribe: turn the bullets into a table` — as a
     Wayland client in the compositor's first workspace (room);
  3. `qos-agent` joining that room and that record, receiving the document
     from the word processor, carrying out the instruction with the word
     processor's own actions, and leaving;
  4. Ctrl+S in the word processor, and the file on disk read back: a pipe
     table where the bullets were, the instruction gone.

Snapshots land in /tmp/agent-demo-{01-before,02-during,03-after}.png; the
middle one is taken while the agent is pacing its edits, so its caret pill
("Scribe") is on screen. Exits non-zero on any failed step.
"""
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
BIN = REPO / "target" / "debug"
UID = os.getuid()

BRIEF = """# Launch plan

- Venue - The Hall
- Date - 3 Sep
- Budget - 1200

Scribe: turn the bullets into a table
"""


def fail(msg: str, *logs: pathlib.Path) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    for p in logs:
        if p.exists():
            tail = p.read_text(errors="replace").splitlines()[-25:]
            print(f"--- {p.name} ---\n" + "\n".join(tail), file=sys.stderr)
    sys.exit(1)


def control(sock: pathlib.Path, payload: dict) -> dict:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(30)
    s.connect(str(sock))
    s.sendall(json.dumps(payload).encode() + b"\n")
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(1 << 20)
        if not chunk:
            raise RuntimeError("compositor closed the control connection")
        buf += chunk
    s.close()
    resp = json.loads(buf.split(b"\n", 1)[0])
    if "error" in resp:
        raise RuntimeError(f"control {payload['op']} failed: {resp['error']}")
    return resp


def wait_for(pred, timeout: float, what: str, *logs):
    deadline = time.time() + timeout
    while time.time() < deadline:
        got = pred()
        if got:
            return got
        time.sleep(0.25)
    fail(f"timed out waiting for {what}", *logs)


def main() -> None:
    for b in ("shell", "word-processor", "qos-agent"):
        if not (BIN / b).exists():
            fail(f"build first: cargo build -p shell -p word-processor -p agent ({BIN / b} missing)")

    run = pathlib.Path(f"/run/user/{UID}/qos-agent-demo-{os.getpid()}")
    home = run / "home"
    home.mkdir(parents=True)
    brief = home / "brief.md"
    brief.write_text(BRIEF)
    ctl = run / "ctl"
    comp_log = run / "compositor.log"
    wp_log = run / "word-processor.log"
    agent_log = run / "agent.log"

    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "DISPLAY")}
    env.update({
        "XDG_RUNTIME_DIR": str(run),
        "XDG_CONFIG_HOME": str(run / "config"),
        "XDG_DATA_HOME": str(run / "data"),
        "XDG_STATE_HOME": str(run / "state"),
        "XDG_CACHE_HOME": str(run / "cache"),
        "HOME": str(home),
        "QOS_TEST_CONTROL": str(ctl),
        "NIRIOS_HEADLESS_SIZE": "1600x900",
        "RUST_LOG": "warn,shell=info",
    })
    procs = []
    try:
        comp = subprocess.Popen(
            [str(BIN / "shell"), "--mode", "native", "--backend", "headless"],
            env=env, stdout=subprocess.DEVNULL, stderr=open(comp_log, "w"),
        )
        procs.append(comp)
        wait_for(lambda: ctl.exists(), 30, "the control socket", comp_log)
        display_file = run / "qos-session" / "wayland-display"
        wait_for(lambda: display_file.exists(), 30, "the Wayland display file", comp_log)
        display = display_file.read_text().strip()
        broker = run / "qos" / "services" / "collab-json.sock"
        wait_for(lambda: broker.exists(), 30, "the JSON broker socket", comp_log)

        state = control(ctl, {"op": "state_dump"})["state"]
        tree = state["workspace_tree"]
        ws = next(w for w in tree["workspaces"] if w["id"] == tree["active"])
        room = ws["room"]
        print(f"room {room}; display {display}")

        # The word processor, as a client of this compositor, on the JSON
        # broker, announcing the room and its own handle — the env the
        # launcher would export for it.
        control(ctl, {"op": "place_next_window", "app_id": "dev.qos.word-processor"})
        wp_env = dict(env)
        wp_env.update({
            "WAYLAND_DISPLAY": display,
            "QUICK_COLLAB_SOCK": str(broker),
            "QOS_PACKAGE_ID": "dev.qos.word-processor",
            "QOS_WORKSPACE_ID": room,
            "QUICK_APP_ID": "quinten",
            "QOS_USER_ID": "quinten",
            "RUST_LOG": "warn",
        })
        wp = subprocess.Popen(
            [str(BIN / "word-processor"), str(brief)],
            env=wp_env, stdout=subprocess.DEVNULL, stderr=open(wp_log, "w"),
        )
        procs.append(wp)

        def tile():
            names = [t["name"] for t in control(ctl, {"op": "list_targets"})["targets"]]
            return next((n for n in names if n.startswith("foreign-surface-")), None)

        tile_name = wait_for(tile, 90, "the word processor's window", wp_log, comp_log)
        time.sleep(4.0)  # first frame + the app's record announcement
        control(ctl, {"op": "snapshot", "path": "/tmp/agent-demo-01-before.png"})
        print("OK  word processor is up")

        # The agent: same room, same record, its own name.
        record = f"file:{brief.resolve()}"
        agent = subprocess.Popen(
            [str(BIN / "qos-agent"), "--socket", str(broker), "--workspace", room,
             "--record", record, "--name", "Scribe", "--once", "--pace-ms", "600"],
            env={**env, "RUST_LOG": "info"}, stdout=subprocess.DEVNULL, stderr=open(agent_log, "w"),
        )
        procs.append(agent)
        time.sleep(2.0)
        control(ctl, {"op": "snapshot", "path": "/tmp/agent-demo-02-during.png"})
        try:
            code = agent.wait(timeout=60)
        except subprocess.TimeoutExpired:
            fail("the agent never finished", agent_log, wp_log, comp_log)
        if code != 0:
            fail(f"the agent exited with {code}", agent_log, wp_log, comp_log)
        log = agent_log.read_text(errors="replace")
        if "insert table" not in log:
            fail("the agent did not report inserting a table", agent_log, wp_log)
        print("OK  agent carried out the instruction")
        time.sleep(1.5)
        control(ctl, {"op": "snapshot", "path": "/tmp/agent-demo-03-after.png"})

        # Save from the word processor and read the file back.
        rect = next(t["rect"] for t in control(ctl, {"op": "list_targets"})["targets"] if t["name"] == tile_name)
        cx, cy = rect[0] + rect[2] / 2, rect[1] + rect[3] / 2
        control(ctl, {"op": "click_at", "x": cx, "y": cy})
        time.sleep(0.3)
        # A client sees modifiers through real key events on the seat, not
        # the harness's modifier mask: hold Control, press s, release.
        control(ctl, {"op": "key_down", "key": "Control"})
        control(ctl, {"op": "key", "key": "s"})
        control(ctl, {"op": "key_up", "key": "Control"})

        def saved():
            text = brief.read_text()
            return text if "|" in text and "Scribe:" not in text else None

        text = wait_for(saved, 20, "the saved file to contain the table", wp_log, agent_log)
        print("--- brief.md after ---\n" + text.strip())
        rows = [l for l in text.splitlines() if l.startswith("|") and "---" not in l]
        cells = [[c.strip() for c in r.strip("|").split("|")] for r in rows]
        expect = [["Venue", "The Hall"], ["Date", "3 Sep"], ["Budget", "1200"]]
        if cells != expect:
            fail(f"table rows {cells!r}, expected {expect!r}", wp_log, agent_log)
        if any(l.startswith("- ") for l in text.splitlines()):
            fail("the bullets are still there", wp_log, agent_log)
        print("OK  the word processor saved the table the agent made")
        print("PASS")
    finally:
        for p in procs:
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)
        for p in procs:
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()


if __name__ == "__main__":
    main()
