"""Types for Views (`so.tilo.lang.views`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `views/<resource>`, read with
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
    "Device",
    "Notice",
    "WrittenView",
    "Default",
    "Views",
    "LangViews",
    "KeepArgs",
    "SendArgs",
    "DiscardArgs",
    "MakeDefaultArgs",
]

Device = Literal["desktop", "tablet", "phone"]
"""A kind of device a person chooses how things show on."""

KeepArgs = TypedDict("KeepArgs", {
})
"""The arguments of `keep`."""

SendArgs = TypedDict("SendArgs", {
    "to": "Any",
})
"""The arguments of `send`."""

DiscardArgs = TypedDict("DiscardArgs", {
    "v": "WrittenView",
})
"""The arguments of `discard`."""

MakeDefaultArgs = TypedDict("MakeDefaultArgs", {
    "v": "WrittenView",
    "device": NotRequired["Device | None"],
})
"""The arguments of `make_default`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.lang.views@6049df08d107956e"
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


class WrittenView(Resource):
    """WrittenView — `tl.so.tilo.lang.views.WrittenView`."""
    __surface__ = "so.tilo.lang.views@6049df08d107956e"
    @property
    def _2fxqftu3(self) -> str:
        """title (`2fxqftu3` on the wire)."""
        return self.fields["2fxqftu3"]

    @_2fxqftu3.setter
    def _2fxqftu3(self, value: str) -> None:
        self.edit("2fxqftu3", value=value)
    fdxj35iu: str  # question
    @property
    def _3pj7zyxo(self) -> str:
        """definition (`3pj7zyxo` on the wire)."""
        return self.fields["3pj7zyxo"]

    @_3pj7zyxo.setter
    def _3pj7zyxo(self, value: str) -> None:
        self.edit("3pj7zyxo", value=value)
    @property
    def _3eioszed(self) -> str:
        """author (`3eioszed` on the wire)."""
        return self.fields["3eioszed"]

    @_3eioszed.setter
    def _3eioszed(self, value: str) -> None:
        self.edit("3eioszed", value=value)
    loizxjpf: bool  # kept
    bckibjl6: str  # shows

    def keep(self) -> None:
        """Keeps a view among the person's own."""
        return self.action("keep")

    def keep_many(self, items: list[KeepArgs]) -> list[dict[str, Any]]:
        """Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep", items)

    def send(self, *, to: Any) -> None:
        """Sends a view to a channel: anyone there can add it to their own views."""
        return self.action("send", to=to)

    def send_many(self, items: list[SendArgs]) -> list[dict[str, Any]]:
        """Runs `send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep", "keep_many", "send", "send_many"})


class Default(Resource):
    """Default — `tl.so.tilo.lang.views.Default`."""
    __surface__ = "so.tilo.lang.views@6049df08d107956e"
    @property
    def _2atmuo4h(self) -> str:
        """shows (`2atmuo4h` on the wire)."""
        return self.fields["2atmuo4h"]

    @_2atmuo4h.setter
    def _2atmuo4h(self, value: str) -> None:
        self.edit("2atmuo4h", value=value)
    h6n4gvgn: Device  # device
    @property
    def _7rv6o4rt(self) -> WrittenView:
        """chosen (`7rv6o4rt` on the wire)."""
        return self.fields["7rv6o4rt"]

    @_7rv6o4rt.setter
    def _7rv6o4rt(self, value: WrittenView) -> None:
        self.edit("7rv6o4rt", value=value)


class Views(Resource):
    """Views — `tl.so.tilo.lang.views.Views`."""
    __surface__ = "so.tilo.lang.views@6049df08d107956e"
    bweal36x: list[Entry[WrittenView]]  # items
    @property
    def _2tnozirr(self) -> list[Entry[Default]]:
        """defaults (`2tnozirr` on the wire)."""
        return self.fields["2tnozirr"]

    @_2tnozirr.setter
    def _2tnozirr(self, value: list[Entry[Default]]) -> None:
        self.edit("2tnozirr", value=value)

    def answer(self,
        *,
        title: str,
        question: str,
        definition: str,
        shows: str | None = None,
    ) -> WrittenView:
        """Records the view an agent answered with, under the name of whoever asked. It opens beside what the person pointed at; until they keep it, it is the agent's answer."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["question"] = question
        args["definition"] = definition
        if shows is not None:
            args["shows"] = shows
        return self.action("answer", **args)

    def write(self,
        *,
        title: str,
        definition: str,
        shows: str | None = None,
    ) -> WrittenView:
        """A view the person writes, kept among their own."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["definition"] = definition
        if shows is not None:
            args["shows"] = shows
        return self.action("write", **args)

    def discard(self, *, v: WrittenView) -> None:
        """Throws a view away, and with it how a type showed through it."""
        return self.action("discard", v=v)

    def discard_many(self, items: list[DiscardArgs]) -> list[dict[str, Any]]:
        """Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("discard", items)

    def make_default(self,
        *,
        v: WrittenView,
        device: Device | None = None,
    ) -> None:
        """Makes a view how the person shows its type on a kind of device."""
        args: dict[str, Any] = {}
        args["v"] = v
        if device is not None:
            args["device"] = device
        return self.action("make_default", **args)

    def make_default_many(self, items: list[MakeDefaultArgs]) -> list[dict[str, Any]]:
        """Runs `make_default` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("make_default", items)

    def add(self, *, m: Any) -> WrittenView:
        """Adds the view a message sent to the person's own views. Its author is who sent the message, as the log recorded them, never who adds it: what it does asks first unless the person trusts that sender. It is a copy: a later change the sender makes to theirs does not reach it."""
        return self.action("add", m=m)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"answer", "write", "discard", "discard_many", "make_default", "make_default_many", "add"})


class LangViews(Resource):
    """Views — `tl.so.tilo.lang.views`."""
    __surface__ = "so.tilo.lang.views@6049df08d107956e"

