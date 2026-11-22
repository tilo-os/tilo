"""Types for Agents (`so.tilo.agent`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Ask` is the app's `tl.so.tilo.agent.Ask` at `agent/<resource>`, read with
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
    "AskState",
    "Ask",
    "Step",
    "SentView",
    "Shown",
    "Reply",
    "Agents",
    "Agent",
    "WithdrawArgs",
    "ReopenArgs",
    "StopArgs",
    "SayArgs",
    "ThinkArgs",
    "StepArgs",
    "ShowViewArgs",
    "ShowArgs",
    "FinishArgs",
    "SupersedeArgs",
]

AskState = Literal["waiting", "working", "done", "failed", "stopped", "withdrawn", "cannot_read"]
"""Where an ask stands. The workspace's agent engine moves it on, except that its asker withdraws it and a person stops a turn that is working."""

WithdrawArgs = TypedDict("WithdrawArgs", {
})
"""The arguments of `withdraw`."""

ReopenArgs = TypedDict("ReopenArgs", {
})
"""The arguments of `reopen`."""

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

SayArgs = TypedDict("SayArgs", {
    "more": "str",
})
"""The arguments of `say`."""

ThinkArgs = TypedDict("ThinkArgs", {
    "more": "str",
})
"""The arguments of `think`."""

StepArgs = TypedDict("StepArgs", {
    "called": "str",
    "answered": "str",
    "failed": NotRequired["bool | None"],
})
"""The arguments of `step`."""

ShowViewArgs = TypedDict("ShowViewArgs", {
    "title": "str",
    "definition": "str",
})
"""The arguments of `show_view`."""

ShowArgs = TypedDict("ShowArgs", {
    "card": "str",
})
"""The arguments of `show`."""

FinishArgs = TypedDict("FinishArgs", {
    "state": "AskState",
})
"""The arguments of `finish`."""

SupersedeArgs = TypedDict("SupersedeArgs", {
})
"""The arguments of `supersede`."""

class Ask(Resource):
    """Ask — `tl.so.tilo.agent.Ask`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"
    mcsi6vp5: str  # agent
    @property
    def _7j5pok54(self) -> str | None:
        """said (`7j5pok54` on the wire)."""
        return self.fields["7j5pok54"]

    @_7j5pok54.setter
    def _7j5pok54(self, value: str | None) -> None:
        self.edit("7j5pok54", value=value)
    hw6dxbyx: str  # mention
    wtm364ne: AskState  # state

    def withdraw(self) -> None:
        """Takes an ask back before a device took its turn. Only its asker may."""
        return self.action("withdraw")

    def withdraw_many(self, items: list[WithdrawArgs]) -> list[dict[str, Any]]:
        """Runs `withdraw` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("withdraw", items)

    def reopen(self) -> None:
        """Asks again what was withdrawn: the mention that made it is back, as redo puts it back. Only its asker may."""
        return self.action("reopen")

    def reopen_many(self, items: list[ReopenArgs]) -> list[dict[str, Any]]:
        """Runs `reopen` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reopen", items)

    def stop(self) -> None:
        """Stops a turn that is working; what the agent did until then stays."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def answer(self) -> Reply:
        """The agent starts its reply to `a`, the ask's next attempt."""
        return self.action("answer")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"withdraw", "withdraw_many", "reopen", "reopen_many", "stop", "stop_many", "answer"})


class Step(Resource):
    """Step — `tl.so.tilo.agent.Step`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"
    ippal7k2: str  # called
    cyu3le2j: str  # answered
    cma7vgpx: bool  # failed


class SentView(Resource):
    """SentView — `tl.so.tilo.agent.SentView`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"
    gdhl4pzx: str  # title
    ytw765gn: str  # definition


