#!/usr/bin/env python3
"""The Tasks agent working in tilolang, end to end, in a headless nested session.

Board, Todo, Chat, Notes and Views are built with `tilolang build` and installed into a
headless nested Tilo session (dist/dev/nested-session.sh, lavapipe only) beside the Tasks agent,
Tasks and Settings. The workspace's model is a recorded one: the fake Anthropic Messages
endpoint of scripts/perf/agent_task.py on loopback, replaying
scripts/headless-examples/agent-tilolang/release.json, configured as a provider on this device
through Settings' own provider actions, as a person adds LM Studio. Nothing reaches a hosted
model and no key is used.

Board, Todo and Chat are filled through their own actions with a release a week out. Board is
focused, the quick composer opened on it (Super+C), "what's left for the release?" typed and
sent, as a person asks. Then:

- **steps**: the run records each program it ran on its issue, with what it answered, in
  tilolang: three steps over Board, Todo and Chat, each answer naming their resources.
- **answer**: the run finishes with a view: a View in Views, written by the agent's task, which
  the composer opens beside Board.
- **shots**: PNGs of the session while it works and once it answered (OUT/*.png).

  scripts/headless-examples/agent-tilolang-e2e.py [--no-build] [--out DIR] [--keep]

Without --no-build it prepares the session (nested-session.sh --prepare-only, building through
TILO_REMOTE_CARGO, default rcargo) and builds the tilolang tool. The verdict is
OUT/summary.json; the exit status is non-zero when a check fails. Never touches the live
desktop.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import sys
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE / "agent-tilolang"))
from replay_model import serve  # noqa: E402

_spec = importlib.util.spec_from_file_location("tilolang_e2e", HERE / "tilolang-e2e.py")
e2e = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(e2e)
Failed, Session, until, run = e2e.Failed, e2e.Session, e2e.until, e2e.run

APPS = ["board", "todo", "chat", "notes", "views"]
NATIVE = "agent tasks settings"
SCRIPT = HERE / "agent-tilolang/release.json"
TRACKER = "tasks.tracker"


def app_id(name: str) -> str:
    return f"so.tilo.lang.{name}"


def action(session: Session, app: str, title: str, deadline: float = 30.0) -> dict:
    """The action `app` publishes under `title`, once it does."""
    return until(f"{app}'s {title!r}", lambda: e2e.app_action(session, app, title), deadline)


def invoke(session: Session, found: dict, args: dict | None = None):
    answer = session.invoke(found["provider"], found["id"], args)
    state = answer.get("receipt", {}).get("update", {}).get("state")
    if state not in (None, "succeeded"):
        raise Failed(f"{found['title']} did not succeed: {json.dumps(answer)[:1500]}")
    return answer


def configure_model(session: Session, port: int) -> dict:
    """A provider on this device, keyless, at the recorded model's loopback address, speaking
    Anthropic Messages, chosen for agents: Settings' own provider actions, as a person does."""
    settings = "so.tilo.settings"
    invoke(session, action(session, settings, "Add provider"), {"preset": "lm-studio"})
    providers = until("the new provider", lambda: [
        a for a in session.actions("Set provider details") if a.get("application") == settings])
    found = providers[-1]
    provider = found.get("resource", "").rsplit("/", 1)[-1] or "lm-studio"
    invoke(session, found, {"provider": provider, "label": "Recorded model",
                            "endpoint": f"http://127.0.0.1:{port}", "model": "release-replay"})
    invoke(session, action(session, settings, "Speak another protocol to a provider"),
           {"provider": provider, "protocol": "anthropic"})
    invoke(session, action(session, settings, "Set default provider"), {"provider": provider})
    return {"provider": provider, "port": port}


def seed(session: Session) -> dict:
    """A release a week out, through each app's own actions: Board's columns and cards, the
    Todo list and the release conversation in Chat."""
    board, todo, chat = app_id("board"), app_id("todo"), app_id("chat")
    columns = {}
    for title in ["To do", "Doing", "Done"]:
        made = e2e.value_of(invoke(session, action(session, board, "New column")))
        column = made["id"] if isinstance(made, dict) else made
        invoke(session, action(session, board, "Rename column"), {"col": column, "title": title})
        columns[title] = column
    cards = [
        ("To do", "Write the release notes", "Lead with the sync fix, then the new board."),
        ("To do", "Screenshots for the docs", ""),
        ("To do", "Translate the onboarding", "German and French first."),
        ("To do", "Check the importer on large tables", ""),
        ("Doing", "Fix the sidebar at large text", "It collapses too early above 150%."),
        ("Doing", "Review the sharing dialog", ""),
        ("Done", "Bump the version", ""),
        ("Done", "Merge the offline queue", "Landed on Tuesday."),
    ]
    for column, title, notes in cards:
        invoke(session, action(session, board, "Add card"),
               {"col": columns[column], "title": title, "notes": notes})
    invoke(session, action(session, todo, "Rename"), {"name": "Release 2.4"})
    items = ["Freeze the strings for translation", "Record the demo video", "Update the changelog",
             "Send the beta to the testers", "Check the upgrade from 2.3"]
    for title in items:
        invoke(session, action(session, todo, "Add"), {"title": title})
    release = e2e.value_of(invoke(session, action(session, chat, "Start"), {"title": "Release checklist"}))
    conversation = release["id"] if isinstance(release, dict) else release
    for text in ["Docs still need the new screenshots.", "I'll take the screenshots after lunch."]:
        invoke(session, action(session, chat, "Post"), {"c": conversation, "text": text, "block": ""})
    return {"columns": len(columns), "cards": len(cards), "todos": len(items)}


