"""Types for Code Editor (`so.tilo.code-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Project` is the app's `code_editor.project` at `code-editor/<resource>`, read with
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
    "Entry",
    "EntryKind",
    "FinishedCommand",
    "ItemKind",
    "Terminal",
    "Project",
    "NewFileArgs",
    "NewFolderArgs",
    "RenameArgs",
    "MoveEntryArgs",
    "ReplaceTextArgs",
    "ReplaceRangeArgs",
    "DeleteArgs",
    "RecordEntryArgs",
    "RecordTextArgs",
    "RecordTargetArgs",
    "RecordRelaySessionArgs",
    "ClaimPaneArgs",
    "StopArgs",
    "RecordCommandArgs",
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

class Entry(TypedDict):
    kind: EntryKind
    name: str
    parent: str | None
    language: str

EntryKind = Literal["file", "folder"]

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

class Terminal(TypedDict):
    target: str | None
    relay_session: str | None
    pane: str | None
    device: str | None
    holder: str | None
    commands: list[Entry[Command]]
    requests: Any

NewFileArgs = TypedDict("NewFileArgs", {
    "name": NotRequired["str | None"],
    "folder": NotRequired["str | None"],
    "text": NotRequired["str | None"],
})
"""The arguments of `new-file`."""

NewFolderArgs = TypedDict("NewFolderArgs", {
    "name": NotRequired["str | None"],
    "folder": NotRequired["str | None"],
})
"""The arguments of `new-folder`."""

RenameArgs = TypedDict("RenameArgs", {
    "entry": "str",
    "name": "str",
})
"""The arguments of `rename`."""

MoveEntryArgs = TypedDict("MoveEntryArgs", {
    "entry": "str",
    "folder": NotRequired["str | None"],
    "before": NotRequired["str | None"],
})
"""The arguments of `move-entry`."""

ReplaceTextArgs = TypedDict("ReplaceTextArgs", {
    "file": "str",
    "text": "str",
})
"""The arguments of `replace-text`."""

ReplaceRangeArgs = TypedDict("ReplaceRangeArgs", {
    "at": "Any",
    "text": "str",
})
"""The arguments of `replace-range`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "entry": "str",
})
"""The arguments of `delete`."""

RecordEntryArgs = TypedDict("RecordEntryArgs", {
    "id": "str",
    "kind": "EntryKind",
    "name": "str",
    "folder": NotRequired["str | None"],
    "language": "str",
})
"""The arguments of `record-entry`."""

RecordTextArgs = TypedDict("RecordTextArgs", {
    "file": "str",
    "text": "str",
})
"""The arguments of `record-text`."""

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

