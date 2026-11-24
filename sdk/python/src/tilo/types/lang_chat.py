"""Types for Chat (`so.tilo.lang.chat`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Conversations` is the app's `tl.so.tilo.lang.chat.Conversations` at `chat/<resource>`, read with
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
    "Conversations",
    "Category",
    "TextChannel",
    "Message",
    "Thread",
    "Direct",
    "Part",
    "Chat",
    "DeleteArgs",
    "PinArgs",
    "ReactArgs",
    "ThreadDeleteArgs",
    "DirectDeleteArgs",
    "ChatDeleteArgs",
]

DeleteArgs = TypedDict("DeleteArgs", {
    "to": "Direct | TextChannel | Thread",
    "m": "Message",
})
"""The arguments of `delete`."""

PinArgs = TypedDict("PinArgs", {
    "pinned": NotRequired["bool | None"],
})
"""The arguments of `pin`."""

ReactArgs = TypedDict("ReactArgs", {
    "emoji": "str",
})
"""The arguments of `react`."""

ThreadDeleteArgs = TypedDict("ThreadDeleteArgs", {
    "to": "Direct | TextChannel | Thread",
    "m": "Message",
})
"""The arguments of `delete`."""

DirectDeleteArgs = TypedDict("DirectDeleteArgs", {
    "to": "Direct | TextChannel | Thread",
    "m": "Message",
})
"""The arguments of `delete`."""

ChatDeleteArgs = TypedDict("ChatDeleteArgs", {
    "to": "Direct | TextChannel | Thread",
    "m": "Message",
})
"""The arguments of `delete`."""

class Conversations(Resource):
    """Conversations — `tl.so.tilo.lang.chat.Conversations`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    @property
    def _5kzdptyg(self) -> list[Entry[Category | Direct | TextChannel]]:
        """channels (`5kzdptyg` on the wire)."""
        return self.fields["5kzdptyg"]

    @_5kzdptyg.setter
    def _5kzdptyg(self, value: list[Entry[Category | Direct | TextChannel]]) -> None:
        self.edit("5kzdptyg", value=value)

    def new_channel(self,
        *,
        name: str | None = None,
    ) -> TextChannel:
        """Adds a text channel to the sidebar."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_channel", **args)

    def new_category(self,
        *,
        name: str | None = None,
    ) -> Category:
        """Adds a category to the sidebar."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_category", **args)

    def message_person(self, *, who: str) -> Direct:
        """Opens the direct message with `who`, in the sidebar, or the one already open with them: a person has one conversation, not a new empty one each time."""
        return self.action("message_person", who=who)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_channel", "new_category", "message_person"})


class Category(Resource):
    """Category — `tl.so.tilo.lang.chat.Category`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    hbhrds5c: str  # name
    hvqgmwrk: list[Entry[TextChannel]]  # channels

    def add_channel(self,
        *,
        name: str | None = None,
    ) -> TextChannel:
        """Adds a text channel to a category."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("add_channel", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_channel"})


class TextChannel(Resource):
    """TextChannel — `tl.so.tilo.lang.chat.TextChannel`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    x3haugpo: str  # name
    u6b35mgm: str  # topic
    ksuxpopi: list[Entry[Message]]  # messages
    ztou5nin: list[Entry[Thread]]  # threads

    def send(self,
        *,
        to: Direct | TextChannel | Thread,
        body: str,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("send", **args)

    def send_view(self,
        *,
        to: Direct | TextChannel | Thread,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", to=to, title=title, definition=definition)

    def delete(self,
        *,
        to: Direct | TextChannel | Thread,
        m: Message,
    ) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable."""
        return self.action("delete", to=to, m=m)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def start_thread(self,
        *,
        c: Direct | TextChannel,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["c"] = c
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "delete", "delete_many", "start_thread"})


