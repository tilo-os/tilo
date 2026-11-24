"""One endpoint the client acts in: the account, or one workspace.

Every call here is one tool call of Tilo's agent surface, carried by the
client's transport, naming the invocation group a call is a step of when one is
open and answering what the tool answered: an action's result, the
`Invocation` that follows one, or the ask it waits on.
"""

from __future__ import annotations

import asyncio
import base64
import contextvars
import hashlib
import time
import uuid
from collections.abc import AsyncIterator, Mapping
from typing import Any

from ._errors import AskPending, ProtocolError, Refused, SurfaceChanged, Unreachable
from ._models import Event, ProgramAnswer

# What an action that made a file answers instead of a result.
CONTENT_PREFIX = "tilo.content:"
# How long an action is given to end before the `Invocation` that follows it.
WAIT_MS = 10_000
# How long one watch holds before it answers with what it has.
WATCH_MS = 15_000
# How long a status read waits before it asks again, and the longest between.
PENDING_PAUSE = 0.2
PENDING_MAX_PAUSE = 1.0
# How many bytes one piece of an upload carries, and how many go at once.
PUT_PIECE = 384 * 1024
PUTS_AT_ONCE = 8
# The phases an invocation event ends at.
ENDED = frozenset({"succeeded", "failed", "cancelled", "refused"})


def result_of(answer: Mapping[str, Any]) -> Any:
    """What an action answered: its value, or, when it gave none and made a
    file instead (Writer's `export-content`), that file's `tilo.content:`
    reference, which `ws.files.download` fetches. Several files answer a
    list of references."""
    result = answer.get("result")
    if result is not None:
        return result
    made = [
        resource
        for resource in answer.get("resources") or []
        if isinstance(resource, str) and resource.startswith(CONTENT_PREFIX)
    ]
    if len(made) == 1:
        return made[0]
    return made or None


def links_of(answer: Mapping[str, Any]) -> list[str]:
    """The record links an answer carries: a call's `link`, a group's `links`."""
    found = [answer.get("link")]
    batch = answer.get("links")
    if isinstance(batch, list):
        found.extend(batch)
    return [link for link in found if isinstance(link, str) and link]


def surface_version(app: str, surface: str) -> str:
    """An app's surface as types name it: `<app>@<the first 16 hex of its SHA-256>`."""
    return f"{app}@{hashlib.sha256(surface.encode()).hexdigest()[:16]}"


class InvocationGroup:
    """An invocation group a set of calls is attributed to: one undo step per app.

    `links` are the links to every record its calls changed, each once, in
    the order first changed; `link` is the one link when they changed one
    record, else `None`: a group across several records hands over `links`.
    A link opens the record in its app for the workspace's members and
    carries no secret.
    """

    def __init__(self, endpoint: Endpoint, title: str, id: str) -> None:
        self.endpoint = endpoint
        self.title = title
        self.id = id
        self.invocations: list[str] = []
        self.links: list[str] = []

    def note(self, answer: Mapping[str, Any]) -> None:
        invocation = answer.get("invocation")
        if isinstance(invocation, str) and invocation:
            self.invocations.append(invocation)
        for link in links_of(answer):
            if link not in self.links:
                self.links.append(link)

    @property
    def link(self) -> str | None:
        """The link to the one record it changed, or `None`."""
        return self.links[0] if len(self.links) == 1 else None

    def undo(self) -> Any:
        """Take back exactly what its calls changed, in every record.

        The workspace refuses, changing nothing, when someone else's change
        stands on top of the group's in a record.
        """
        if not self.invocations:
            raise ProtocolError("the invocation group made no call to take back")
        return self.endpoint.client.handoff(self.endpoint.undo(self.invocations[-1]))

    async def __aenter__(self) -> InvocationGroup:
        return self

    async def __aexit__(self, *_: Any) -> None:
        return None


CURRENT: contextvars.ContextVar[InvocationGroup | None] = contextvars.ContextVar(
    "tilo_invocation_group", default=None
)


