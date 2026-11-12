#!/usr/bin/env python3
"""Two people's sessions in one workspace, and an edit crossing between them.

Starts two headless sessions, each with its own state directory, its own
identity and its own runtime directory, the tree's own apps installed as signed
packages and launched by each session's own launcher into its own sandbox. One
person invites the other into their workspace with the desk's own invitation;
the other is started with that link and comes into the workspace on its way up,
with nobody at its desk.

  workspace  the peer holds the workspace the host invited it into: the same
             durable id, the same owner, and its own copy of what the workspace
             records
  notes      a note page written in the native Notes window on one session is
             found by Notes on the other, and that window draws again with it
  tasks      a task typed into the tilolang Tasks window on one session is
             listed by Tasks on the other, and that window draws again with it
  sandboxes  both apps are the launcher's own sandboxed processes with no
             network of their own, so what crossed went through the
             collaboration door rather than through a socket an app holds

Every claim is read from the session that did not make the change: the app on
the other side answers it from its own copy of the record, its window there is
counted before and after, and the desk's state is read from that session's own
control socket. Screenshots of both sides are left in OUT.

A native app's record crosses today. A tilolang app's does not: each session's
tilolang host keeps its own copy of the same record and never merges with the
other's, and the first action a session invokes on a joined workspace's tilolang
app ends that host's collaboration ("collaboration stopped: native invocation
capture; outcome unknown"). The two Tasks checks say so, with what the peer's
own session logged, and stay red until the record crosses.

    scripts/two-session-collab-demo.py
    scripts/two-session-collab-demo.py --out target/collab-demo --build

It needs the session and its apps built (dist/dev/nested-session-build.sh, or
`TILO_NESTED_SESSION_BUILD` naming where they were); `--build` makes them
first, through `TILO_NESTED_SESSION_BUILD_EXEC` when this machine has no
toolchain. Nothing here touches the live desktop or this machine's GPU: each
session renders headless on lavapipe.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sys
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE / "headless-examples"))
from real_apps import BUILD, Failed, Session, run, until  # noqa: E402
from two_people import (  # noqa: E402
    DESK,
    device_identity,
    front_workspace,
    joined,
    members_of,
    owner_of,
    workspaces,
)

# The native app: a package the tree builds, launched by the session's launcher.
NOTES = "so.tilo.notes"
# The tilolang app: Tasks, installed from its own signed package.
TASKS = "so.tilo.lang.tasks"
TASKS_PACKAGE = REPO / "apps/tl/tasks"
# The resource the desk publishes its own windows as: what brings one to the front.
WINDOWS = "desktop:desk"
# How long a change may take to cross: two devices on one machine, so this is
# the app's own wake, not a network's round trip.
CROSSING_BOUND = 60.0
# What each session installs besides Tasks: the native app the session package
# the tree builds itself.
APPS = "notes"

failures: list[str] = []


def step(number: str, text: str) -> None:
    print(f"\n=== {number}  {text}", flush=True)


def check(label: str, ok, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""), flush=True)
    if not ok:
        failures.append(label)
    return bool(ok)


def note(text: str) -> None:
    print(f"       {text}", flush=True)


# --- what a session holds ---------------------------------------------------------


def listed_action(session: Session, application: str, title: str, deadline: float = 120.0):
    """The action whose title holds `title` that the window of `application` publishes."""
    def found():
        for action in session.actions(title):
            if (action.get("application") == application
                    and title.lower() in (action.get("title") or "").lower()):
                return action
        return None
    try:
        return until(f"{application}'s {title!r} action", found, deadline)
    except Failed:
        listed = sorted({f"{a.get('title')} ({a.get('operation_kind')})"
                         for a in session.actions("") if a.get("application") == application})
        raise Failed(f"{application} publishes no {title!r}; it lists {listed}") from None


def answers(session: Session, action: dict, args: dict | None = None) -> str:
    """What an app's own action answers, as JSON text."""
    answer = session.invoke(action.get("provider") or "app-actions", action["id"], args)
    try:
        value = answer["receipt"]["update"]["output"]["value"]
    except (KeyError, TypeError):
        return json.dumps(answer)
    return json.dumps(value)


def frames(session: Session, app: str) -> int:
    """How many frames the window of `app` has drawn: a still window keeps its count."""
    for window in session.windows():
        if window.get("app_id") == app and window.get("window") is not None:
            return int(window.get("frames") or 0)
    return 0


def redrawn(session: Session, app: str, drawn: int, what: str) -> int:
    """Wait for the window of `app` to draw again past `drawn`: it woke with `what`."""
    return until(f"the {app} window on {session.tag} to draw {what}",
                 lambda: (now if (now := frames(session, app)) > drawn else None),
                 CROSSING_BOUND, 0.25)


