"""Types for Code Editor (`so.tilo.code-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Project` is the app's `code_editor.project` at `code-editor/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "Entry",
    "EntryKind",
    "ItemKind",
    "Run",
    "RunItem",
    "RunRequest",
    "Terminal",
    "Project",
]

class Entry(TypedDict):
    kind: EntryKind
    name: str
    parent: str | None
    language: str

EntryKind = Literal["file", "folder"]

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

class Terminal(TypedDict):
    target: str | None
    relay_session: str | None
    pane: str | None
    device: str | None
    holder: str | None
    runs: list[Run]
    requests: Any

class Project(Resource):
    """Code Editor — `code_editor.project`."""
    __surface__ = "so.tilo.code-editor@a337d60d96a31d9c"
    files: list[Entry]
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
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if folder is not None:
            args["folder"] = folder
        if text is not None:
            args["text"] = text
        return self.action("new-file", **args)

    def new_folder(self,
        *,
        name: str | None = None,
        folder: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if folder is not None:
            args["folder"] = folder
        return self.action("new-folder", **args)

    def rename(self,
        *,
        entry: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename", entry=entry, name=name)

    def move_entry(self,
        *,
        entry: str,
        folder: str | None = None,
        before: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        if folder is not None:
            args["folder"] = folder
        if before is not None:
            args["before"] = before
        return self.action("move-entry", **args)

    def replace_text(self,
        *,
        file: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("replace-text", file=file, text=text)

    def replace_range(self,
        *,
        at: Any,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("replace-range", at=at, text=text)

    def delete(self, *, entry: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", entry=entry)

    def record_entry(self,
        *,
        id: str,
        kind: EntryKind,
        name: str,
        language: str,
        folder: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["kind"] = kind
        args["name"] = name
        if folder is not None:
            args["folder"] = folder
        args["language"] = language
        return self.action("record-entry", **args)

    def record_text(self,
        *,
        file: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record-text", file=file, text=text)

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

    def record_relay_session(self,
        *,
        session: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if session is not None:
            args["session"] = session
        return self.action("record-relay-session", **args)

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

    def run(self, *, command: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("run", command=command)

    def rerun(self, *, run: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("rerun", run=run)

    def rerun_test(self,
        *,
        run: str,
        test: str,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("rerun-test", run=run, test=test)

    def stop(self) -> Run | None:
        """Runs the app's action."""
        return self.action("stop")

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


