#!/usr/bin/env python3
"""An agent edits a document someone has open, live, through the real stack.

The demo's agent beat (docs/demo-script.md, beat 6), driven end to end:

  1. a headless compositor with a private runtime dir (its own broker,
     Wayland socket and control socket);
  2. the real word processor opened on `brief.md` — a heading, three
     bullets, and the line `Claude: turn the bullets into a table` — as a
     Wayland client in the compositor's first workspace (room);
  3. `tilo-agent` joining that room and that record, receiving the document
     from the word processor, carrying out the instruction with the word
     processor's own actions, and leaving;
  4. Ctrl+S in the word processor, and the file on disk read back: a pipe
     table where the bullets were, the instruction gone.

Which action the agent takes is chosen by a model, from the catalogue of
things the word processor exports. That choice is the only scripted step
this demo ever had, and the run asserts which path made it rather than
assuming: `--no-model` pins the decision to the agent's keyword vocabulary
and this script then requires the log to say so, so a run on the fallback
can never be mistaken afterwards for a live one.

For a live run the agent needs a key. It is read from `ANTHROPIC_API_KEY`,
or from the `KEY=value` file named by `ANTHROPIC_ENV_FILE` — pass the file,
not the value, so no credential reaches a command line or a process list.
`HOME` is redirected here, so the agent's own home-directory fallback will
not find anything; name the file explicitly.

Snapshots land in /tmp/agent-demo-{01-before,02-during,03-after}.png; the
middle one is taken while the agent is pacing its edits, so its caret pill
("Claude") is on screen. Exits non-zero on any failed step.
"""
import argparse
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import time

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
BIN = REPO / "target" / "debug"
UID = os.getuid()

