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
import shutil
import sys
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE / "headless-examples"))
from real_apps import BUILD, Failed, Session, build, prepare, run, until  # noqa: E402

# The native app: a package the tree builds, launched by the session's launcher.
NOTES = "so.tilo.notes"
# The tilolang app: Tasks, installed from its own signed package.
TASKS = "so.tilo.lang.tasks"
TASKS_PACKAGE = REPO / "apps/tl/tasks"
# The session's own provider, and the two resources it publishes the desk as: the
# desk's windows (focus), and the workspace's own commands (its invitation).
DESK = "so.tilo.session"
WINDOWS = "desktop:desk"
WORKSPACE_COMMANDS = "session:desk"
# The variable a session joins the workspace of an invitation with, at startup.
JOIN_INVITATION = "TILO_SHELL_JOIN_INVITATION_FILE"
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


def workspaces(session: Session) -> dict[str, dict]:
    """The workspaces this session's desk holds, each by its durable id."""
    tree = (session.state_dump() or {}).get("workspace_tree") or {}
    return {workspace["room"]: workspace
            for workspace in tree.get("workspaces") or [] if workspace.get("room")}


def front_workspace(session: Session) -> dict | None:
    """The workspace in front on this session's desk."""
    tree = (session.state_dump() or {}).get("workspace_tree") or {}
    held = workspaces(session)
    front = tree.get("active")
    for workspace in held.values():
        if workspace.get("id") == front or workspace.get("room") == front:
            return workspace
    return next(iter(held.values()), None)


def owner_of(workspace: dict) -> str | None:
    """The person who governs a workspace, as its policy names them."""
    owner = (workspace.get("policy") or {}).get("owner")
    return owner if isinstance(owner, str) and owner else None


def members_of(workspace: dict) -> list[str]:
    """Everyone the workspace's policy names besides its owner."""
    members = (workspace.get("policy") or {}).get("members") or {}
    return sorted(members) if isinstance(members, dict) else []


def invitation(host: Session) -> str:
    """The link the host makes into the workspace in front, through its own desk action."""
    answer = host.invoke(DESK, f"{WORKSPACE_COMMANDS}\u001fissue-invitation",
                         {"role": "editor", "expires_in_seconds": 3600})
    issued = (answer.get("receipt") or {}).get("update", {}).get("output", {}).get("value") or {}
    link = issued.get("link")
    if not link:
        raise Failed(f"the desk made no invitation: {json.dumps(answer)[:1500]}")
    return link


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
    """Bring `app`'s window to the front, as the desk's own verb does."""
    window = until(f"the window of {app} on {session.tag}",
                   lambda: next((w for w in session.windows()
                                 if w.get("app_id") == app and w.get("window") is not None), None),
                   180, 0.5)
    session.invoke(DESK, f"{WINDOWS}\u001ffocus", {"tab": str(window["tab"])})
    return window


def sandboxed(session: Session, app: str) -> bool:
    """Whether `app` runs as the launcher's own sandboxed process.

    A sandboxed app's binary sits in the store the launcher mounts for it,
    `/var/tilo-apps/<id>/`, which exists in the app's own mount namespace and
    nowhere else: a process of the app running from that path is one the
    launcher made, inside the namespaces it gives it."""
    return bool(session.processes(app))


def device_identity(session: Session) -> str:
    """A digest of this session's device identity: two sessions of one person
    share it, and two people's computers never do."""
    import hashlib
    digest = hashlib.sha256()
    for path in sorted((session.state / "identity").rglob("*")):
        if path.is_file():
            digest.update(str(path.relative_to(session.state)).encode())
            digest.update(path.read_bytes())
    return digest.hexdigest() if (session.state / "identity").is_dir() else ""


