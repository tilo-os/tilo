#!/usr/bin/env python3
"""The Tasks agent working in tilolang, end to end, in a headless nested session.

Tasks, Notes, Chat and Views are built with `tilolang build` and installed into a headless
nested Tilo session (dist/dev/nested-session.sh, lavapipe only) beside the Tasks agent, the
Tasks tracker and Settings. The workspace's model is a recorded one: the fake Anthropic Messages
endpoint of scripts/perf/agent_task.py on loopback, replaying
scripts/headless-examples/agent-tilolang/release.json, configured as a provider on this device
through Settings' own provider actions, as a person adds LM Studio. Nothing reaches a hosted
model and no key is used.

Tasks, Notes and Chat are filled through their own actions with a release a week out. Tasks is
focused, the quick composer opened on it (Super+C), the request typed and sent, as a person
asks. The agent works through tilolang programs, each run by the session's door as a step of
the task:

- **steps**: the run records each program on its issue with what it answered: reads over
  Tasks, Notes and Chat, a note it writes, a step it undoes, a change to many tasks previewed
  and then confirmed.
- **answer**: the run finishes with a view: a View in Views, written by the agent's task, which
  the composer opens beside Tasks.
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

APPS = ["tasks", "notes", "chat", "views"]
NATIVE = "agent tasks settings"
SCRIPT = HERE / "agent-tilolang/release.json"
TRACKER = "tasks.tracker"
AGENT = "scribe"
MODEL = "release-replay"
# Writes a keyless provider on this device into the session's settings
# (crates/agent/provider-config/examples/local_provider.rs).
LOCAL_PROVIDER = REPO / "target/debug/examples/local_provider"


def app_id(name: str) -> str:
    return f"so.tilo.lang.{name}"


def tl_resource(found: dict) -> str | None:
    """The tilolang resource an action is bound to (`tl:<app>:<id>`): a bound resource's id
    ends with its published id in hex."""
    resource = found.get("resource") or ""
    if resource.startswith("tl:"):
        return resource
    tail = resource.rsplit(":", 1)[-1]
    try:
        return bytes.fromhex(tail).decode()
    except ValueError:
        return None


def actions_of(session: Session, app: str, title: str) -> list[dict]:
    """The actions `app` publishes under `title`, each bound to one of its resources."""
    return [a for a in session.actions(title)
            if a.get("title") == title and (tl_resource(a) or "").startswith(f"tl:{app}:")]


def action(session: Session, app: str, title: str, deadline: float = 30.0) -> dict:
    """The action `app` publishes under `title`, once it does."""
    return until(f"{app}'s {title!r}", lambda: next(iter(actions_of(session, app, title)), None), deadline)


def act(session: Session, app: str, title: str, args: dict | None = None):
    """Invoke the action `app` publishes under `title`, found afresh: a directory id names one
    revision of the directory, and one taken before the last change is refused."""
    for _ in range(20):
        try:
            return invoke(session, action(session, app, title), args)
        except Failed as error:
            if "list it again" not in str(error):
                raise
            time.sleep(0.25)
    raise Failed(f"{app}'s {title!r} kept changing under its invocation")


def invoke(session: Session, found: dict, args: dict | None = None):
    answer = session.invoke(found["provider"], found["id"], args)
    state = answer.get("receipt", {}).get("update", {}).get("state")
    if state not in (None, "succeeded"):
        raise Failed(f"{found['title']} did not succeed: {json.dumps(answer)[:1500]}")
    return answer


def configure_model(session: Session, port: int) -> dict:
    """A provider on this device, keyless, at the recorded model's loopback address, chosen for
    agents, and an agent in the workspace to ask: what a person sets up in Settings and the
    workspace's Agents tab."""
    env = session.env()
    run([LOCAL_PROVIDER, f"http://127.0.0.1:{port}/v1", MODEL], env=env)
    session.invoke("actions", "agent.invite", {"name": AGENT, "preset": "contributor"})
    session.control_cmd("key", "Escape")
    ready = until("the agent ready to be asked", lambda: [
        a for a in session.actions("Ask an agent: release")
        if a.get("provider") == "ask-agent" and AGENT in (a.get("subtitle") or "").lower()], 30)
    return {"port": port, "agent": AGENT, "says": ready[0].get("subtitle")}


def seed(session: Session) -> dict:
    """A release a week out, through each app's own actions: what is left in Tasks, a note in
    Notes, and the team's channel in Chat."""
    tasks, notes, chat = app_id("tasks"), app_id("notes"), app_id("chat")
    left = ["Record the demo video", "Update the changelog", "Check the upgrade from 2.3",
            "Write the release notes"]
    for title in left:
        act(session, tasks, "Add", {"title": title})
    act(session, notes, "New note")
    act(session, chat, "New channel", {"name": "release"})
    return {"tasks": len(left), "notes": 1, "channels": 1}


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
    """Tasks in front, the composer opened on it, the request typed and sent."""
    front = windows[app_id("tasks")]
    session.invoke("desktop-windows", f"tab.focus:{front['group']}:{front['tab']}")
    time.sleep(0.5)
    session.control_cmd("modifiers", "logo")
    session.control_cmd("key", "c")
    session.control_cmd("modifiers", "")
    try:
        # The agent the chord starts takes up to its start wait to come up.
        until("the composer", lambda: "agent-quick-editor" in session.control_cmd("list-targets"), 30)
    except Failed:
        # The palette's ask makes the same task for the same agent.
        session.control_cmd("snapshot", str(out / "no-composer.png"))
        found = until("the palette's ask", lambda: [
            a for a in session.actions(f"Ask an agent: {request}")
            if a.get("provider") == "ask-agent"], 30)
        invoke(session, found[0])
        return {"asked": request, "through": "palette"}
    session.control_cmd("type", request)
    session.control_cmd("snapshot", str(out / "asking.png"))
    session.control_cmd("click", "agent-quick-send")
    return {"asked": request, "through": "composer"}


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
        if len(replay.requests) >= 9:
            time.sleep(3)
            break
        time.sleep(1)
    session.control_cmd("snapshot", str(out / "answered.png"))
    return {"turns": len(replay.requests), "shots": shots}


def check_steps(session: Session) -> dict:
    """The run's steps, as its issue records them: one per program, each with its answer."""
    dump = json.dumps(session.control_json("read", TRACKER))
    for needle in ["Record the demo video", "Left for 2.4", "undone", "Left for the release"]:
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
    parser.add_argument("--port", type=int, default=0, help="the recorded model's port (any free one)")
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
        run(["cargo", "build", "--locked", "-p", "tilolang-cli", "--bin", "tilolang",
             "-p", "agent-provider-config", "--example", "local_provider"], cwd=REPO)
    runtime = (state / "runtimes/.version").read_text().strip()
    run([tool, "build", REPO / "apps/tl/views", "--search", REPO / "apps/tl", "--out", out / "build",
         "--tilo-runtime", f"tilo-runtime/{runtime}"])

    model, replay = serve(SCRIPT, port=args.port)
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
