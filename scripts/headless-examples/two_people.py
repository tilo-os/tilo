"""Two sessions of different people in one workspace, as the collaboration harnesses start them.

One person's session starts first, holding the workspace its own desk made; that person invites
someone else into it with the desk's own invitation; the other person's session is started with
that link and comes into the workspace on its way up, with nobody at its desk. Each session keeps
its own state directory, its own identity and its own runtime directory, and the desk of each one
stands in the workspace they share, which is where a person works and where their session says
they are.

The two sessions are the ones `real_apps.Session` starts, so the tree's own apps are installed and
launched by the session's own launcher. What a harness does with them is its own: an edit that
crosses, a person arriving and leaving.

    from two_people import joined
    with joined(out, apps="notes") as (host, peer, workspace, link):
        ...
"""

from __future__ import annotations

import contextlib
import hashlib
import json
from pathlib import Path
from typing import Callable, Iterator

from real_apps import BUILD, Failed, Session, build, prepare, until

# The session's own provider, and the resource it publishes the workspace's commands as: the
# desk's own invitation among them.
DESK = "so.tilo.session"
WORKSPACE_COMMANDS = "session:desk"
# The variable a session joins the workspace of an invitation with, at startup.
JOIN_INVITATION = "TILO_SHELL_JOIN_INVITATION_FILE"
# How long a workspace a person joined may take to reach them: two devices on one machine, so this
# is the join's own work, not a network's round trip.
JOINING_BOUND = 240.0


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


def stand_in(session: Session, room: str) -> None:
    """Put the desk in `room`, as a person switching to the workspace does, and wait for it: a
    session says where its person is only in the workspace it is in front of."""
    front = front_workspace(session)
    if front is None or front["room"] != room:
        # The desk names a workspace by its own key, the slot it holds it in.
        session.invoke(DESK, f"{WORKSPACE_COMMANDS}\u001fswitch-workspace",
                       {"workspace": str(workspaces(session)[room]["id"])})
    until(f"the desk on {session.tag} to stand in the workspace",
          lambda: (now["room"] if (now := front_workspace(session)) and now["room"] == room
                   else None), 60, 0.25)


def device_identity(session: Session) -> str:
    """A digest of this session's device identity: a person's own computers share a person, two
    people's never do."""
    digest = hashlib.sha256()
    for path in sorted((session.state / "identity").rglob("*")):
        if path.is_file():
            digest.update(str(path.relative_to(session.state)).encode())
            digest.update(path.read_bytes())
    return digest.hexdigest() if (session.state / "identity").is_dir() else ""


@contextlib.contextmanager
def joined(
    out: Path,
    *,
    apps: str = "notes",
    tapps: Callable[[Path], list[Path]] | None = None,
    build_first: bool = False,
    remote: str | None = None,
) -> Iterator[tuple[Session, Session, dict, str]]:
    """The host's session and the peer's, in one workspace, both standing in it.

    `out` is the run directory: each session's state, its log and the invitation go under it.
    `tapps` builds what both sessions install besides `apps`, from the host's prepared state, so an
    app that is not the tree's own signed package is installed by both. What is yielded is the two
    sessions, the workspace's row from the host's desk, and the link the peer came in by; both
    sessions are stopped on the way out.
    """
    out = Path(out).resolve()
    if build_first:
        build(apps=apps, remote=remote)
    if not (BUILD / "bin/tilo-session").is_file():
        raise Failed(f"no session build in {BUILD}: pass build_first, or build "
                     "dist/dev/nested-session-build.sh")
    host, peer = Session(out / "host"), Session(out / "peer")
    try:
        for session in (host, peer):
            prepare(session.state, apps=apps)
        installed = tapps(host.state) if tapps is not None else None
        host.start(apps=apps, fresh=True, out=out, tag="host", tapps=installed)
        workspace = front_workspace(host)
        if workspace is None:
            raise Failed("the host's desk holds no workspace to invite into")
        link = invitation(host)
        invitation_path = out / "invitation.txt"
        invitation_path.write_text(link)
        peer.start(apps=apps, fresh=True, out=out, tag="peer", tapps=installed,
                   env={JOIN_INVITATION: str(invitation_path)})
        room = until("the peer to hold the host's workspace",
                     lambda: (workspace["room"] if workspace["room"] in workspaces(peer) else None),
                     JOINING_BOUND, 0.5)
        for session in (host, peer):
            stand_in(session, room)
        yield host, peer, workspace, link
    finally:
        for session in (peer, host):
            session.stop()