def focus(session: Session, app: str) -> dict:
    """Bring `app`'s window to the front, as the desk's own verb does, and let it take the
    keyboard: a window brought forward hands its field the keys on the pass after it is up."""
    window = until(f"the window of {app} on {session.tag}",
                   lambda: next((w for w in session.windows()
                                 if w.get("app_id") == app and w.get("window") is not None), None),
                   180, 0.5)
    session.invoke(DESK, f"{WINDOWS}\u001ffocus", {"tab": str(window["tab"])})
    # The desk's own row says which tab is in front: keys typed before the switch has
    # landed go to the window that had them, so the wait is on that row and the pause
    # on the frame that follows it.
    until(f"the desk on {session.tag} to put {app} in front",
          lambda: next((row for row in session.windows()
                        if row.get("tab") == window["tab"] and row.get("active")), None), 60, 0.2)
    time.sleep(1.0)
    return window


def sandboxed(session: Session, app: str) -> bool:
    """Whether `app` runs as the launcher's own sandboxed process.

    A sandboxed app's binary sits in the store the launcher mounts for it,
    `/var/tilo-apps/<id>/`, which exists in the app's own mount namespace and
    nowhere else: a process of the app running from that path is one the
    launcher made, inside the namespaces it gives it."""
    return bool(session.processes(app))


def package_network(session: Session, app: str) -> tuple[bool, str]:
    """Whether the package `app` this session installed gives it no network, and what it says.

    The manifest is the one the launcher read the package's sandbox from: the
    installed package's own, not the tree's source for it."""
    for manifest in sorted((session.state / "apps").glob("pkgs/*.pkg/manifest.toml")):
        text = manifest.read_text(errors="replace")
        declared = re.search(r'^id\s*=\s*"([^"]+)"', text, re.MULTILINE)
        if not declared or declared.group(1) != app:
            continue
        net = text.split("[sandbox.net]", 1)[-1] if "[sandbox.net]" in text else ""
        mode = re.search(r'^\s*mode\s*=\s*"([^"]*)"', net, re.MULTILINE)
        where = f"{manifest.relative_to(session.state)}: [sandbox.net] mode = {mode.group(1) if mode else 'unset'}"
        return (mode is not None and mode.group(1) == "off"), where
    return False, f"no installed package of {app} in {session.state}"


def stumbles(session: Session) -> list[str]:
    """What this session logged about collaboration it could not carry out."""
    log = session.state / "logs/session.log"
    if not log.is_file():
        return []
    plain = (re.sub(r"\x1b\[[0-9;]*m", "", line) for line in log.read_text(errors="replace").splitlines())
    return [line.strip()[-220:] for line in plain
            if any(needle in line for needle in
                   ("collaboration stopped", "outcome unknown", "did not become current"))][-4:]


def read_action(session: Session):
    """Tasks' `read` of its workspace's records, as the session lists it now.

    The row is an address in a listing that moves as the app publishes: a window
    that opens another record, or an instance that comes up again, offers its
    actions under a new address, and the one a listing held before is gone. So a
    caller reads the row again each time rather than keeping one."""
    for action in session.actions("Read the content"):
        parts = (action.get("id") or "").split("\u001f")
        if action.get("application") == TASKS and parts[-2:] == [f"tl:{TASKS}", "read"]:
            return action
    return None


def tasks_content(session: Session, bound: float = 180.0) -> str:
    """What Tasks on this session holds: its own `read` of the workspace's records.

    A tilolang app answers a read of the workspace's records by kind and id, and a
    read of each record of its own: the task titles are in what it answers. An
    instance that has not attached to its broker yet answers nothing, which is a
    task list none of whose titles are there rather than a failure of this run.
    A read refused because the row the listing held has moved on is tried again
    from a listing taken at the time, until `bound` has passed."""
    deadline = time.monotonic() + bound
    refusal = ""
    while True:
        action = read_action(session)
        if action is not None:
            try:
                text = answers(session, action)
            except Failed as error:
                refusal = str(error)
            else:
                try:
                    records = json.loads(text)
                except json.JSONDecodeError:
                    return text
                if isinstance(records, dict):
                    for record in records.get("records") or []:
                        if str(record.get("kind", "")).startswith(f"tl.{TASKS}."):
                            try:
                                text += "\n" + answers(session, action, {"item": record["item"]})
                            except Failed as error:
                                text += f"\n(Tasks did not answer yet: {error})"
                                break
                return text
        if time.monotonic() >= deadline:
            return f"(Tasks did not answer: {refusal})" if refusal else ""
        time.sleep(0.5)