class Message(Resource):
    """Message — `tl.so.tilo.lang.chat.Message`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    xfrqdy3y: str  # body
    @property
    def _4uwxpnw2(self) -> Message | None:
        """reply_to (`4uwxpnw2` on the wire)."""
        return self.fields["4uwxpnw2"]

    @_4uwxpnw2.setter
    def _4uwxpnw2(self, value: Message | None) -> None:
        self.edit("4uwxpnw2", value=value)
    kxekobh6: Thread | None  # thread
    mwily5kw: list[Entry[Part]]  # parts
    @property
    def _4zr7hti6(self) -> bool:
        """pinned (`4zr7hti6` on the wire)."""
        return self.fields["4zr7hti6"]

    @_4zr7hti6.setter
    def _4zr7hti6(self, value: bool) -> None:
        self.edit("4zr7hti6", value=value)
    ofad24rc: dict[str, list[str]]  # reactions
    dh6346no: str  # sent_view
    tz5yw5dt: str  # by
    mmlcexsa: int  # written
    jcgolgdf: str  # reacted

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.
    def pin(self,
        *,
        pinned: bool | None = None,
    ) -> None:
        """Pins a message to its channel, or unpins it."""
        args: dict[str, Any] = {}
        if pinned is not None:
            args["pinned"] = pinned
        return self.action("pin", **args)

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def react(self, *, emoji: str) -> None:
        """Adds your reaction with an emoji to a message, or takes it back when you had reacted with it."""
        return self.action("react", emoji=emoji)

    def react_many(self, items: list[ReactArgs]) -> list[dict[str, Any]]:
        """Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("react", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"pin", "pin_many", "react", "react_many"})


class Thread(Resource):
    """Thread — `tl.so.tilo.lang.chat.Thread`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    xuvfifa5: str  # name
    ijy454g6: list[Entry[Message]]  # messages
    @property
    def _2ki7h2z5(self) -> bool:
        """archived (`2ki7h2z5` on the wire)."""
        return self.fields["2ki7h2z5"]

    @_2ki7h2z5.setter
    def _2ki7h2z5(self, value: bool) -> None:
        self.edit("2ki7h2z5", value=value)

    def send(self,
        *,
        to: Direct | TextChannel | Thread,
        body: str,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("send", **args)

    def send_view(self,
        *,
        to: Direct | TextChannel | Thread,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", to=to, title=title, definition=definition)

    def delete(self,
        *,
        to: Direct | TextChannel | Thread,
        m: Message,
    ) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable."""
        return self.action("delete", to=to, m=m)

    def delete_many(self, items: list[ThreadDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "delete", "delete_many"})


class Direct(Resource):
    """Direct — `tl.so.tilo.lang.chat.Direct`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    @property
    def _5stv6ro7(self) -> str:
        """name (`5stv6ro7` on the wire)."""
        return self.fields["5stv6ro7"]

    @_5stv6ro7.setter
    def _5stv6ro7(self, value: str) -> None:
        self.edit("5stv6ro7", value=value)
    ycadyqh5: str  # person
    y65whtrn: list[Entry[Message]]  # messages
    @property
    def _3p73kpuw(self) -> list[Entry[Thread]]:
        """threads (`3p73kpuw` on the wire)."""
        return self.fields["3p73kpuw"]

    @_3p73kpuw.setter
    def _3p73kpuw(self, value: list[Entry[Thread]]) -> None:
        self.edit("3p73kpuw", value=value)

    def send(self,
        *,
        to: Direct | TextChannel | Thread,
        body: str,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("send", **args)

    def send_view(self,
        *,
        to: Direct | TextChannel | Thread,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", to=to, title=title, definition=definition)

    def delete(self,
        *,
        to: Direct | TextChannel | Thread,
        m: Message,
    ) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable."""
        return self.action("delete", to=to, m=m)

    def delete_many(self, items: list[DirectDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def start_thread(self,
        *,
        c: Direct | TextChannel,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["c"] = c
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "delete", "delete_many", "start_thread"})


class Part(Resource):
    """Part — `tl.so.tilo.lang.chat.Part`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    zevnbs3i: str  # kind
    @property
    def _3hkzkqvr(self) -> str:
        """body (`3hkzkqvr` on the wire)."""
        return self.fields["3hkzkqvr"]

    @_3hkzkqvr.setter
    def _3hkzkqvr(self, value: str) -> None:
        self.edit("3hkzkqvr", value=value)


class Chat(Resource):
    """Chat — `tl.so.tilo.lang.chat`."""
    __surface__ = "so.tilo.lang.chat@56a96c9bab42d4c0"
    def send(self,
        *,
        to: Direct | TextChannel | Thread,
        body: str,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("send", **args)

    def send_view(self,
        *,
        to: Direct | TextChannel | Thread,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", to=to, title=title, definition=definition)

    def delete(self,
        *,
        to: Direct | TextChannel | Thread,
        m: Message,
    ) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable."""
        return self.action("delete", to=to, m=m)

    def delete_many(self, items: list[ChatDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def start_thread(self,
        *,
        c: Direct | TextChannel,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["c"] = c
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "delete", "delete_many", "start_thread"})


