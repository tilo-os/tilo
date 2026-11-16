"""Types for Terminal (`so.tilo.terminal`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Session` is the app's `terminal.session` at `terminal/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one run.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Resource, Run

__all__ = [
    "ItemKind",
    "Run",
    "RunItem",
    "RunRequest",
    "Remembered",
    "Session",
    "History",
    "RecentEnvironments",
    "StopArgs",
    "RecordRunArgs",
    "RecordTargetArgs",
    "RecordRelaySessionArgs",
    "ClaimPaneArgs",
    "RememberArgs",
    "UsedArgs",
]

ItemKind = Literal["error", "warning", "location", "url", "test-passed", "test-failed", "test-ignored", "change"]

class Run(TypedDict):
    command: str
    cwd: str
    environment: str | None
    actor: str
    for_person: str | None
    started_ms: int
    ended_ms: int
    exit: int
    output: Any
    truncated: bool
    tool: str
    undoable: bool
    items: list[RunItem]

class RunItem(TypedDict):
    kind: ItemKind
    text: str
    path: str | None
    line: int | None
    column: int | None
    code: str | None

class RunRequest(TypedDict):
    command: str
    requested_by: str
    for_person: str | None
    pane: str
    agent: bool

class Remembered(TypedDict):
    command: str
    cwd: str
    environment: str | None
    exit: int
    ended_ms: int
    actor: str
    session: str
    run: str

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

RecordRunArgs = TypedDict("RecordRunArgs", {
    "command": "str",
    "cwd": "str",
    "started_ms": "int",
    "ended_ms": "int",
    "exit": "int",
    "output": NotRequired["Any | None"],
    "truncated": "bool",
    "tool": "str",
    "undoable": "bool",
    "items": "list[Any]",
    "request": NotRequired["str | None"],
    "run": "str",
})
"""The arguments of `record-run`."""

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
    "actor": "str",
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
    __surface__ = "so.tilo.terminal@54ffe4e754ac58fe"
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
    def runs(self) -> list[Run]:
        """Read only."""
        return self.fields["runs"]

    def run(self, *, command: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("run", command=command)

    def rerun(self, *, runs: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("rerun", runs=runs)

    def rerun_test(self,
        *,
        runs: str,
        test: str,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("rerun-test", runs=runs, test=test)

    def stop(self) -> Run | None:
        """Runs the app's action."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("stop", items)

    def record_run(self,
        *,
        command: str,
        cwd: str,
        started_ms: int,
        ended_ms: int,
        exit: int,
        truncated: bool,
        tool: str,
        undoable: bool,
        items: list[Any],
        run: str,
        output: Any | None = None,
        request: str | None = None,
    ) -> Run | None:
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
        args["run"] = run
        return self.action("record-run", **args)

    def record_run_many(self, items: list[RecordRunArgs]) -> list[dict[str, Any]]:
        """Runs `record-run` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("record-run", items)

    def record_target(self,
        *,
        target: str,
        device: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["target"] = target
        if device is not None:
            args["device"] = device
        return self.action("record-target", **args)

    def record_target_many(self, items: list[RecordTargetArgs]) -> list[dict[str, Any]]:
        """Runs `record-target` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("record-target", items)

    def record_relay_session(self,
        *,
        session: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if session is not None:
            args["session"] = session
        return self.action("record-relay-session", **args)

    def record_relay_session_many(self, items: list[RecordRelaySessionArgs]) -> list[dict[str, Any]]:
        """Runs `record-relay-session` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("record-relay-session", items)

    def claim_pane(self,
        *,
        pane: str,
        device: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["pane"] = pane
        if device is not None:
            args["device"] = device
        return self.action("claim-pane", **args)

    def claim_pane_many(self, items: list[ClaimPaneArgs]) -> list[dict[str, Any]]:
        """Runs `claim-pane` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
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

    def open_link(self, *, url: str) -> None:
        """Runs the app's action."""
        return self.action("open-link", url=url)


class History(Resource):
    """Command history — `terminal.history`."""
    __surface__ = "so.tilo.terminal@54ffe4e754ac58fe"
    @property
    def runs(self) -> list[Remembered]:
        """Read only."""
        return self.fields["runs"]

    def remember(self,
        *,
        command: str,
        cwd: str,
        exit: int,
        ended_ms: int,
        actor: str,
        session: str,
        run: str,
        environment: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["command"] = command
        args["cwd"] = cwd
        if environment is not None:
            args["environment"] = environment
        args["exit"] = exit
        args["ended_ms"] = ended_ms
        args["actor"] = actor
        args["session"] = session
        args["run"] = run
        return self.action("remember", **args)

    def remember_many(self, items: list[RememberArgs]) -> list[dict[str, Any]]:
        """Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remember", items)


class RecentEnvironments(Resource):
    """Recent environments — `terminal.recent-environments`."""
    __surface__ = "so.tilo.terminal@54ffe4e754ac58fe"
    devices: dict[str, str]

    def used(self,
        *,
        device: str,
        target: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("used", device=device, target=target)

    def used_many(self, items: list[UsedArgs]) -> list[dict[str, Any]]:
        """Runs `used` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("used", items)