def present(session: Session, room: str) -> list[str]:
    """Who `session` says is here in `room`, by the seat each of them is at."""
    peers = session.state_dump().get("peers") or {}
    held = next((held for held in peers.get("workspaces") or [] if held.get("room") == room), {})
    return [member.get("publisher") for member in held.get("members") or []]


def build_tasks(built: Path, state: Path) -> Path:
    """`apps/tl/tasks` as a signed package of `state`'s runtime, to install."""
    built.mkdir(parents=True, exist_ok=True)
    package = built / "tasks.tapp"
    if package.is_file():
        return package
    runtime = (state / "runtimes/.version").read_text().strip()
    tool = next(path for path in (state / "bin/tilolang", BUILD / "bin/tilolang") if path.is_file())
    run([tool, "build", TASKS_PACKAGE, "--search", REPO / "apps/tl", "--out", built,
         "--tilo-runtime", f"tilo-runtime/{runtime}"], cwd=REPO)
    return package


# --- the demonstration ------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=REPO / "target/collab-demo",
                        help="run directory for logs, screenshots and the two states")
    parser.add_argument("--build", action="store_true",
                        help="build the session and its apps first")
    parser.add_argument("--remote-exec", default=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"),
                        help="command prefix the session's build runs through")
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    token = uuid.uuid4().hex[:8]
    note_title = f"Release plan {token}"
    note_body = f"Ship the {token} build on Friday."
    task_title = f"Book the studio for {token}"

    # Tasks as a tilolang package of the host's runtime: built once, installed into
    # both states as the signed package the launcher trusts.
    with joined(out, apps=APPS, tapps=lambda state: [build_tasks(out / "build", state)],
                build_first=args.build, remote=args.remote_exec) as (
            host, peer, host_workspace, link):
        room = host_workspace["room"]
        token = uuid.uuid4().hex[:8]
        note_title = f"Release plan {token}"
        note_body = f"Ship the {token} build on Friday."
        task_title = f"Book the studio for {token}"
        try:
            step("1", "two sessions of different people, in one workspace")
            note(f"workspace {host_workspace['name']} ({room}), the link is {len(link)} characters")
            check("the host governs the workspace it made", bool(owner_of(host_workspace)),
                  json.dumps(host_workspace.get("policy"))[:600])
            check("the two sessions are separate processes", host.proc.pid != peer.proc.pid,
                  f"{host.proc.pid} and {peer.proc.pid}")
            check("they keep their state and their runtime apart",
                  host.state != peer.state and host.runtime_directory() != peer.runtime_directory(),
                  f"{host.runtime_directory()} and {peer.runtime_directory()}")
            peer_workspace = workspaces(peer)[room]
            check("the peer holds the workspace the link names", peer_workspace["room"], room)
            check("the peer's copy names the host's person as its owner",
                  owner_of(peer_workspace), owner_of(host_workspace))
            host_identity, peer_identity = device_identity(host), device_identity(peer)
            check("the two sessions are different people's computers",
                  bool(host_identity) and host_identity != peer_identity,
                  f"{host_identity[:16]} and {peer_identity[:16]}")
            check("the peer's copy carries what the workspace records",
                  bool(members_of(peer_workspace)
                       or (peer_workspace.get("policy") or {}).get("grants")),
                  json.dumps(peer_workspace.get("policy"))[:600])
            for one, other in ((host, peer), (peer, host)):
                seat = (other.state_dump().get("peers") or {}).get("own", {}).get("publisher")
                seen = until(f"{one.tag} to say {other.tag} is here",
                             lambda: seat in present(one, room) or None, CROSSING_BOUND, 0.5)
                check(f"{one.tag} says {other.tag} is here", bool(seat) and bool(seen),
                      f"{seat} among {present(one, room)}")
            host.snapshot(out / "01-host-workspace.png")
            peer.snapshot(out / "02-peer-workspace.png")

            step("2", "both sessions open the native app, through their own desktops")
            for session in (host, peer):
                session.launch(NOTES)
                session.answering(NOTES, deadline=240)
                check(f"{session.tag} runs Notes as the launcher's sandboxed process",
                      sandboxed(session, NOTES))
            offline, how = package_network(host, NOTES)
            check("the package Notes runs in gives it no network of its own", offline, how)

            step("3", "a note page written on the host reaches the peer's Notes")
            focus(host, NOTES)
            peer_drawn = frames(peer, NOTES)
            host.chord("Control", "n")
            host.type(note_title)
            host.key("Enter")
            host.type(note_body)
            host.key("Enter")
            host.snapshot(out / "03-host-note.png")
            def peer_finds() -> str | None:
                # The row is read again each time: an app's actions move with what its
                # window shows, so one a listing held before a change may be gone.
                pages = answers(peer, listed_action(peer, NOTES, "Find note", deadline=30.0),
                                {"title": note_title})
                return pages if note_title in pages else None

            def peer_holds_text() -> str | None:
                hits = answers(peer, listed_action(peer, NOTES, "Find text", deadline=30.0),
                               {"text": note_body})
                return hits if note_title in hits else None

            found = until("the peer's Notes to find the page", peer_finds, CROSSING_BOUND, 0.5)
            check("Notes on the peer finds the page the host wrote", bool(found), found[:500])
            body = until("the peer's Notes to find the page's text", peer_holds_text,
                         CROSSING_BOUND, 0.5)
            check("and the text of the page is in the peer's own notebook", bool(body), body[:500])
            drawn = redrawn(peer, NOTES, peer_drawn, "the page that arrived")
            check("the peer's Notes window woke and drew it", drawn > peer_drawn,
                  f"{peer_drawn} then {drawn} frames")
            peer.snapshot(out / "04-peer-note.png")

            step("4", "both sessions open the tilolang app")
            for session in (host, peer):
                session.launch(TASKS)
                session.answering(TASKS, deadline=300)
                check(f"{session.tag} runs Tasks as the launcher's sandboxed process",
                      sandboxed(session, TASKS))
            offline, how = package_network(host, TASKS)
            check("the package Tasks runs in gives it no network of its own", offline, how)

            step("5", "a task typed on the host reaches the peer's Tasks")
            # The peer's own Tasks is asked from the workspace the task crosses in: the app a
            # person's desk shows there is the one whose copy of it must list the task, and a
            # desk that stands elsewhere offers none of that workspace's actions at all.
            for session in (host, peer):
                standing = (front_workspace(session) or {}).get("room")
                check(f"{session.tag} stands in the workspace the task crosses in",
                      standing == room, f"{standing} of {room}")
            # Only the peer's window is looked at while the task travels: an app that has
            # not drawn it has nothing to answer, and asking would test the door instead.
            focus(peer, TASKS)
            peer_tasks_drawn = frames(peer, TASKS)
            focus(host, TASKS)
            host.type(task_title)
            host.key("Enter")
            host.snapshot(out / "05-host-task.png")
            typed = until("the task the host typed to be in its own Tasks",
                          lambda: (text if task_title in (text := tasks_content(host)) else None),
                          CROSSING_BOUND, 0.5)
            check("the task typed in the host's window is in the host's own Tasks", bool(typed),
                  (typed or tasks_content(host))[:400])
            try:
                drawn = redrawn(peer, TASKS, peer_tasks_drawn, "the task that arrived")
                check("the peer's Tasks window woke and drew the task", drawn > peer_tasks_drawn,
                      f"{peer_tasks_drawn} then {drawn} frames")
            except Failed:
                check("the peer's Tasks window woke and drew the task", False,
                      f"{peer_tasks_drawn} frames, then {frames(peer, TASKS)} after "
                      f"{CROSSING_BOUND:.0f} s; the peer's session says {stumbles(peer)}")
            peer.snapshot(out / "06-peer-task.png")
            listed = tasks_content(peer, bound=30.0)
            check("the peer's own Tasks lists the task the host typed", task_title in listed,
                  f"{listed[:400]}; the peer's desk stands in "
                  f"{(front_workspace(peer) or {}).get('room')}; "
                  f"its session says {stumbles(peer)}")

            step("6", "each side's app answers from the one record")
            check("the host's own Tasks lists the task it typed", task_title in tasks_content(host))
            check("the host's Notes still holds the page it wrote",
                  note_title in answers(host, listed_action(host, NOTES, "Find note"),
                                        {"title": note_title}))
            check("the peer's Notes holds its text, read back on its own",
                  note_title in answers(peer, listed_action(peer, NOTES, "Find text"),
                                        {"text": note_body}))
        except Failed as error:
            check(f"the run finished: {error}", False)
            for session, tag in ((host, "host"), (peer, "peer")):
                log = session.state / "logs/session.log"
                if log.is_file():
                    said = [line[:300] for line in log.read_text(errors="replace").splitlines()
                            if "ERROR" in line or "WARN" in line][-12:]
                    note(f"{tag} session log: {said}")

    print()
    if failures:
        print(f"FAILED ({len(failures)}):")
        for failure in failures:
            print(f"  - {failure}")
    else:
        print("every claim held")
    print(f"artefacts: {out}")
    return 1 if failures else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAIL — {error}")
        sys.exit(1)
    except Exception as error:  # noqa: BLE001 - a demo that crashed is a failure
        import traceback
        traceback.print_exc()
        print(f"FAIL — {type(error).__name__}: {error}")
        sys.exit(1)
