"""Types for Console (`so.tilo.console`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `console/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "Notice",
    "SessionLink",
    "AgentCard",
    "Window",
    "Console",
    "StopAgentArgs",
    "TakeOverAgentArgs",
    "UndoAgentSessionArgs",
    "LetAgentGoOnArgs",
    "PublishArgs",
]

StopAgentArgs = TypedDict("StopAgentArgs", {
})
"""The arguments of `stop_agent`."""

TakeOverAgentArgs = TypedDict("TakeOverAgentArgs", {
})
"""The arguments of `take_over_agent`."""

UndoAgentSessionArgs = TypedDict("UndoAgentSessionArgs", {
})
"""The arguments of `undo_agent_session`."""

LetAgentGoOnArgs = TypedDict("LetAgentGoOnArgs", {
})
"""The arguments of `let_agent_go_on`."""

PublishArgs = TypedDict("PublishArgs", {
})
"""The arguments of `publish`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.console@25a248c332df2e65"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class SessionLink(Resource):
    """SessionLink — `tl.so.tilo.console.SessionLink`."""
    __surface__ = "so.tilo.console@25a248c332df2e65"
    acxv3wxd: str  # run
    @property
    def _2pulq7rs(self) -> str:
        """instruction (`2pulq7rs` on the wire)."""
        return self.fields["2pulq7rs"]

    @_2pulq7rs.setter
    def _2pulq7rs(self, value: str) -> None:
        self.edit("2pulq7rs", value=value)


class AgentCard(Resource):
    """AgentCard — `tl.so.tilo.console.AgentCard`."""
    __surface__ = "so.tilo.console@25a248c332df2e65"
    mpwn6fmy: str  # client

    def stop_agent(self) -> None:
        """Stops the client's working session, keeping its grant."""
        return self.action("stop_agent")

    def stop_agent_many(self, items: list[StopAgentArgs]) -> list[dict[str, Any]]:
        """Runs `stop_agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_agent", items)

    def take_over_agent(self) -> None:
        """Stops the client's session and opens what it changed last, or the workspace it worked in."""
        return self.action("take_over_agent")

    def take_over_agent_many(self, items: list[TakeOverAgentArgs]) -> list[dict[str, Any]]:
        """Runs `take_over_agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_over_agent", items)

    def undo_agent_session(self) -> None:
        """Takes back every change of the client's newest session, when the history says it can."""
        return self.action("undo_agent_session")

    def undo_agent_session_many(self, items: list[UndoAgentSessionArgs]) -> list[dict[str, Any]]:
        """Runs `undo_agent_session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("undo_agent_session", items)

    def revoke_agent(self) -> None:
        """Takes back the grant the client came in by."""
        return self.action("revoke_agent")

    def let_agent_go_on(self) -> None:
        """Lets a stopped client's calls through again."""
        return self.action("let_agent_go_on")

    def let_agent_go_on_many(self, items: list[LetAgentGoOnArgs]) -> list[dict[str, Any]]:
        """Runs `let_agent_go_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("let_agent_go_on", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"stop_agent", "stop_agent_many", "take_over_agent", "take_over_agent_many", "undo_agent_session", "undo_agent_session_many", "revoke_agent", "let_agent_go_on", "let_agent_go_on_many"})


class Window(Resource):
    """Window — `tl.so.tilo.console.Window`."""
    __surface__ = "so.tilo.console@25a248c332df2e65"
    e6xn3nds: str  # place
    ujzhryea: list[Entry[SessionLink]]  # sessions
    hadfiy76: list[Entry[AgentCard]]  # cards

    def publish(self) -> None:
        """Gives each of the newest sessions the history lists, and each client that acted here, a resource of its own, so each can be linked to, found and opened outside this window. An insert under a key a resource already stands under leaves that resource as it stands, so a reading that answers again changes nothing twice."""
        return self.action("publish")

    def publish_many(self, items: list[PublishArgs]) -> list[dict[str, Any]]:
        """Runs `publish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("publish", items)

    def session_link(self, *, run: str) -> SessionLink:
        """The resource of the session `run`, made now when the window has not published it yet: every way of opening a session goes through it, so the list, a link, search and another app open the same window."""
        return self.action("session_link", run=run)

    def agent_card(self, *, client: str) -> AgentCard:
        """The card of `client`, made now when the window has not published it yet: every way of opening a client's card goes through it."""
        return self.action("agent_card", client=client)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"publish", "publish_many", "session_link", "agent_card"})


class Console(Resource):
    """Console — `tl.so.tilo.console`."""
    __surface__ = "so.tilo.console@25a248c332df2e65"

