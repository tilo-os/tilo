"""Types for Chat (`so.tilo.chat`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Conversations` is the app's `tl.so.tilo.chat.Conversations` at `chat/<resource>`, read with
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
    "VoiceChannel",
    "TextChannel",
    "Message",
    "Report",
    "Thread",
    "Direct",
    "Attached",
    "Part",
    "Skill",
    "Example",
    "Chat",
    "DeleteChannelArgs",
    "DeleteCategoryArgs",
    "MoveChannelArgs",
    "MoveCategoryArgs",
    "CategoryDeleteChannelArgs",
    "DeleteArgs",
    "PinArgs",
    "ReactArgs",
    "DecideArgs",
    "ThreadDeleteArgs",
    "DirectDeleteArgs",
    "CorrectSkillArgs",
    "ForgetSkillArgs",
    "KeepExampleArgs",
    "DropExampleArgs",
]

DeleteChannelArgs = TypedDict("DeleteChannelArgs", {
    "ch": "TextChannel",
})
"""The arguments of `delete_channel`."""

DeleteCategoryArgs = TypedDict("DeleteCategoryArgs", {
    "c": "Category",
})
"""The arguments of `delete_category`."""

MoveChannelArgs = TypedDict("MoveChannelArgs", {
    "ch": "TextChannel | VoiceChannel",
    "category": NotRequired["Category | None"],
    "at": NotRequired["int | None"],
})
"""The arguments of `move_channel`."""

MoveCategoryArgs = TypedDict("MoveCategoryArgs", {
    "c": "Category",
    "at": NotRequired["int | None"],
})
"""The arguments of `move_category`."""

CategoryDeleteChannelArgs = TypedDict("CategoryDeleteChannelArgs", {
    "ch": "TextChannel",
})
"""The arguments of `delete_channel`."""

DeleteArgs = TypedDict("DeleteArgs", {
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

DecideArgs = TypedDict("DecideArgs", {
    "decision": "str",
})
"""The arguments of `decide`."""

ThreadDeleteArgs = TypedDict("ThreadDeleteArgs", {
    "m": "Message",
})
"""The arguments of `delete`."""

DirectDeleteArgs = TypedDict("DirectDeleteArgs", {
    "m": "Message",
})
"""The arguments of `delete`."""

CorrectSkillArgs = TypedDict("CorrectSkillArgs", {
    "name": NotRequired["str | None"],
    "work": NotRequired["str | None"],
    "how": NotRequired["str | None"],
    "form": NotRequired["str | None"],
    "model": NotRequired["str | None"],
})
"""The arguments of `correct_skill`."""

ForgetSkillArgs = TypedDict("ForgetSkillArgs", {
})
"""The arguments of `forget_skill`."""

KeepExampleArgs = TypedDict("KeepExampleArgs", {
    "text": "str",
    "accepted": "bool",
    "note": NotRequired["str | None"],
})
"""The arguments of `keep_example`."""

DropExampleArgs = TypedDict("DropExampleArgs", {
    "example": "Example",
})
"""The arguments of `drop_example`."""

class Conversations(Resource):
    """Conversations — `tl.so.tilo.chat.Conversations`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    @property
    def _5kzdptyg(self) -> list[Entry[Category | Direct | TextChannel | VoiceChannel]]:
        """channels (`5kzdptyg` on the wire)."""
        return self.fields["5kzdptyg"]

    @_5kzdptyg.setter
    def _5kzdptyg(self, value: list[Entry[Category | Direct | TextChannel | VoiceChannel]]) -> None:
        self.edit("5kzdptyg", value=value)
    n2xgh234: list[Entry[Skill]]  # skills

    def new_channel(self,
        *,
        name: str | None = None,
        topic: str | None = None,
    ) -> TextChannel:
        """Adds a text channel to the sidebar, about `topic` when it is given."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if topic is not None:
            args["topic"] = topic
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

    def new_voice_channel(self,
        *,
        name: str | None = None,
    ) -> VoiceChannel:
        """Adds a voice channel to the sidebar."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_voice_channel", **args)

    def new_skill(self,
        *,
        name: str | None = None,
    ) -> Skill:
        """Starts a skill, called `name`. What it is for and how its work is done are written on its page, or by an agent told to share the guidance with the workspace."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_skill", **args)

    def report_problem(self, *, body: str) -> Message:
        """Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens. The people who read the reports there answer, react and start a thread under one as under any message. The channel is made under one key on every device, so reports filed apart open one."""
        return self.action("report_problem", body=body)

    def message_person(self, *, who: str) -> Direct:
        """Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time. With yourself it is your notes to self."""
        return self.action("message_person", who=who)

    def write_to(self,
        *,
        who: str,
        body: str,
    ) -> Message:
        """Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar. The message is sent as every message is."""
        return self.action("write_to", who=who, body=body)

    def delete_channel(self, *, ch: TextChannel) -> None:
        """Deletes a channel and what was said in it, from the sidebar or the category it is in."""
        return self.action("delete_channel", ch=ch)

    def delete_channel_many(self, items: list[DeleteChannelArgs]) -> list[dict[str, Any]]:
        """Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_channel", items)

    def delete_category(self, *, c: Category) -> None:
        """Deletes a category that holds no channel."""
        return self.action("delete_category", c=c)

    def delete_category_many(self, items: list[DeleteCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `delete_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_category", items)

    def move_channel(self,
        *,
        ch: TextChannel | VoiceChannel,
        category: Category | None = None,
        at: int | None = None,
    ) -> None:
        """Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among the channels listed there (the end when it is negative). A move keeps the channel and what was written in it."""
        args: dict[str, Any] = {}
        args["ch"] = ch
        if category is not None:
            args["category"] = category
        if at is not None:
            args["at"] = at
        return self.action("move_channel", **args)

    def move_channel_many(self, items: list[MoveChannelArgs]) -> list[dict[str, Any]]:
        """Runs `move_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_channel", items)

    def move_category(self,
        *,
        c: Category,
        at: int | None = None,
    ) -> None:
        """Moves a category to a place in the sidebar, at `at` among the categories and channels listed there (the end when it is negative). Its own channels move with it."""
        args: dict[str, Any] = {}
        args["c"] = c
        if at is not None:
            args["at"] = at
        return self.action("move_category", **args)

    def move_category_many(self, items: list[MoveCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `move_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_category", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_channel", "new_category", "new_voice_channel", "new_skill", "report_problem", "message_person", "write_to", "delete_channel", "delete_channel_many", "delete_category", "delete_category_many", "move_channel", "move_channel_many", "move_category", "move_category_many"})


class Category(Resource):
    """Category — `tl.so.tilo.chat.Category`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    hbhrds5c: str  # name
    hvqgmwrk: list[Entry[TextChannel | VoiceChannel]]  # channels

    def add_channel(self,
        *,
        name: str | None = None,
        topic: str | None = None,
    ) -> TextChannel:
        """Adds a text channel to a category, about `topic` when it is given."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if topic is not None:
            args["topic"] = topic
        return self.action("add_channel", **args)

    def add_voice_channel(self,
        *,
        name: str | None = None,
    ) -> VoiceChannel:
        """Adds a voice channel to a category."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("add_voice_channel", **args)

    def delete_channel(self, *, ch: TextChannel) -> None:
        """Deletes a channel and what was said in it, from the sidebar or the category it is in."""
        return self.action("delete_channel", ch=ch)

    def delete_channel_many(self, items: list[CategoryDeleteChannelArgs]) -> list[dict[str, Any]]:
        """Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_channel", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_channel", "add_voice_channel", "delete_channel", "delete_channel_many"})


class VoiceChannel(Resource):
    """VoiceChannel — `tl.so.tilo.chat.VoiceChannel`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    olqjezxu: str  # name


class TextChannel(Resource):
    """TextChannel — `tl.so.tilo.chat.TextChannel`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    x3haugpo: str  # name
    u6b35mgm: str  # topic
    ksuxpopi: list[Entry[Message]]  # messages
    ztou5nin: list[Entry[Thread]]  # threads
    kgihivmr: list[Entry[Report]]  # reports

    def send(self,
        *,
        body: str,
        reply_to: Message | None = None,
        attachments: list[Any] | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        if attachments is not None:
            args["attachments"] = attachments
        return self.action("send", **args)

    def send_view(self,
        *,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", title=title, definition=definition)

    def relay(self,
        *,
        body: str,
        by: str,
        written_in: str,
        written: int | None = None,
        attachments: list[Any] | None = None,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["body"] = body
        args["by"] = by
        args["written_in"] = written_in
        if written is not None:
            args["written"] = written
        if attachments is not None:
            args["attachments"] = attachments
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("relay", **args)

    def delete(self, *, m: Message) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted."""
        return self.action("delete", m=m)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def report(self,
        *,
        m: Message,
        reason: str | None = None,
    ) -> Report:
        """Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted."""
        args: dict[str, Any] = {}
        args["m"] = m
        if reason is not None:
            args["reason"] = reason
        return self.action("report", **args)

    def start_thread(self,
        *,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "relay", "delete", "delete_many", "report", "start_thread"})


class Message(Resource):
    """Message — `tl.so.tilo.chat.Message`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
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
    @property
    def _4da5gcom(self) -> str:
        """written_in (`4da5gcom` on the wire)."""
        return self.fields["4da5gcom"]

    @_4da5gcom.setter
    def _4da5gcom(self, value: str) -> None:
        self.edit("4da5gcom", value=value)
    mmlcexsa: int  # written
    jcgolgdf: str  # reacted
    n7d27huf: list[Entry[Attached]]  # attachments

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


class Report(Resource):
    """Report — `tl.so.tilo.chat.Report`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    jqcabz2h: Message | None  # message
    e3zqz33a: str | None  # person
    adip7bw4: str  # said
    bmfbgqt6: str  # reason
    @property
    def _3oner4zg(self) -> str:
        """decision (`3oner4zg` on the wire)."""
        return self.fields["3oner4zg"]

    @_3oner4zg.setter
    def _3oner4zg(self, value: str) -> None:
        self.edit("3oner4zg", value=value)

    def decide(self, *, decision: str) -> None:
        """Settles a report: what a moderator, or the moderation agent, decided about it."""
        return self.action("decide", decision=decision)

    def decide_many(self, items: list[DecideArgs]) -> list[dict[str, Any]]:
        """Runs `decide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("decide", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"decide", "decide_many"})


class Thread(Resource):
    """Thread — `tl.so.tilo.chat.Thread`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    xuvfifa5: str  # name
    ijy454g6: list[Entry[Message]]  # messages
    @property
    def _2ki7h2z5(self) -> bool:
        """archived (`2ki7h2z5` on the wire)."""
        return self.fields["2ki7h2z5"]

    @_2ki7h2z5.setter
    def _2ki7h2z5(self, value: bool) -> None:
        self.edit("2ki7h2z5", value=value)
    ep7fvntj: list[Entry[Report]]  # reports

    def send(self,
        *,
        body: str,
        reply_to: Message | None = None,
        attachments: list[Any] | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        if attachments is not None:
            args["attachments"] = attachments
        return self.action("send", **args)

    def send_view(self,
        *,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", title=title, definition=definition)

    def relay(self,
        *,
        body: str,
        by: str,
        written_in: str,
        written: int | None = None,
        attachments: list[Any] | None = None,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["body"] = body
        args["by"] = by
        args["written_in"] = written_in
        if written is not None:
            args["written"] = written
        if attachments is not None:
            args["attachments"] = attachments
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("relay", **args)

    def delete(self, *, m: Message) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted."""
        return self.action("delete", m=m)

    def delete_many(self, items: list[ThreadDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def report(self,
        *,
        m: Message,
        reason: str | None = None,
    ) -> Report:
        """Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted."""
        args: dict[str, Any] = {}
        args["m"] = m
        if reason is not None:
            args["reason"] = reason
        return self.action("report", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "relay", "delete", "delete_many", "report"})


class Direct(Resource):
    """Direct — `tl.so.tilo.chat.Direct`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    @property
    def _5stv6ro7(self) -> str:
        """name (`5stv6ro7` on the wire)."""
        return self.fields["5stv6ro7"]

    @_5stv6ro7.setter
    def _5stv6ro7(self, value: str) -> None:
        self.edit("5stv6ro7", value=value)
    wph25aa5: list[str]  # people
    ycadyqh5: str  # person
    y65whtrn: list[Entry[Message]]  # messages
    @property
    def _3p73kpuw(self) -> list[Entry[Thread]]:
        """threads (`3p73kpuw` on the wire)."""
        return self.fields["3p73kpuw"]

    @_3p73kpuw.setter
    def _3p73kpuw(self, value: list[Entry[Thread]]) -> None:
        self.edit("3p73kpuw", value=value)
    doiukw5i: list[Entry[Report]]  # reports

    def send(self,
        *,
        body: str,
        reply_to: Message | None = None,
        attachments: list[Any] | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds."""
        args: dict[str, Any] = {}
        args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        if attachments is not None:
            args["attachments"] = attachments
        return self.action("send", **args)

    def send_view(self,
        *,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", title=title, definition=definition)

    def relay(self,
        *,
        body: str,
        by: str,
        written_in: str,
        written: int | None = None,
        attachments: list[Any] | None = None,
        reply_to: Message | None = None,
    ) -> Message:
        """Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["body"] = body
        args["by"] = by
        args["written_in"] = written_in
        if written is not None:
            args["written"] = written
        if attachments is not None:
            args["attachments"] = attachments
        if reply_to is not None:
            args["reply_to"] = reply_to
        return self.action("relay", **args)

    def delete(self, *, m: Message) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted."""
        return self.action("delete", m=m)

    def delete_many(self, items: list[DirectDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def report(self,
        *,
        m: Message,
        reason: str | None = None,
    ) -> Report:
        """Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted."""
        args: dict[str, Any] = {}
        args["m"] = m
        if reason is not None:
            args["reason"] = reason
        return self.action("report", **args)

    def start_thread(self,
        *,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "send_view", "relay", "delete", "delete_many", "report", "start_thread"})


class Attached(Resource):
    """Attached — `tl.so.tilo.chat.Attached`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    qsdc77jc: str  # name
    h5lqkkjt: str  # media
    g7nx7a7g: int  # size
    wu2n7dpm: str  # blob


class Part(Resource):
    """Part — `tl.so.tilo.chat.Part`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    zevnbs3i: str  # kind
    @property
    def _3hkzkqvr(self) -> str:
        """body (`3hkzkqvr` on the wire)."""
        return self.fields["3hkzkqvr"]

    @_3hkzkqvr.setter
    def _3hkzkqvr(self, value: str) -> None:
        self.edit("3hkzkqvr", value=value)


class Skill(Resource):
    """Skill — `tl.so.tilo.chat.Skill`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    gfk6pm4a: str  # name
    gnfldsvz: str  # work
    bn3w5doj: str  # how
    ijpa4i7d: str  # form
    @property
    def _3wlkwi6k(self) -> str:
        """model (`3wlkwi6k` on the wire)."""
        return self.fields["3wlkwi6k"]

    @_3wlkwi6k.setter
    def _3wlkwi6k(self, value: str) -> None:
        self.edit("3wlkwi6k", value=value)
    bxx5rykt: list[Entry[Example]]  # examples

    def correct_skill(self,
        *,
        name: str | None = None,
        work: str | None = None,
        how: str | None = None,
        form: str | None = None,
        model: str | None = None,
    ) -> None:
        """Corrects a skill: only what is given changes, and what is left out stays as it is."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if work is not None:
            args["work"] = work
        if how is not None:
            args["how"] = how
        if form is not None:
            args["form"] = form
        if model is not None:
            args["model"] = model
        return self.action("correct_skill", **args)

    def correct_skill_many(self, items: list[CorrectSkillArgs]) -> list[dict[str, Any]]:
        """Runs `correct_skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("correct_skill", items)

    def forget_skill(self) -> None:
        """Forgets a skill: no later turn reads it."""
        return self.action("forget_skill")

    def forget_skill_many(self, items: list[ForgetSkillArgs]) -> list[dict[str, Any]]:
        """Runs `forget_skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_skill", items)

    def keep_example(self,
        *,
        text: str,
        accepted: bool,
        note: str | None = None,
    ) -> None:
        """Keeps an example of a skill: a result the person accepted (more like this) or rejected (less like this), with what they said about it. The same result said again is that example, moved newest, and a skill keeps its newest twelve."""
        args: dict[str, Any] = {}
        args["text"] = text
        args["accepted"] = accepted
        if note is not None:
            args["note"] = note
        return self.action("keep_example", **args)

    def keep_example_many(self, items: list[KeepExampleArgs]) -> list[dict[str, Any]]:
        """Runs `keep_example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_example", items)

    def drop_example(self, *, example: Example) -> None:
        """Takes an example out of a skill."""
        return self.action("drop_example", example=example)

    def drop_example_many(self, items: list[DropExampleArgs]) -> list[dict[str, Any]]:
        """Runs `drop_example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drop_example", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"correct_skill", "correct_skill_many", "forget_skill", "forget_skill_many", "keep_example", "keep_example_many", "drop_example", "drop_example_many"})


class Example(Resource):
    """Example — `tl.so.tilo.chat.Example`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"
    fspydtu6: str  # text
    @property
    def _2u2ngevv(self) -> bool:
        """accepted (`2u2ngevv` on the wire)."""
        return self.fields["2u2ngevv"]

    @_2u2ngevv.setter
    def _2u2ngevv(self, value: bool) -> None:
        self.edit("2u2ngevv", value=value)
    agcun6lt: str  # note
    @property
    def _3ccutu3r(self) -> int:
        """at (`3ccutu3r` on the wire)."""
        return self.fields["3ccutu3r"]

    @_3ccutu3r.setter
    def _3ccutu3r(self, value: int) -> None:
        self.edit("3ccutu3r", value=value)


class Chat(Resource):
    """Chat — `tl.so.tilo.chat`."""
    __surface__ = "so.tilo.chat@d21a7a397512be3f"