class Project(Resource):
    """Code Editor — `code_editor.project`."""
    __surface__ = "so.tilo.code-editor@bd2b2ca013353dfa"
    files: list[Entry[Entry]]
    buffers: dict[str, Any]
    @property
    def terminal_panel(self) -> Terminal | None:
        """Read only."""
        return self.fields["terminal_panel"]

    def new_file(self,
        *,
        name: str | None = None,
        folder: str | None = None,
        text: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if folder is not None:
            args["folder"] = folder
        if text is not None:
            args["text"] = text
        return self.action("new-file", **args)

    def new_file_many(self, items: list[NewFileArgs]) -> list[dict[str, Any]]:
        """Runs `new-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-file", items)

    def new_folder(self,
        *,
        name: str | None = None,
        folder: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if folder is not None:
            args["folder"] = folder
        return self.action("new-folder", **args)

    def new_folder_many(self, items: list[NewFolderArgs]) -> list[dict[str, Any]]:
        """Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-folder", items)

    def rename(self,
        *,
        entry: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename", entry=entry, name=name)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def move_entry(self,
        *,
        entry: str,
        folder: str | None = None,
        before: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        if folder is not None:
            args["folder"] = folder
        if before is not None:
            args["before"] = before
        return self.action("move-entry", **args)

    def move_entry_many(self, items: list[MoveEntryArgs]) -> list[dict[str, Any]]:
        """Runs `move-entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-entry", items)

    def replace_text(self,
        *,
        file: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("replace-text", file=file, text=text)

    def replace_text_many(self, items: list[ReplaceTextArgs]) -> list[dict[str, Any]]:
        """Runs `replace-text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-text", items)

    def replace_range(self,
        *,
        at: Any,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("replace-range", at=at, text=text)

    def replace_range_many(self, items: list[ReplaceRangeArgs]) -> list[dict[str, Any]]:
        """Runs `replace-range` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-range", items)

    def delete(self, *, entry: str) -> None:
        """Runs the app's action."""
        return self.action("delete", entry=entry)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def record_entry(self,
        *,
        id: str,
        kind: EntryKind,
        name: str,
        language: str,
        folder: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["kind"] = kind
        args["name"] = name
        if folder is not None:
            args["folder"] = folder
        args["language"] = language
        return self.action("record-entry", **args)

    def record_entry_many(self, items: list[RecordEntryArgs]) -> list[dict[str, Any]]:
        """Runs `record-entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-entry", items)

    def record_text(self,
        *,
        file: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("record-text", file=file, text=text)

    def record_text_many(self, items: list[RecordTextArgs]) -> list[dict[str, Any]]:
        """Runs `record-text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-text", items)

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

    def run(self, *, command: str) -> Any:
        """Runs the app's action."""
        return self.action("run", command=command)

    def rerun(self, *, run: str) -> Any:
        """Runs the app's action."""
        return self.action("rerun", run=run)

    def rerun_test(self,
        *,
        run: str,
        test: str,
    ) -> Any:
        """Runs the app's action."""
        return self.action("rerun-test", run=run, test=test)

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

    def show_symbol(self, *, symbol: str) -> None:
        """Runs the app's action."""
        return self.action("show-symbol", symbol=symbol)

    def show_result(self, *, result: str) -> None:
        """Runs the app's action."""
        return self.action("show-result", result=result)

    def reveal(self,
        *,
        line: int,
        column: int,
        results: bool,
        file: str | None = None,
        path: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if file is not None:
            args["file"] = file
        if path is not None:
            args["path"] = path
        args["line"] = line
        args["column"] = column
        args["results"] = results
        return self.action("reveal", **args)

    def find(self) -> None:
        """Runs the app's action."""
        return self.action("find")

    def search_project(self) -> None:
        """Runs the app's action."""
        return self.action("search-project")

    def go_to_line(self,
        *,
        value: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if value is not None:
            args["value"] = value
        return self.action("go-to-line", **args)

    def toggle_bottom_panel(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-bottom-panel")

    def next_theme(self) -> None:
        """Runs the app's action."""
        return self.action("next-theme")

    def go_to_problems(self) -> None:
        """Runs the app's action."""
        return self.action("go-to-problems")

    def go_to_search(self) -> None:
        """Runs the app's action."""
        return self.action("go-to-search")

    def go_to_terminal(self) -> None:
        """Runs the app's action."""
        return self.action("go-to-terminal")

    def toggle_sidebar(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-sidebar")

    def toggle_language_servers(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-language-servers")

    def toggle_format_on_save(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-format-on-save")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_file", "new_file_many", "new_folder", "new_folder_many", "rename", "rename_many", "move_entry", "move_entry_many", "replace_text", "replace_text_many", "replace_range", "replace_range_many", "delete", "delete_many", "record_entry", "record_entry_many", "record_text", "record_text_many", "record_target", "record_target_many", "record_relay_session", "record_relay_session_many", "claim_pane", "claim_pane_many", "run", "rerun", "rerun_test", "stop", "stop_many", "record_command", "record_command_many", "show_symbol", "show_result", "reveal", "find", "search_project", "go_to_line", "toggle_bottom_panel", "next_theme", "go_to_problems", "go_to_search", "go_to_terminal", "toggle_sidebar", "toggle_language_servers", "toggle_format_on_save"})