class Endpoint:
    """The account's tools or one workspace's, and the group a call joins."""

    def __init__(self, client: Any, workspace: str | None = None) -> None:
        self.client = client
        self.workspace = workspace

    # -- the calls -------------------------------------------------------

    def group(self) -> InvocationGroup | None:
        """The invocation group this call is a step of, if one is open."""
        return self.client.invocation_group if self.client.sync else CURRENT.get()

    async def call(
        self, tool: str, arguments: Mapping[str, Any], *, grouped: bool = True
    ) -> Any:
        """One tool call, with the group named when one is open."""
        transport = await self.client._transport()
        group = self.group() if grouped else None
        payload = None if group is None else {"id": group.id, "title": group.title}
        return await transport.call(self.workspace, tool, dict(arguments), payload)

    # -- reading ---------------------------------------------------------

    async def ls(
        self,
        path: str | None = None,
        kind: str | None = None,
        limit: int | None = None,
        cursor: str | None = None,
    ) -> Mapping[str, Any]:
        """The apps, an app's resources, or one resource's children and actions."""
        given = {"path": path, "cursor": cursor, "kind": kind, "limit": limit}
        arguments = {name: value for name, value in given.items() if value is not None}
        answer = await self.call("tilo_ls", arguments)
        return _mapping(answer, "tilo_ls")

    async def read_raw(self, path: str, cursor: str | None = None) -> Mapping[str, Any]:
        """A resource read: its fields, its children and its cursor.

        A record read (`record:<app>:<record>`) answers its records: the
        resource is the first record's value. A read of an app answers its
        surface beside its path.
        """
        arguments: dict[str, Any] = {"path": path}
        if cursor is not None:
            arguments["cursor"] = cursor
        answer = _mapping(await self.call("tilo_read", arguments), "tilo_read")
        records = answer.get("records")
        if isinstance(records, list):
            first = next(
                (record for record in records if isinstance(record, Mapping)),
                {},
            )
            fields = first.get("value") if isinstance(first, Mapping) else {}
            return {
                "path": path,
                "fields": dict(fields) if isinstance(fields, Mapping) else {},
            }
        read = dict(answer)
        app, surface = read.get("app"), read.get("surface")
        if isinstance(app, str) and isinstance(surface, str):
            read["surface_version"] = surface_version(app, surface)
        return read

    async def view(
        self, path: str, picture: bool, scale: float | None
    ) -> Mapping[str, Any]:
        """What the app's window shows for `path`, and its picture when asked."""
        arguments: dict[str, Any] = {"path": path, "picture": bool(picture)}
        if scale is not None:
            arguments["scale"] = scale
        answer = _mapping(await self.call("tilo_view", arguments), "tilo_view")
        if not isinstance(answer.get("text"), str):
            raise ProtocolError(f"the workspace answered no view for {path}")
        return answer

    async def check_surface(self, path: str, sent: str) -> None:
        """Refuse a call whose generated types were written for another surface.

        The app is the path's first part. What it publishes is read once per
        workspace and app, and a match is kept: every later call of the same
        types costs nothing.
        """
        app = path.split("/", 1)[0]
        known = self.client.surfaces.get((self.workspace, app))
        if known == sent:
            return
        read = await self.read_raw(app)
        app_id, surface = read.get("app"), read.get("surface")
        published = (
            surface_version(app_id, surface)
            if isinstance(app_id, str) and isinstance(surface, str)
            else None
        )
        if published != sent:
            raise SurfaceChanged(
                f"these types were made for {sent}; the workspace's app publishes "
                f"{published or 'none'}",
                [f"write the types again: tilo sdk gen -w {self.workspace}"],
            )
        self.client.surfaces[(self.workspace, app)] = published

    # -- actions and invocations ----------------------------------------

    async def do_answer(
        self,
        path: str,
        arguments: Mapping[str, Any],
        *,
        surface: str | None = None,
        invocation_id: str | None = None,
        grouped: bool = True,
    ) -> Mapping[str, Any]:
        """Run the action `path` names (`app/resource#action`), and answer how it went.

        The invocation id is fresh unless one is named: a retried call names
        the same one, and the workspace runs it at most once.
        """
        if surface is not None:
            await self.check_surface(path, surface)
        sent: dict[str, Any] = {
            "path": path,
            "args": dict(arguments),
            "wait_ms": WAIT_MS,
            "invocation_id": invocation_id or str(uuid.uuid4()),
        }
        answer = await self.call("tilo_do", sent, grouped=grouped)
        return _renamed(_mapping(answer, "tilo_do"))

    async def act(
        self, path: str, args: Mapping[str, Any], *, surface: str | None = None
    ) -> Any:
        """Run the action `path` names, and follow it."""
        resource, _, action = path.rpartition("#")
        if not resource or not action:
            raise ProtocolError(f"an action's path is <resource>#<action>, not {path!r}")
        return self.follow(await self.do_answer(path, args, surface=surface), path)

    def follow(self, answer: Mapping[str, Any], path: str | None = None) -> Any:
        """What an action's answer hands back: its result, its `Invocation`, or its
        ask."""
        group = self.group()
        if group is not None:
            group.note(answer)
        status = answer.get("status")
        ask = answer.get("ask")
        if status == "asked" or (status == "pending" and isinstance(ask, Mapping)):
            invocation = answer.get("invocation")
            if not isinstance(invocation, str) or not invocation:
                raise ProtocolError(
                    "an action waits for the person without naming the invocation that follows it"
                )
            question = ask if isinstance(ask, Mapping) else answer
            url = question.get("url")
            raise AskPending(
                url if isinstance(url, str) else None,
                invocation,
                question=dict(question),
                follow=lambda: self.wait(invocation),
            )
        if status == "pending":
            invocation = answer.get("invocation")
            if not isinstance(invocation, str) or not invocation:
                raise ProtocolError(
                    "an action is pending without naming the invocation that followed it"
                )
            from ._invocation import Invocation

            return Invocation(self, invocation, path)
        return result_of(answer)

    async def status(self, invocation: str, cancel: bool = False) -> Mapping[str, Any]:
        """How an invocation stands now, or, with `cancel`, stop it."""
        arguments: dict[str, Any] = {"invocation_id": invocation}
        if cancel:
            arguments["cancel"] = True
        answer = await self.call("tilo_status", arguments, grouped=False)
        return _renamed(_mapping(answer, "tilo_status"))

    async def wait(self, invocation: str, timeout: float | None = None) -> Mapping[str, Any]:
        """Follow `invocation` until it is no longer pending.

        One waiting for its person is not done either: it moves on once they
        answer.
        """
        deadline = None if timeout is None else time.monotonic() + timeout
        pause = PENDING_PAUSE
        while True:
            answer = await self.status(invocation)
            if answer.get("status") not in ("pending", "asked"):
                return answer
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                raise ProtocolError(
                    f"invocation {invocation} is still pending after {timeout}s"
                )
            await asyncio.sleep(pause if remaining is None else min(pause, remaining))
            pause = min(pause * 2, PENDING_MAX_PAUSE)

    async def output(self, invocation: str) -> AsyncIterator[Event]:
        """An invocation's events, from started to succeeded, failed or cancelled."""
        async for event in self.watch():
            if event.kind != "invocation" or event.invocation != invocation:
                continue
            yield event
            if event.phase in ENDED:
                return

    async def undo(self, invocation: str) -> Mapping[str, Any]:
        """Take back what an invocation changed: the one call, or every call of
        its group."""
        answer = await self.call(
            "tilo_status", {"invocation_id": invocation, "undo": True}, grouped=False
        )
        return _renamed(_mapping(answer, "tilo_status"))

    async def program(self, source: str, session: str | None = None) -> ProgramAnswer:
        """Run a tilolang program as one step, in the program session `session` names (an
        earlier answer's `session`), or a new one."""
        arguments: dict[str, Any] = {"program": source}
        if session is not None:
            arguments["session"] = session
        answer = _mapping(await self.call("tilo", arguments), "tilo")
        if not answer.get("text") and isinstance(answer.get("error"), Mapping):
            answer = {**answer, "text": str(answer["error"].get("message") or "")}
        program = ProgramAnswer.of(answer)
        if program.status == "failed":
            raise Refused(
                "failed",
                "\n".join(program.output) or program.text or "the program failed",
            )
        return program

    # -- files -----------------------------------------------------------

    async def put(self, data: bytes) -> str:
        """Upload `data` and answer its `tilo.content:<hash>`."""
        begun = _mapping(
            await self.call("tilo_put_begin", {"size": len(data)}, grouped=False),
            "tilo_put_begin",
        )
        upload = begun.get("upload")
        if not isinstance(upload, int):
            raise ProtocolError("the workspace began no upload")
        pieces = [
            (offset, data[offset : offset + PUT_PIECE])
            for offset in range(0, len(data), PUT_PIECE)
        ]
        # Several pieces go at once: each names where its bytes belong.
        for start in range(0, len(pieces), PUTS_AT_ONCE):
            await asyncio.gather(
                *[
                    self.call(
                        "tilo_put_append",
                        {
                            "upload": upload,
                            "offset": offset,
                            "chunk": base64.b64encode(piece).decode(),
                        },
                        grouped=False,
                    )
                    for offset, piece in pieces[start : start + PUTS_AT_ONCE]
                ]
            )
        finished = _mapping(
            await self.call("tilo_put_finish", {"upload": upload}, grouped=False),
            "tilo_put_finish",
        )
        content = finished.get("content")
        if not isinstance(content, str) or not content:
            raise ProtocolError("the workspace took every piece without naming its content")
        return content

    async def get(self, content: str) -> bytes:
        """The bytes of `content`, a `tilo.content:<hash>`, a piece at a time."""
        pieces: list[bytes] = []
        offset = 0
        while True:
            answer = _mapping(
                await self.call("tilo_get", {"content": content, "offset": offset}, grouped=False),
                "tilo_get",
            )
            text = answer.get("bytes")
            piece = base64.b64decode(text) if isinstance(text, str) else b""
            pieces.append(piece)
            offset += len(piece)
            size = answer.get("size")
            if not piece or (isinstance(size, int) and offset >= size):
                return b"".join(pieces)

    # -- events ----------------------------------------------------------

    async def watch(self, path: str | None = None) -> AsyncIterator[Event]:
        """Each change, invocation event and presence change, resuming after the
        last."""
        cursor: str | None = None
        pause = 0.2
        while True:
            arguments: dict[str, Any] = {"wait_ms": WATCH_MS}
            if cursor is not None:
                arguments["cursor"] = cursor
            if path is not None:
                arguments["path"] = path
            began = time.monotonic()
            try:
                answer = _mapping(
                    await self.call("tilo_watch", arguments, grouped=False), "tilo_watch"
                )
            except Unreachable:
                # The connection went: the cursor picks up where it stopped.
                await asyncio.sleep(pause)
                pause = min(pause * 2, 5.0)
                continue
            following = answer.get("cursor")
            if isinstance(following, str):
                cursor = following
            events = [event for event in answer.get("events") or [] if isinstance(event, Mapping)]
            for event in events:
                yield _event_of(event, cursor)
            # A watch holds until something happens; one that answered at once with
            # nothing is asked again after a pause, never in a spin.
            if events:
                pause = 0.2
            elif time.monotonic() - began < 1.0:
                await asyncio.sleep(pause)
                pause = min(pause * 2, 5.0)


def _mapping(value: Any, tool: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ProtocolError(f"{tool} answered {type(value).__name__}, not an object")
    return value


def _renamed(answer: Mapping[str, Any]) -> Mapping[str, Any]:
    """An invocation's id as the rest of the SDK names it: `invocation`."""
    out = dict(answer)
    if "invocation_id" in out:
        out["invocation"] = out.pop("invocation_id")
    return out


def _event_of(event: Mapping[str, Any], cursor: str | None) -> Event:
    """One watch event: its kind, and everything else as its data."""
    kind = event.get("kind")
    data = {key: value for key, value in event.items() if key != "kind"}
    if "invocation_id" in data:
        data["invocation"] = data.pop("invocation_id")
    if cursor is not None:
        data["cursor"] = cursor
    return Event.of(kind if isinstance(kind, str) else "change", data)


__all__ = [
    "CONTENT_PREFIX",
    "CURRENT",
    "Endpoint",
    "InvocationGroup",
    "links_of",
    "result_of",
    "surface_version",
]
