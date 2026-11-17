"""Types for Tasks (`so.tilo.tasks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Tracker` is the app's `tasks.tracker` at `tasks/<resource>`, read with
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
    "Area",
    "Issue",
    "Kind",
    "Link",
    "Message",
    "MessageKind",
    "Person",
    "Status",
    "TrackerArea",
    "Tracker",
    "CreateArgs",
    "CreateAreaArgs",
    "RenameArgs",
    "RenameAreaArgs",
    "SetKindArgs",
    "InvolveArgs",
    "RemovePersonArgs",
    "MoveUnderArgs",
    "AssignArgs",
    "AddLabelArgs",
    "PlaceArgs",
    "CommentArgs",
    "LinkArgs",
    "LinkNoteArgs",
    "DeleteArgs",
    "PlanArgs",
]

class Area(TypedDict):
    resource: str
    app: str
    kind: str
    label: str

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
    author: str | None
    created_ms: int
    url: str | None

Kind = Literal["project", "area"]

class Link(TypedDict):
    label: str
    app: str
    kind: str
    resource: str
    record: str | None

class Message(TypedDict):
    author: str | None
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

CreateArgs = TypedDict("CreateArgs", {
    "title": "str",
    "parent": NotRequired["str | None"],
    "body": NotRequired["str | None"],
    "status": NotRequired["Status | None"],
    "assignee": NotRequired["str | None"],
    "url": NotRequired["str | None"],
})
"""The arguments of `create`."""

CreateAreaArgs = TypedDict("CreateAreaArgs", {
    "name": "str",
    "kind": NotRequired["Kind | None"],
})
"""The arguments of `create-area`."""

RenameArgs = TypedDict("RenameArgs", {
    "issue": "str",
    "title": "str",
})
"""The arguments of `rename`."""

RenameAreaArgs = TypedDict("RenameAreaArgs", {
    "area": "str",
    "name": "str",
})
"""The arguments of `rename-area`."""

SetKindArgs = TypedDict("SetKindArgs", {
    "area": "str",
    "kind": "Kind",
})
"""The arguments of `set-kind`."""

InvolveArgs = TypedDict("InvolveArgs", {
    "area": "str",
    "name": "str",
    "role": NotRequired["str | None"],
})
"""The arguments of `involve`."""

RemovePersonArgs = TypedDict("RemovePersonArgs", {
    "area": "str",
    "person": "str",
})
"""The arguments of `remove-person`."""

MoveUnderArgs = TypedDict("MoveUnderArgs", {
    "issue": "str",
    "parent": NotRequired["str | None"],
})
"""The arguments of `move-under`."""

AssignArgs = TypedDict("AssignArgs", {
    "issue": "str",
    "assignee": NotRequired["str | None"],
})
"""The arguments of `assign`."""

AddLabelArgs = TypedDict("AddLabelArgs", {
    "issue": "str",
    "label": "str",
})
"""The arguments of `add-label`."""

PlaceArgs = TypedDict("PlaceArgs", {
    "issue": "str",
    "status": "Status",
    "before": NotRequired["str | None"],
})
"""The arguments of `place`."""

CommentArgs = TypedDict("CommentArgs", {
    "issue": "str",
    "text": "str",
})
"""The arguments of `comment`."""

LinkArgs = TypedDict("LinkArgs", {
    "issue": "str",
    "app": "str",
    "kind": "str",
    "resource": "str",
    "label": "str",
    "record": NotRequired["str | None"],
})
"""The arguments of `link`."""

LinkNoteArgs = TypedDict("LinkNoteArgs", {
    "issue": "str",
    "note": "Any",
})
"""The arguments of `link-note`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "issue": "str",
})
"""The arguments of `delete`."""

PlanArgs = TypedDict("PlanArgs", {
    "issue": "str",
    "area": NotRequired["Area | None"],
    "priority": NotRequired["int | None"],
    "due_ms": NotRequired["int | None"],
})
"""The arguments of `plan`."""

class Tracker(Resource):
    """Tasks — `tasks.tracker`."""
    __surface__ = "so.tilo.tasks@7e65343905e59068"
    issues: list[Issue]
    areas: list[TrackerArea]
    @property
    def numbers(self) -> int:
        """Read only."""
        return self.fields["numbers"]

    def create(self,
        *,
        title: str,
        parent: str | None = None,
        body: str | None = None,
        status: Status | None = None,
        assignee: str | None = None,
        url: str | None = None,
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
        if url is not None:
            args["url"] = url
        return self.action("create", **args)

    def create_many(self, items: list[CreateArgs]) -> list[dict[str, Any]]:
        """Runs `create` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("create", items)

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

    def create_area_many(self, items: list[CreateAreaArgs]) -> list[dict[str, Any]]:
        """Runs `create-area` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("create-area", items)

    def rename(self,
        *,
        issue: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename", issue=issue, title=title)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("rename", items)

    def rename_area(self,
        *,
        area: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-area", area=area, name=name)

    def rename_area_many(self, items: list[RenameAreaArgs]) -> list[dict[str, Any]]:
        """Runs `rename-area` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("rename-area", items)

    def set_kind(self,
        *,
        area: str,
        kind: Kind,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-kind", area=area, kind=kind)

    def set_kind_many(self, items: list[SetKindArgs]) -> list[dict[str, Any]]:
        """Runs `set-kind` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("set-kind", items)

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

    def involve_many(self, items: list[InvolveArgs]) -> list[dict[str, Any]]:
        """Runs `involve` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("involve", items)

    def remove_person(self,
        *,
        area: str,
        person: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-person", area=area, person=person)

    def remove_person_many(self, items: list[RemovePersonArgs]) -> list[dict[str, Any]]:
        """Runs `remove-person` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remove-person", items)

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

    def move_under_many(self, items: list[MoveUnderArgs]) -> list[dict[str, Any]]:
        """Runs `move-under` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("move-under", items)

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

    def assign_many(self, items: list[AssignArgs]) -> list[dict[str, Any]]:
        """Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("assign", items)

    def add_label(self,
        *,
        issue: str,
        label: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-label", issue=issue, label=label)

    def add_label_many(self, items: list[AddLabelArgs]) -> list[dict[str, Any]]:
        """Runs `add-label` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("add-label", items)

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

    def place_many(self, items: list[PlaceArgs]) -> list[dict[str, Any]]:
        """Runs `place` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("place", items)

    def comment(self,
        *,
        issue: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("comment", issue=issue, text=text)

    def comment_many(self, items: list[CommentArgs]) -> list[dict[str, Any]]:
        """Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("comment", items)

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

    def link_many(self, items: list[LinkArgs]) -> list[dict[str, Any]]:
        """Runs `link` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("link", items)

    def link_note(self,
        *,
        issue: str,
        note: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("link-note", issue=issue, note=note)

    def link_note_many(self, items: list[LinkNoteArgs]) -> list[dict[str, Any]]:
        """Runs `link-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("link-note", items)

    def delete(self, *, issue: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", issue=issue)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete", items)

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

    def plan_many(self, items: list[PlanArgs]) -> list[dict[str, Any]]:
        """Runs `plan` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("plan", items)

    def new_project(self) -> None:
        """Runs the app's action."""
        return self.action("new-project")


