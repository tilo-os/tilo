"""One endpoint the client calls: the account, or one workspace.

Every call here is a route of Tilo's `/v1`, naming the group a call is a step
of when one is open, and answering what the route answers: an action's
result, the `Invocation` that follows one, or the ask it waits on.
"""

from __future__ import annotations

import asyncio
import contextvars
import math
import time
from collections.abc import AsyncIterator, Mapping
from typing import Any
from urllib.parse import quote, unquote

from ._api import MAX_WAIT, refusal_of_error
from ._errors import AskPending, ProtocolError, Refused, Unreachable
from ._models import Event, ProgramAnswer
from ._transport import Response, events

# The headers a call names its group in, and the surface generated types are for.
INVOCATION_HEADER = "tilo-invocation"
INVOCATION_TITLE_HEADER = "tilo-invocation-title"
SURFACE_HEADER = "tilo-surface"
# What an action that made a file answers instead of a result.
CONTENT_PREFIX = "tilo.content:"


def segments(path: str) -> str:
    """A resource path as a route takes it: each segment percent-encoded alone.

    A name that holds a `/` is written with its own escape (`notes/Q3%2F4%20plan`),
    so it stays one segment; a name with a space or an accent is escaped here.
    """
    return "/".join(quote(unquote(part), safe="") for part in path.split("/") if part != "")


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


