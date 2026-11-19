"""Types for Terminal (`so.tilo.terminal`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Session` is the app's `terminal.session` at `terminal/<resource>`, read with
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
    "Command",
    "CommandItem",
    "CommandRequest",
    "FinishedCommand",
    "ItemKind",
    "Ran",
    "Remembered",
    "Session",
    "History",
    "RecentEnvironments",
    "StopArgs",
    "RecordCommandArgs",
    "RecordTargetArgs",
    "RecordRelaySessionArgs",
    "ClaimPaneArgs",
    "RememberArgs",
    "UsedArgs",
]

class Command(TypedDict):
    command: str
    cwd: str
    environment: str | None
    actor: str | None
    for_person: str | None
    started_ms: int
    ended_ms: int
    exit: int
    output: Any
    truncated: bool
    tool: str
    undoable: bool
    items: list[CommandItem]

class CommandItem(TypedDict):
    kind: ItemKind
    text: str
    path: str | None
    line: int | None
    column: int | None
    code: str | None

class CommandRequest(TypedDict):
    command: str
    requested_by: str | None
    for_person: str | None
    pane: str
    agent: bool

class FinishedCommand(TypedDict):
    command: str
    cwd: str
    started_ms: int
    ended_ms: int
    exit: int
    output: Any | None
    truncated: bool
    tool: str
    undoable: bool
    items: list[CommandItem]
    request: str | None
    id: str

ItemKind = Literal["error", "warning", "location", "url", "test-passed", "test-failed", "test-ignored", "change"]

class Ran(TypedDict):
    command: str
    cwd: str
    environment: str | None
    exit: int
    ended_ms: int
    actor: Any | None
    session: str
    run: str

class Remembered(TypedDict):
    command: str
    cwd: str
    environment: str | None
    exit: int
    ended_ms: int
    actor: str | None
    session: str
    run: str

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

RecordCommandArgs = TypedDict("RecordCommandArgs", {
    "command": "str",
    "cwd": "str",
    "started_ms": "int",
    "ended_ms": "int",
    "exit": "int",
    "output": NotRequired["Any | None"],
    "truncated": "bool",
    "tool": "str",
    "undoable": "bool",
    "items": "list[CommandItem]",
    "request": NotRequired["str | None"],
    "id": "str",
})
"""The arguments of `record-command`."""

RecordTargetArgs = TypedDict("RecordTargetArgs", {
    "target": "str",
    "device": NotRequired["str | None"],
})
"""The arguments of `record-target`."""

RecordRelaySessionArgs = TypedDict("RecordRelaySessionArgs", {
    "session": NotRequired["str | None"],
})
"""The arguments of `record-relay-session`."""

ClaimPaneArgs = TypedDict("ClaimPaneArgs", {
    "pane": "str",
    "device": NotRequired["str | None"],
})
"""The arguments of `claim-pane`."""

RememberArgs = TypedDict("RememberArgs", {
    "command": "str",
    "cwd": "str",
    "environment": NotRequired["str | None"],
    "exit": "int",
    "ended_ms": "int",
    "actor": NotRequired["Any | None"],
    "session": "str",
    "run": "str",
})
"""The arguments of `remember`."""

UsedArgs = TypedDict("UsedArgs", {
    "device": "str",
    "target": "str",
})
"""The arguments of `used`."""

class Session(Resource):
    """Terminal — `terminal.session`."""
    __surface__ = "so.tilo.terminal@2222358fc45cc0b3"
    @property
    def target(self) -> str | None:
        """Read only."""
        return self.fields["target"]
    @property
    def relay_session(self) -> str | None:
        """Read only."""
        return self.fields["relay_session"]
    @property
    def pane(self) -> str | None:
        """Read only."""
        return self.fields["pane"]
    @property
    def device(self) -> str | None:
        """Read only."""
        return self.fields["device"]
    @property
    def holder(self) -> str | None:
        """Read only."""
        return self.fields["holder"]
    @property
    def commands(self) -> list[Entry[Command]]:
        """Read only."""
        return self.fields["commands"]

    def run(self, *, command: str) -> Any:
        """Runs the app's action."""
        return self.action("run", command=command)

    def rerun(self, *, command: str) -> Any:
        """Runs the app's action."""
        return self.action("rerun", command=command)

    def rerun_test(self,
        *,
        command: str,
        test: str,
    ) -> Any:
        """Runs the app's action."""
        return self.action("rerun-test", command=command, test=test)

    def stop(self) -> None:
        """Runs the app's action."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def record_command(self,
        *,
        command: str,
        cwd: str,
        started_ms: int,
        ended_ms: int,
        exit: int,
        truncated: bool,
        tool: str,
        undoable: bool,
        items: list[CommandItem],
        id: str,
        output: Any | None = None,
        request: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["command"] = command
        args["cwd"] = cwd
        args["started_ms"] = started_ms
        args["ended_ms"] = ended_ms
        args["exit"] = exit
        if output is not None:
            args["output"] = output
        args["truncated"] = truncated
        args["tool"] = tool
        args["undoable"] = undoable
        args["items"] = items
        if request is not None:
            args["request"] = request
        args["id"] = id
        return self.action("record-command", **args)

    def record_command_many(self, items: list[RecordCommandArgs]) -> list[dict[str, Any]]:
        """Runs `record-command` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-command", items)

    def record_target(self,
        *,
        target: str,
        device: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["target"] = target
        if device is not None:
            args["device"] = device
        return self.action("record-target", **args)

    def record_target_many(self, items: list[RecordTargetArgs]) -> list[dict[str, Any]]:
        """Runs `record-target` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-target", items)

    def record_relay_session(self,
        *,
        session: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if session is not None:
            args["session"] = session
        return self.action("record-relay-session", **args)

    def record_relay_session_many(self, items: list[RecordRelaySessionArgs]) -> list[dict[str, Any]]:
        """Runs `record-relay-session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-relay-session", items)

    def claim_pane(self,
        *,
        pane: str,
        device: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["pane"] = pane
        if device is not None:
            args["device"] = device
        return self.action("claim-pane", **args)

    def claim_pane_many(self, items: list[ClaimPaneArgs]) -> list[dict[str, Any]]:
        """Runs `claim-pane` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("claim-pane", items)

    def inspect_session(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-session")

    def read_screen(self) -> None:
        """Runs the app's action."""
        return self.action("read-screen")

    def inspect_editor(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-editor")

    def copy_visible(self) -> None:
        """Runs the app's action."""
        return self.action("copy-visible")

    def copy_selection(self) -> None:
        """Runs the app's action."""
        return self.action("copy-selection")

    def select_all(self) -> None:
        """Runs the app's action."""
        return self.action("select-all")

    def history_top(self) -> None:
        """Runs the app's action."""
        return self.action("history-top")

    def history_live(self) -> None:
        """Runs the app's action."""
        return self.action("history-live")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"run", "rerun", "rerun_test", "stop", "stop_many", "record_command", "record_command_many", "record_target", "record_target_many", "record_relay_session", "record_relay_session_many", "claim_pane", "claim_pane_many", "inspect_session", "read_screen", "inspect_editor", "copy_visible", "copy_selection", "select_all", "history_top", "history_live"})


class History(Resource):
    """Command history — `terminal.history`."""
    __surface__ = "so.tilo.terminal@2222358fc45cc0b3"
    @property
    def remembered(self) -> list[Entry[Remembered]]:
        """Read only."""
        return self.fields["remembered"]

    def remember(self,
        *,
        command: str,
        cwd: str,
        exit: int,
        ended_ms: int,
        session: str,
        run: str,
        environment: str | None = None,
        actor: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["command"] = command
        args["cwd"] = cwd
        if environment is not None:
            args["environment"] = environment
        args["exit"] = exit
        args["ended_ms"] = ended_ms
        if actor is not None:
            args["actor"] = actor
        args["session"] = session
        args["run"] = run
        return self.action("remember", **args)

    def remember_many(self, items: list[RememberArgs]) -> list[dict[str, Any]]:
        """Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"remember", "remember_many"})


class RecentEnvironments(Resource):
    """Recent environments — `terminal.recent-environments`."""
    __surface__ = "so.tilo.terminal@2222358fc45cc0b3"
    devices: dict[str, str]

    def used(self,
        *,
        device: str,
        target: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("used", device=device, target=target)

    def used_many(self, items: list[UsedArgs]) -> list[dict[str, Any]]:
        """Runs `used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("used", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"used", "used_many"})