class Shown(Resource):
    """Shown — `tl.so.tilo.agent.Shown`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"
    mx5wcfl2: str  # card


class Reply(Resource):
    """Reply — `tl.so.tilo.agent.Reply`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"
    gjd462ep: Ask  # ask
    oka345cb: str  # body
    czzg7mhh: str  # thinking
    hzlmnzzw: AskState  # state
    tnkzsmes: int  # attempt
    e36wbuu7: list[Entry[Step]]  # steps
    bnlsdtf6: list[Entry[SentView]]  # views
    mwwrjl52: list[Entry[Shown]]  # shown
    eyh6zmbf: bool  # superseded

    def say(self, *, more: str) -> None:
        """The agent writes `more` at the end of its answer."""
        return self.action("say", more=more)

    def say_many(self, items: list[SayArgs]) -> list[dict[str, Any]]:
        """Runs `say` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("say", items)

    def think(self, *, more: str) -> None:
        """The agent's model thought `more` on the way, as its provider shows it: at the end of the reply's thinking, beside its answer."""
        return self.action("think", more=more)

    def think_many(self, items: list[ThinkArgs]) -> list[dict[str, Any]]:
        """Runs `think` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("think", items)

    def step(self,
        *,
        called: str,
        answered: str,
        failed: bool | None = None,
    ) -> None:
        """The agent called `called` on the way, and it answered `answered`."""
        args: dict[str, Any] = {}
        args["called"] = called
        args["answered"] = answered
        if failed is not None:
            args["failed"] = failed
        return self.action("step", **args)

    def step_many(self, items: list[StepArgs]) -> list[dict[str, Any]]:
        """Runs `step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("step", items)

    def show_view(self,
        *,
        title: str,
        definition: str,
    ) -> None:
        """The agent answers with a view: `definition`, called `title`."""
        return self.action("show_view", title=title, definition=definition)

    def show_view_many(self, items: list[ShowViewArgs]) -> list[dict[str, Any]]:
        """Runs `show_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_view", items)

    def show(self, *, card: str) -> None:
        """The agent shows the thing `card` projects in its answer, live."""
        return self.action("show", card=card)

    def show_many(self, items: list[ShowArgs]) -> list[dict[str, Any]]:
        """Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show", items)

    def finish(self, *, state: AskState) -> None:
        """The agent's turn ended as `state`: done, failed, or stopped when a person stopped it."""
        return self.action("finish", state=state)

    def finish_many(self, items: list[FinishArgs]) -> list[dict[str, Any]]:
        """Runs `finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("finish", items)

    def supersede(self) -> None:
        """Another device's answer to the same ask stands instead of `r`."""
        return self.action("supersede")

    def supersede_many(self, items: list[SupersedeArgs]) -> list[dict[str, Any]]:
        """Runs `supersede` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("supersede", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"say", "say_many", "think", "think_many", "step", "step_many", "show_view", "show_view_many", "show", "show_many", "finish", "finish_many", "supersede", "supersede_many"})


class Agents(Resource):
    """Agents — `tl.so.tilo.agent.Agents`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"
    mzzoxssv: list[Entry[Ask]]  # asks
    @property
    def _353i7jsi(self) -> list[Entry[Reply]]:
        """replies (`353i7jsi` on the wire)."""
        return self.fields["353i7jsi"]

    @_353i7jsi.setter
    def _353i7jsi(self, value: list[Entry[Reply]]) -> None:
        self.edit("353i7jsi", value=value)

    def ask(self,
        *,
        agent: str,
        said: str,
        mention: str | None = None,
    ) -> Ask:
        """Asks `agent` about the resource the ask is made from (`.made_from(r)`). Whoever makes it decides first whether the agent may read that resource, and asks with nothing said when it may not: the ask then says so, and holds nothing of the resource. An ask made from a resource is named by it and the mention: every device that asks it so makes the same ask, and asking again is that ask."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        args["said"] = said
        if mention is not None:
            args["mention"] = mention
        return self.action("ask", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"ask"})


class Agent(Resource):
    """Agents — `tl.so.tilo.agent`."""
    __surface__ = "so.tilo.agent@ae7c7a90e4ca67e5"