# The two endings. Each is a document and an instruction; which action
# answers the instruction is the model's to choose, from the catalogue the
# word processor exports. Neither instruction names an action.
ENDINGS = {
    # The bigger visible change at demo speed.
    "table": """# Launch plan

- Venue - The Hall
- Date - 3 Sep
- Budget - 1200

Claude: turn these into a table
""",
    # The one that shows judgement rather than one mechanical edit: a flat
    # draft, no markup at all, asked to become a document.
    "structure": """Quarterly review

Revenue

We closed the quarter ahead of plan, with most of the gain arriving late.

Risks

Two suppliers remain single-sourced and that has not moved since March.

Claude: make this a proper document
""",
}


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
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--no-model",
        action="store_true",
        help="decide by keyword instead of asking a model, and require the log to say so",
    )
    ap.add_argument(
        "--snapshot-prefix",
        default="/tmp/agent-demo",
        help="where the three snapshots go (default: /tmp/agent-demo)",
    )
    ap.add_argument(
        "--ending",
        choices=sorted(ENDINGS),
        default="table",
        help="which ending to drive: bullets becoming a table, or a flat "
             "draft being given its structure (default: table)",
    )
    args = ap.parse_args()

    for b in ("shell", "word-processor", "tilo-agent"):
        if not (BIN / b).exists():
            fail(f"build first: cargo build -p shell -p word-processor -p agent ({BIN / b} missing)")

    run = pathlib.Path(f"/run/user/{UID}/tilo-agent-demo-{os.getpid()}")
    home = run / "home"
    home.mkdir(parents=True)
    brief = home / "brief.md"
    brief.write_text(ENDINGS[args.ending])
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
        "TILO_TEST_CONTROL": str(ctl),
        "TILO_HEADLESS_SIZE": "1600x900",
        "RUST_LOG": "warn,shell=info",
        # The sandbox does not clear the environment, so the host locale
        # reaches the app and a de_DE machine renders the word processor's
        # chrome in German. Any frame this writes could end up as footage,
        # and half-translated chrome reads as broken software.
        "LC_ALL": "en_US.UTF-8",
        "LANG": "en_US.UTF-8",
        "LANGUAGE": "en_US",
    })
    procs = []
    try:
        comp = subprocess.Popen(
            [str(BIN / "shell"), "--mode", "native", "--backend", "headless"],
            env=env, stdout=subprocess.DEVNULL, stderr=open(comp_log, "w"),
            preexec_fn=die_with_parent(),
        )
        procs.append(track(comp))
        wait_for(lambda: ctl.exists(), 30, "the control socket", comp_log)
        display_file = run / "tilo-session" / "wayland-display"
        wait_for(lambda: display_file.exists(), 30, "the Wayland display file", comp_log)
        display = display_file.read_text().strip()
        broker = run / "tilo" / "services" / "collab.sock"
        wait_for(lambda: broker.exists(), 30, "the typed broker socket", comp_log)

        state = control(ctl, {"op": "state_dump"})["state"]
        tree = state["workspace_tree"]
        ws = next(w for w in tree["workspaces"] if w["id"] == tree["active"])
        room = ws["room"]
        print(f"room {room}; display {display}")

        # The word processor inherits the same connected service fd the real
        # launcher supplies.
        control(ctl, {"op": "place_next_window", "app_id": "so.tilo.word-processor"})
        wp_env = dict(env)
        wp_env.update({
            "WAYLAND_DISPLAY": display,
            "TILO_PACKAGE_ID": "so.tilo.word-processor",
            "TILO_WORKSPACE_ID": room,
            "QUICK_APP_ID": "ada",
            "TILO_USER_ID": "ada",
            "RUST_LOG": "warn",
        })
        wp_service = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        wp_service.connect(str(broker))
        wp_env["TILO_SVC_COLLAB_FD"] = str(wp_service.fileno())
        wp = subprocess.Popen(
            [str(BIN / "word-processor"), str(brief)],
            env=wp_env, stdout=subprocess.DEVNULL, stderr=open(wp_log, "w"),
            pass_fds=(wp_service.fileno(),),
            preexec_fn=die_with_parent(),
        )
        wp_service.close()
        procs.append(track(wp))

        def tile():
            names = [t["name"] for t in control(ctl, {"op": "list_targets"})["targets"]]
            return next((n for n in names if n.startswith("foreign-surface-")), None)

        tile_name = wait_for(tile, 90, "the word processor's window", wp_log, comp_log)
        time.sleep(4.0)  # first frame + the app's record announcement
        control(ctl, {"op": "snapshot", "path": f"{args.snapshot_prefix}-01-before.png"})
        print("OK  word processor is up")

        # The agent: same room, same record, its own name.
        record = f"file:{brief.resolve()}"
        argv = [str(BIN / "tilo-agent"), "--endpoint", str(broker), "--workspace", room,
                "--record", record, "--name", "Claude", "--once", "--pace-ms", "600"]
        if args.no_model:
            argv.append("--no-model")
        agent = subprocess.Popen(
            argv,
            env={**env, "RUST_LOG": "info"}, stdout=subprocess.DEVNULL, stderr=open(agent_log, "w"),
            preexec_fn=die_with_parent(),
        )
        procs.append(track(agent))

        # The middle frame is meant to catch the agent mid-edit, with its
        # caret pill on screen. Choosing takes a few seconds on a live run,
        # so wait for the first edit instead of sleeping a fixed 2s — that
        # photographed the pause before the work rather than the work.
        def editing():
            return agent_log.exists() and "applied" in agent_log.read_text(errors="replace")

        wait_for(editing, 60, "the agent's first edit", agent_log, wp_log)
        time.sleep(0.4)
        control(ctl, {"op": "snapshot", "path": f"{args.snapshot_prefix}-02-during.png"})
        try:
            code = agent.wait(timeout=60)
        except subprocess.TimeoutExpired:
            fail("the agent never finished", agent_log, wp_log, comp_log)
        if code != 0:
            fail(f"the agent exited with {code}", agent_log, wp_log, comp_log)
        log = agent_log.read_text(errors="replace")
        did = "insert table" if args.ending == "table" else "made the draft a document"
        if did not in log:
            fail(f"the agent never reported {did!r}", agent_log, wp_log)

        # Which path chose the action. The agent logs this per instruction;
        # requiring it here is what keeps a fallback run from being written
        # up as a live one. The log is colourised and quotes its string
        # values, so strip the escapes and allow the quotes.
        plain = re.sub(r"\x1b\[[0-9;]*m", "", log)
        decided = re.search(r'decided_by="?(\w+)"?.*?\blive=(\w+)', plain)
        if not decided:
            fail("the agent did not report how it chose", agent_log)
        route, live = decided.group(1), decided.group(2) == "true"
        if args.no_model:
            if route != "keywords" or live:
                fail(f"--no-model, but the agent reported {route!r} (live={live})", agent_log)
            print("OK  the keyword vocabulary chose, with no model and no network")
        else:
            if route != "model" or not live:
                fail(
                    f"expected a live model choice, got {route!r} (live={live}). "
                    "Set ANTHROPIC_API_KEY or ANTHROPIC_ENV_FILE, or run with --no-model.",
                    agent_log,
                )
            print("OK  a model chose the action, from the app's exported catalogue")
        print("OK  agent carried out the instruction")
        time.sleep(1.5)
        control(ctl, {"op": "snapshot", "path": f"{args.snapshot_prefix}-03-after.png"})

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

        # The instruction is gone from every ending, and the mark that
        # says the work happened differs by ending.
        mark = "|" if args.ending == "table" else "## "

        def saved():
            text = brief.read_text()
            return text if mark in text and "Claude:" not in text else None

        text = wait_for(saved, 20, f"the saved file to show the {args.ending}", wp_log, agent_log)
        print("--- brief.md after ---\n" + text.strip())
        lines = text.splitlines()

        if args.ending == "table":
            rows = [l for l in lines if l.startswith("|") and "---" not in l]
            cells = [[c.strip() for c in r.strip("|").split("|")] for r in rows]
            expect = [["Venue", "The Hall"], ["Date", "3 Sep"], ["Budget", "1200"]]
            if cells != expect:
                fail(f"table rows {cells!r}, expected {expect!r}", wp_log, agent_log)
            if any(l.startswith("- ") for l in lines):
                fail("the bullets are still there", wp_log, agent_log)
            print("OK  the word processor saved the table the agent made")
        else:
            if "# Quarterly review" not in lines:
                fail(f"the first line never became the title: {lines[:3]!r}", wp_log, agent_log)
            sections = sorted(l for l in lines if l.startswith("## "))
            if sections != ["## Revenue", "## Risks"]:
                fail(f"section headings {sections!r}, expected Revenue and Risks", wp_log, agent_log)
            # The prose under each heading must still be prose — promoting
            # a sentence would be the failure that looks broken.
            for prose in ("We closed the quarter", "Two suppliers remain"):
                if not any(l.startswith(prose) for l in lines):
                    fail(f"the paragraph {prose!r} did not survive as prose", wp_log, agent_log)
            print("OK  the word processor saved the structure the agent gave the draft")
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