def open_apps(session: Session) -> dict:
    for name in APPS:
        session.invoke("applications", app_id(name))
    def admitted():
        windows = {w.get("app_id"): w for w in session.windows()}
        return windows if all(app_id(n) in windows for n in APPS) else None
    windows = until("every app's window", admitted, 60)
    return {app_id(n): {"group": windows[app_id(n)]["group"], "tab": windows[app_id(n)]["tab"]}
            for n in APPS}


def tracker_runs(session: Session) -> list:
    """The tracker's issues as the door reads them."""
    read = session.control_json("read", TRACKER)
    return read if isinstance(read, list) else read.get("issues", read.get("children", []))


def ask(session: Session, windows: dict, request: str, out: Path) -> dict:
    """Board in front, the composer opened on it, the request typed and sent."""
    board = windows[app_id("board")]
    session.invoke("desktop-windows", f"tab.focus:{board['group']}:{board['tab']}")
    time.sleep(0.5)
    session.control_cmd("modifiers", "logo")
    session.control_cmd("key", "c")
    session.control_cmd("modifiers", "")
    until("the composer", lambda: "agent-quick-editor" in session.control_cmd("list-targets"), 15)
    session.control_cmd("type", request)
    session.control_cmd("snapshot", str(out / "asking.png"))
    session.control_cmd("click", "agent-quick-send")
    return {"asked": request}


def follow(session: Session, out: Path, replay) -> dict:
    """Shots while the run works, until the recorded model's last turn was asked and the
    tracker says the run ended."""
    shots = []
    end = time.monotonic() + 300
    while time.monotonic() < end:
        if replay.requests and len(replay.requests) >= 2 and len(shots) < 3:
            path = out / f"working-{len(shots) + 1}.png"
            session.control_cmd("snapshot", str(path))
            shots.append(str(path))
        state = session.control_json("state-dump")
        if '"Done"' in json.dumps(state.get("tasks", {})):
            break
        if len(replay.requests) >= 4:
            time.sleep(3)
            break
        time.sleep(1)
    session.control_cmd("snapshot", str(out / "answered.png"))
    return {"turns": len(replay.requests), "shots": shots}


def check_steps(session: Session) -> dict:
    """The run's steps, as its issue records them: one per program, each with its answer."""
    dump = json.dumps(session.control_json("read", TRACKER))
    for needle in ["Write the release notes", "Record the demo video", "Release checklist",
                   "Left for the release"]:
        if needle not in dump:
            raise Failed(f"the issue does not record {needle!r}: {dump[:3000]}")
    return {"recorded": True}


def check_answer(session: Session) -> dict:
    """The answer is a View in Views, open in a window."""
    windows = [w for w in session.windows() if w.get("app_id") == app_id("views")]
    if not windows:
        raise Failed("Views has no window open for the answer")
    return {"windows": len(windows)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path, default=REPO / "target/agent-tilolang-e2e")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    state = out / "state"
    tool = REPO / "target/debug/tilolang"
    os.environ.setdefault("TMPDIR", str(Path.home() / "tmp"))
    Path(os.environ["TMPDIR"]).mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "TILO_REMOTE_CARGO": os.environ.get("TILO_REMOTE_CARGO", "rcargo"),
           "TILO_NESTED_APPS": NATIVE}
    build = [] if not args.no_build else ["--no-build"]
    print("==> preparing the session", file=sys.stderr)
    run([e2e.SESSION, *build, "--prepare-only", "--apps", NATIVE, "--state", state], env=env)
    if not args.no_build:
        print("==> building the tilolang tool", file=sys.stderr)
        run(["cargo", "build", "--locked", "-p", "tilolang-cli", "--bin", "tilolang"], cwd=REPO)
    runtime = (state / "runtimes/.version").read_text().strip()
    run([tool, "build", REPO / "apps/tl/views", "--search", REPO / "apps/tl", "--out", out / "build",
         "--tilo-runtime", f"tilo-runtime/{runtime}"])

    model, replay = serve(SCRIPT)
    summary: dict = {"checks": {}, "failed": []}
    proc = None
    try:
        proc, session = e2e.start_session(state, out, [out / "build" / f"{n}.tapp" for n in APPS],
                                          "agent", native=NATIVE)
        e2e.run_checks(summary, [
            ("model", lambda: configure_model(session, model.port)),
            ("apps", lambda: open_apps(session)),
            ("seed", lambda: seed(session)),
            ("ask", lambda: ask(session, summary["checks"]["apps"], json.loads(SCRIPT.read_text())["request"], out)),
            ("follow", lambda: follow(session, out, replay)),
            ("steps", lambda: check_steps(session)),
            ("answer", lambda: check_answer(session)),
        ])
        if model.errors:
            summary["failed"].append("model")
            summary["model_errors"] = model.errors
    except Failed as error:
        summary["failed"].append("session")
        summary["error"] = str(error)
        print(f"FAIL {error}", file=sys.stderr)
    finally:
        (out / "summary.json").write_text(json.dumps(summary, indent=2))
        model.close()
        if not args.keep:
            e2e.stop_session(proc)
    print(f"==> {'FAILED: ' + ', '.join(summary['failed']) if summary['failed'] else 'every check passed'}"
          f" (summary: {out / 'summary.json'})", file=sys.stderr)
    return 1 if summary["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