class InvocationGroup:
    """An invocation group a set of calls is attributed to: one undo step per
    app.

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

        The host refuses, changing nothing, when someone else's change stands
        on top of the group's in a record.
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
    """The account's routes or one workspace's, and the group a call joins."""

    def __init__(self, client: Any, workspace: str | None = None) -> None:
        self.client = client
        self.workspace = workspace

    # -- the routes ------------------------------------------------------

    def path(self, *parts: str) -> str:
        """The route `parts` name, under the account or under this workspace."""
        base = (
            "/v1"
            if self.workspace is None
            else f"/v1/workspaces/{quote(self.workspace, safe='')}"
        )
        return "/".join([base, *[part for part in parts if part != ""]])

    @property
    def api(self) -> Any:
        return self.client.api

    def group(self) -> InvocationGroup | None:
        """The invocation group this call is a step of, if one is open."""
        return self.client.invocation_group if self.client.sync else CURRENT.get()

    def invocation_headers(self) -> dict[str, str]:
        group = self.group()
        if group is None:
            return {}
        return {INVOCATION_HEADER: group.id, INVOCATION_TITLE_HEADER: group.title}

    async def call(
        self,
        method: str,
        path: str,
        *,
        query: Mapping[str, Any] | None = None,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        stream: bool = False,
        grouped: bool = True,
    ) -> Response:
        """One route, with the group named and the answer refused if it is one."""
        sent = self.invocation_headers() if grouped else {}
        sent.update(headers or {})
        return await self.api.call(
            method, path, query=query, json=json, data=data, headers=sent, stream=stream
        )

    async def json(
        self,
        method: str,
        path: str,
        *,
        query: Mapping[str, Any] | None = None,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        grouped: bool = True,
    ) -> Mapping[str, Any]:
        """One route, its answer as a JSON object."""
        answer = await self.call(
            method,
            path,
            query=query,
            json=json,
            data=data,
            headers=headers,
            grouped=grouped,
        )
        body = answer.json()
        if body is None:
            return {}
        if not isinstance(body, Mapping):
            raise ProtocolError(
                f"{method} {path} answered {type(body).__name__}, not an object"
            )
        return body

    # -- actions and invocations ----------------------------------------

    async def act(
        self, path: str, args: Mapping[str, Any], *, surface: str | None = None
    ) -> Any:
        """Run the action `path` names (`app/resource#action`), and follow it."""
        resource, _, action = path.rpartition("#")
        if not resource or not action:
            raise ProtocolError(f"an action's path is <resource>#<action>, not {path!r}")
        headers = {SURFACE_HEADER: surface} if surface is not None else {}
        answer = await self.json(
            "POST",
            self.path("resources", segments(resource), "actions", quote(action, safe="")),
            json=dict(args),
            headers=headers,
        )
        return self.follow(answer, path)

    def follow(self, answer: Mapping[str, Any], path: str | None = None) -> Any:
        """What an action's answer hands back: its result, its `Invocation`, or its
        ask."""
        group = self.group()
        if group is not None:
            group.note(answer)
        invocation = answer.get("invocation")
        if answer.get("status") != "pending":
            return result_of(answer)
        if not isinstance(invocation, str) or not invocation:
            raise ProtocolError(
                "an action is pending without naming the invocation that followed it"
            )
        ask = answer.get("ask")
        if isinstance(ask, Mapping):
            url = ask.get("url")
            raise AskPending(
                url if isinstance(url, str) else None,
                invocation,
                question=ask,
                follow=lambda: self.wait(invocation),
            )
        from ._invocation import Invocation

        return Invocation(self, invocation, path)

    def _invocation_path(self, invocation: str) -> str:
        return self.path("invocations", quote(invocation, safe=""))

    async def status(self, invocation: str, cancel: bool = False) -> Mapping[str, Any]:
        """How an invocation stands now, or, with `cancel`, stop it."""
        if cancel:
            return await self.json(
                "POST", self._invocation_path(invocation) + "/cancel", grouped=False
            )
        return await self.json(
            "GET", self._invocation_path(invocation), query={"wait": 0}, grouped=False
        )

    async def wait(self, invocation: str, timeout: float | None = None) -> Mapping[str, Any]:
        """Follow `invocation` until it is no longer pending."""
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            remaining = None if deadline is None else deadline - time.monotonic()
            hold = (
                MAX_WAIT
                if remaining is None
                else max(0, min(MAX_WAIT, math.ceil(remaining)))
            )
            answer = await self.json(
                "GET", self._invocation_path(invocation), query={"wait": hold}, grouped=False
            )
            if answer.get("status") != "pending":
                return answer
            if remaining is not None and remaining <= 0:
                raise ProtocolError(
                    f"invocation {invocation} is still pending after {timeout}s"
                )

    async def output(self, invocation: str) -> AsyncIterator[Event]:
        """An invocation's events, from started to succeeded, failed or cancelled."""
        response = await self.call(
            "GET", self._invocation_path(invocation) + "/output", stream=True, grouped=False
        )
        async for event in events(response.chunks or _none()):
            sent = _event_of(event.event, event.data)
            if sent.kind == "error":
                raise refusal_of_error(sent.raw.get("error") or sent.raw)
            yield sent
            if sent.phase in ("succeeded", "failed", "cancelled"):
                return

    async def undo(self, invocation: str) -> Any:
        """Take back what an invocation changed: the one call, or every call of
        its group."""
        return await self.json(
            "POST", self._invocation_path(invocation) + "/undo", grouped=False
        )

    async def program(
        self, source: str, confirm: bool | None = None, session: str | None = None
    ) -> ProgramAnswer:
        """Run a tilolang program as one step, in the program session `session` names (an
        earlier answer's `session`), or a new one."""
        body: dict[str, Any] = {"program": source}
        if confirm is not None:
            body["confirm"] = confirm
        if session is not None:
            body["session"] = session
        program = ProgramAnswer.of(await self.json("POST", self.path("programs"), json=body))
        if program.status == "failed":
            raise Refused(
                "failed",
                "\n".join(program.output) or "the program failed",
                ["ws.program(…, confirm=True) runs a program that waits for confirmation"],
            )
        return program

    # -- events ----------------------------------------------------------

    async def watch(self, path: str | None = None) -> AsyncIterator[Event]:
        """Each change, invocation event and presence change, resuming after the
        last."""
        cursor: str | None = None
        pause = 0.2
        while True:
            headers = {"accept": "text/event-stream"}
            if cursor is not None:
                headers["last-event-id"] = cursor
            seen = False
            try:
                response = await self.call(
                    "GET",
                    self.path("events"),
                    query={"path": path},
                    headers=headers,
                    stream=True,
                    grouped=False,
                )
                async for event in events(response.chunks or _none()):
                    if event.id:
                        cursor = event.id
                    sent = _event_of(event.event, event.data)
                    if sent.kind == "error":
                        raise refusal_of_error(sent.raw.get("error") or sent.raw)
                    seen = True
                    yield sent
            except Unreachable:
                # The connection went: the cursor picks up where it stopped.
                seen = False
            pause = 0.2 if seen else min(pause * 2, 5.0)
            await asyncio.sleep(pause)


def _event_of(kind: str, data: Any) -> Event:
    return Event.of(kind, data if isinstance(data, Mapping) else {"data": data})


async def _none() -> AsyncIterator[bytes]:
    """No bytes at all, for a stream that named none."""
    return
    yield  # pragma: no cover  (an async generator that yields nothing)


__all__ = [
    "CONTENT_PREFIX",
    "CURRENT",
    "Endpoint",
    "INVOCATION_HEADER",
    "INVOCATION_TITLE_HEADER",
    "SURFACE_HEADER",
    "InvocationGroup",
    "links_of",
    "result_of",
    "segments",
]
