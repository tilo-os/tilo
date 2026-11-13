"""Types for Tasks (`so.tilo.tasks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Tracker` is the app's `tasks.tracker` at `tasks/<resource>`, read with
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
    "Area",
    "Imported",
    "ImportedMessage",
    "Issue",
    "Kind",
    "Link",
    "Message",
    "MessageKind",
    "Person",
    "Status",
    "TrackerArea",
    "Tracker",
]

class Area(TypedDict):
    resource: str
    app: str
    kind: str
    label: str

class Imported(TypedDict):
    task: str
    title: str
    instruction: str
    status: Status
    assignee: str | None
    author: str
    created_ms: int
    messages: list[ImportedMessage]

class ImportedMessage(TypedDict):
    author: str
    by_agent: bool
    at_ms: int
    text: str

class Issue(TypedDict):
    number: int
    title: str
    body: str
    status: Status
    assignee: str | None
    labels: list[str]
    parent: str | None
    links: list[Link]
    area: Area | None
    due_ms: int | None
    priority: int | None
    comments: list[Message]
    author: str
    created_ms: int

Kind = Literal["project", "area"]

class Link(TypedDict):
    label: str
    app: str
    kind: str
    resource: str
    record: str | None

class Message(TypedDict):
    author: str
    by_agent: bool
    at_ms: int
    text: str
    kind: MessageKind
    run: int
    lookup: str
    step: str
    incorporated: bool

MessageKind = Literal["comment", "to_run", "from_run", "lookup", "step", "answer"]

class Person(TypedDict):
    name: str
    role: str

Status = Literal["backlog", "todo", "in_progress", "in_review", "done", "cancelled"]

class TrackerArea(TypedDict):
    id: str
    name: str
    kind: Kind
    people: list[Person]

class Tracker(Resource):
    """Tasks — `tasks.tracker`."""
    __surface__ = "so.tilo.tasks@1af478b177cd7ffc"
    issues: list[Issue]
    areas: list[TrackerArea]
    @property
    def numbers(self) -> int:
        """Read only."""
        return self.fields["numbers"]
    @property
    def imported(self) -> list[str]:
        """Read only."""
        return self.fields["imported"]

    def create(self,
        *,
        title: str,
        parent: str | None = None,
        body: str | None = None,
        status: Status | None = None,
        assignee: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if parent is not None:
            args["parent"] = parent
        if body is not None:
            args["body"] = body
        if status is not None:
            args["status"] = status
        if assignee is not None:
            args["assignee"] = assignee
        return self.action("create", **args)

    def create_area(self,
        *,
        name: str,
        kind: Kind | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if kind is not None:
            args["kind"] = kind
        return self.action("create-area", **args)

    def rename(self,
        *,
        issue: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename", issue=issue, title=title)

    def rename_area(self,
        *,
        area: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-area", area=area, name=name)

    def set_kind(self,
        *,
        area: str,
        kind: Kind,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-kind", area=area, kind=kind)

    def involve(self,
        *,
        area: str,
        name: str,
        role: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["area"] = area
        args["name"] = name
        if role is not None:
            args["role"] = role
        return self.action("involve", **args)

    def remove_person(self,
        *,
        area: str,
        person: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-person", area=area, person=person)

    def move_under(self,
        *,
        issue: str,
        parent: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if parent is not None:
            args["parent"] = parent
        return self.action("move-under", **args)

    def assign(self,
        *,
        issue: str,
        assignee: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if assignee is not None:
            args["assignee"] = assignee
        return self.action("assign", **args)

    def add_label(self,
        *,
        issue: str,
        label: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-label", issue=issue, label=label)

    def place(self,
        *,
        issue: str,
        status: Status,
        before: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        args["status"] = status
        if before is not None:
            args["before"] = before
        return self.action("place", **args)

    def comment(self,
        *,
        issue: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("comment", issue=issue, text=text)

    def link(self,
        *,
        issue: str,
        app: str,
        kind: str,
        resource: str,
        label: str,
        record: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        args["app"] = app
        args["kind"] = kind
        args["resource"] = resource
        args["label"] = label
        if record is not None:
            args["record"] = record
        return self.action("link", **args)

    def link_note(self,
        *,
        issue: str,
        note: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("link-note", issue=issue, note=note)

    def delete(self, *, issue: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", issue=issue)

    def plan(self,
        *,
        issue: str,
        area: Area | None = None,
        priority: int | None = None,
        due_ms: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if area is not None:
            args["area"] = area
        if priority is not None:
            args["priority"] = priority
        if due_ms is not None:
            args["due_ms"] = due_ms
        return self.action("plan", **args)

    def import_(self, *, tasks: list[Imported]) -> Run | None:
        """Runs the app's action."""
        return self.action("import", tasks=tasks)

    def new_project(self) -> None:
        """Runs the app's action."""
        return self.action("new-project")