def no_network(app: str) -> tuple[bool, str]:
    """Whether the package's sandbox gives `app` no network of its own, and what it says."""
    for manifest in REPO.glob("apps/*/tilo-app.toml"):
        text = manifest.read_text(errors="replace")
        if any(line.strip() in (f'id = "{app}"', f'id       = "{app}"') for line in text.splitlines()):
            net = text.split("[sandbox.net]", 1)[-1] if "[sandbox.net]" in text else ""
            mode = next((line.split("=", 1)[1].strip() for line in net.splitlines()
                         if line.strip().startswith("mode")), "")
            return mode.strip('"') == "off", f"[sandbox.net] mode = {mode}"
    return False, f"no manifest declares {app}"


def tasks_content(session: Session) -> str:
    """What Tasks on this session holds: its own `read` of the workspace's records.

    A tilolang app answers a read of the workspace's records by kind and id, and a
    read of each record of its own: the task titles are in what it answers."""
    def read_action():
        for action in session.actions("Read the content"):
            parts = (action.get("id") or "").split("\u001f")
            if action.get("application") == TASKS and parts[-2:] == [f"tl:{TASKS}", "read"]:
                return action
        return None
    try:
        action = until("Tasks' read action", read_action, 180, 0.5)
    except Failed:
        return ""
    text = answers(session, action)
    try:
        records = json.loads(text)
    except json.JSONDecodeError:
        return text
    if isinstance(records, dict):
        for record in records.get("records") or []:
            if str(record.get("kind", "")).startswith(f"tl.{TASKS}."):
                text += "\n" + answers(session, action, {"item": record["item"]})
    return text


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

    if args.build:
        build(apps=APPS, remote=args.remote_exec)
    if not (BUILD / "bin/tilo-session").is_file():
        print(f"no session build in {BUILD}: pass --build, or set TILO_NESTED_SESSION_BUILD",
              file=sys.stderr)
        return 2

    token = uuid.uuid4().hex[:8]
    note_title = f"Release plan {token}"
    note_body = f"Ship the {token} build on Friday."
    task_title = f"Book the studio for {token}"

    host = Session(out / "host")
    peer = Session(out / "peer")
    try:
        step("1", "two states, each with its own apps and runtime")
        for session, tag in ((host, "host"), (peer, "peer")):
            prepare(session.state, apps=APPS)
            note(f"{tag}: {session.state}")
        # Tasks as a tilolang package of this session's runtime: built once,
        # installed into both states as the signed package the launcher trusts.
        tapp = build_tasks(out / "build", host.state)
        note(f"Tasks package: {tapp.name}")

        step("2", "the host's session starts and holds its own workspace")
        host.start(apps=APPS, fresh=True, out=out, tag="host", tapps=[tapp])
        host_workspace = front_workspace(host)
        if host_workspace is None:
            raise Failed("the host's desk holds no workspace to invite into")
        note(f"host workspace {host_workspace['name']} ({host_workspace['room']})")
        check("the host governs the workspace it made", bool(owner_of(host_workspace)),
              json.dumps(host_workspace.get("policy"))[:600])

        step("3", "the host invites the other person into that workspace")
        link = invitation(host)
        note(f"the link is {len(link)} characters")
        link_path = out / "invitation.txt"
        link_path.write_text(link)

        step("4", "the peer starts with the link and comes into that workspace")
        peer.start(apps=APPS, fresh=True, out=out, tag="peer", tapps=[tapp],
                   env={JOIN_INVITATION: str(link_path)})
        check("the two sessions are separate processes", host.proc.pid != peer.proc.pid,
              f"{host.proc.pid} and {peer.proc.pid}")
        check("they keep their state and their runtime apart",
              host.state != peer.state and host.runtime_directory() != peer.runtime_directory(),
              f"{host.runtime_directory()} and {peer.runtime_directory()}")
        room = until("the peer to hold the host's workspace",
                     lambda: (host_workspace["room"] if host_workspace["room"] in workspaces(peer)
                              else None), 240, 0.5)
        peer_workspace = workspaces(peer)[room]
        check("the peer holds the workspace the link names",
              peer_workspace["room"], host_workspace["room"])
        check("the peer's copy names the host's person as its owner",
              owner_of(peer_workspace), owner_of(host_workspace))
        check("the two sessions are different people's computers",
              device_identity(host) != device_identity(peer) != "",
              f"{device_identity(host)[:16]} and {device_identity(peer)[:16]}")
        check("the peer's copy carries what the workspace records",
              bool(members_of(peer_workspace) or (peer_workspace.get("policy") or {}).get("grants")),
              json.dumps(peer_workspace.get("policy"))[:600])
        host.snapshot(out / "01-host-workspace.png")
        peer.snapshot(out / "02-peer-workspace.png")

        step("5", "both sessions open the native app, through their own desktops")
        for session in (host, peer):
            session.launch(NOTES)
            session.answering(NOTES, deadline=240)
            check(f"{session.tag} runs Notes as the launcher's sandboxed process",
                  sandboxed(session, NOTES))
        offline, how = no_network(NOTES)
        check("Notes' package gives it no network of its own", offline, how)

        step("6", "a note page written on the host reaches the peer's Notes")
        focus(host, NOTES)
        peer_drawn = frames(peer, NOTES)
        host.chord("Control", "n")
        host.type(note_title)
        host.key("Enter")
        host.type(note_body)
        host.key("Enter")
        host.snapshot(out / "03-host-note.png")
        find_note = listed_action(peer, NOTES, "Find note")

        def peer_finds() -> str | None:
            text = answers(peer, find_note, {"title": note_title})
            return text if note_title in text else None

        found = until("the peer's Notes to find the page", peer_finds, CROSSING_BOUND, 0.5)
        check("Notes on the peer finds the page the host wrote", bool(found), found[:500])
        check("and the text of the page crossed with it", note_body in found, found[:500])
        drawn = redrawn(peer, NOTES, peer_drawn, "the page that arrived")
        check("the peer's Notes window woke and drew it", drawn > peer_drawn,
              f"{peer_drawn} then {drawn} frames")
        peer.snapshot(out / "04-peer-note.png")

        step("7", "both sessions open the tilolang app")
        for session in (host, peer):
            session.launch(TASKS)
            session.answering(TASKS, deadline=300)
            check(f"{session.tag} runs Tasks as the launcher's sandboxed process",
                  sandboxed(session, TASKS))
        offline, how = no_network(TASKS)
        check("Tasks' package gives it no network of its own", offline, how)

        step("8", "a task typed on the host reaches the peer's Tasks")
        focus(peer, TASKS)
        peer_tasks_drawn = frames(peer, TASKS)
        focus(host, TASKS)
        host.type(task_title)
        host.key("Enter")
        host.snapshot(out / "05-host-task.png")

        def peer_lists() -> str | None:
            text = tasks_content(peer)
            return text if task_title in text else None

        listed = until("the peer's Tasks to list the new task", peer_lists, CROSSING_BOUND, 0.5)
        check("the peer's own Tasks lists the task the host typed", bool(listed), listed[:500])
        drawn = redrawn(peer, TASKS, peer_tasks_drawn, "the task that arrived")
        check("the peer's Tasks window woke and drew it", drawn > peer_tasks_drawn,
              f"{peer_tasks_drawn} then {drawn} frames")
        peer.snapshot(out / "06-peer-task.png")

        step("9", "each side's app answers from the one record")
        check("the host's own Tasks lists the task it typed", task_title in tasks_content(host))
        check("the host's Notes still holds the page it wrote",
              note_title in answers(host, listed_action(host, NOTES, "Find note"),
                                    {"title": note_title}))
        check("the peer's Notes holds the page too, read back on its own",
              note_body in answers(peer, find_note, {"title": note_title}))
    except Failed as error:
        check(f"the run finished: {error}", False)
        for session, tag in ((host, "host"), (peer, "peer")):
            log = session.state / "logs/session.log"
            if log.is_file():
                tail = [line[:300] for line in log.read_text(errors="replace").splitlines()
                        if "ERROR" in line or "WARN" in line][-12:]
                note(f"{tag} session log: {tail}")
    finally:
        for session in (host, peer):
            session.stop()

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
