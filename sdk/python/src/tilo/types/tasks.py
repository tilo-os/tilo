"""Types for Tasks (`so.tilo.tasks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Tracker` is the app's `tasks.tracker` at `tasks/<resource>`, read with
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
    "Kind",
    "Person",
    "TrackerArea",
    "Area",
    "Attachment",
    "Issue",
    "Link",
    "Message",
    "MessageKind",
    "NewIssue",
    "Origin",
    "Priority",
    "Status",
    "Tracker",
    "Issues",
    "CreateAreaArgs",
    "RenameAreaArgs",
    "SetKindArgs",
    "InvolveArgs",
    "RemovePersonArgs",
    "DeleteAreaArgs",
    "CreateArgs",
    "StartArgs",
    "RenameArgs",
    "MoveUnderArgs",
    "AssignArgs",
    "SetProjectArgs",
    "AttachArgs",
    "DetachArgs",
    "AddLabelArgs",
    "PlaceArgs",
    "CommentArgs",
    "LinkArgs",
    "LinkNoteArgs",
    "DeleteArgs",
    "PlanArgs",
]

Kind = Literal["project", "area"]

class Person(TypedDict):
    name: str
    role: str

class TrackerArea(TypedDict):
    id: str
    name: str
    kind: Kind
    people: list[Entry[Person]]

class Area(TypedDict):
    resource: str
    app: str
    kind: str
    label: str

class Attachment(TypedDict):
    name: str
    media_type: str | None
    bytes: Any
    author: str | None
    author_name: str
    at_ms: int

class Issue(TypedDict):
    number: int
    rank: str
    title: str
    body: str
    status: Status
    assignee: str | None
    assigned_by: str | None
    assigned_ms: int
    labels: list[str]
    parent: str | None
    links: list[Entry[Link]]
    area: Area | None
    due_ms: int | None
    priority: int | None
    estimate: int | None
    attachments: list[Entry[Attachment]]
    comments: list[Entry[Message]]
    author: str | None
    author_name: str
    created_ms: int
    origin: Origin | None
    url: str | None

class Link(TypedDict):
    label: str
    app: str
    kind: str
    resource: str
    record: str | None

class Message(TypedDict):
    author: str | None
    author_name: str
    by_agent: bool
    at_ms: int
    origin: Origin | None
    text: str
    kind: MessageKind
    run: int
    lookup: str
    step: str
    incorporated: bool

MessageKind = Literal["Comment", "ToRun", "FromRun", "Lookup", "Step", "Answer"]

class NewIssue(TypedDict):
    title: str
    parent: str | None
    body: str | None
    status: Status | None
    assignee: str | None
    url: str | None
    project: str | None
    priority: Priority | None
    estimate: int | None
    due_ms: int | None
    labels: list[str] | None
    origin: Origin | None
    rank: str | None
    goal: Area | None

class Origin(TypedDict):
    author: str
    handle: str | None
    source: str
    url: str | None
    reference: str | None
    created_ms: int | None
    updated_ms: int | None

Priority = Literal["none", "urgent", "high", "medium", "low"]

Status = Literal["backlog", "todo", "in-progress", "in-review", "done", "cancelled"]

CreateAreaArgs = TypedDict("CreateAreaArgs", {
    "name": "str",
    "kind": NotRequired["Kind | None"],
})
"""The arguments of `create-area`."""

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

DeleteAreaArgs = TypedDict("DeleteAreaArgs", {
    "area": "str",
})
"""The arguments of `delete-area`."""

CreateArgs = TypedDict("CreateArgs", {
    "title": "str",
    "parent": NotRequired["str | None"],
    "body": NotRequired["str | None"],
    "status": NotRequired["Status | None"],
    "assignee": NotRequired["str | None"],
    "url": NotRequired["str | None"],
    "project": NotRequired["str | None"],
    "priority": NotRequired["Priority | None"],
    "estimate": NotRequired["int | None"],
    "due_ms": NotRequired["int | None"],
    "labels": NotRequired["list[str] | None"],
    "origin": NotRequired["Origin | None"],
    "rank": NotRequired["str | None"],
    "goal": NotRequired["Area | None"],
})
"""The arguments of `create`."""

StartArgs = TypedDict("StartArgs", {
    "before": "int",
})
"""The arguments of `start`."""

RenameArgs = TypedDict("RenameArgs", {
    "issue": "str",
    "title": "str",
})
"""The arguments of `rename`."""

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

SetProjectArgs = TypedDict("SetProjectArgs", {
    "issue": "str",
    "project": NotRequired["str | None"],
    "goal": NotRequired["Area | None"],
})
"""The arguments of `set-project`."""

AttachArgs = TypedDict("AttachArgs", {
    "issue": "str",
    "name": "str",
    "bytes": "Any",
    "media_type": NotRequired["str | None"],
})
"""The arguments of `attach`."""

DetachArgs = TypedDict("DetachArgs", {
    "issue": "str",
    "attachment": "str",
})
"""The arguments of `detach`."""

AddLabelArgs = TypedDict("AddLabelArgs", {
    "issue": "str",
    "label": "str",
})
"""The arguments of `add-label`."""

PlaceArgs = TypedDict("PlaceArgs", {
    "issue": "str",
    "status": "Status",
    "before": NotRequired["str | None"],
    "rank": NotRequired["str | None"],
})
"""The arguments of `place`."""

CommentArgs = TypedDict("CommentArgs", {
    "issue": "str",
    "text": "str",
    "origin": NotRequired["Origin | None"],
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
    "priority": NotRequired["Priority | None"],
    "estimate": NotRequired["int | None"],
    "due_ms": NotRequired["int | None"],
})
"""The arguments of `plan`."""

class Tracker(Resource):
    """Tasks — `tasks.tracker`."""
    __surface__ = "so.tilo.tasks@188395cb119c73de"
    areas: list[Entry[TrackerArea]]

    def create_area(self,
        *,
        name: str,
        kind: Kind | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if kind is not None:
            args["kind"] = kind
        return self.action("create-area", **args)

    def create_area_many(self, items: list[CreateAreaArgs]) -> list[dict[str, Any]]:
        """Runs `create-area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-area", items)

    def rename_area(self,
        *,
        area: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-area", area=area, name=name)

    def rename_area_many(self, items: list[RenameAreaArgs]) -> list[dict[str, Any]]:
        """Runs `rename-area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-area", items)

    def set_kind(self,
        *,
        area: str,
        kind: Kind,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-kind", area=area, kind=kind)

    def set_kind_many(self, items: list[SetKindArgs]) -> list[dict[str, Any]]:
        """Runs `set-kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-kind", items)

    def involve(self,
        *,
        area: str,
        name: str,
        role: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["area"] = area
        args["name"] = name
        if role is not None:
            args["role"] = role
        return self.action("involve", **args)

    def involve_many(self, items: list[InvolveArgs]) -> list[dict[str, Any]]:
        """Runs `involve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("involve", items)

    def remove_person(self,
        *,
        area: str,
        person: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-person", area=area, person=person)

    def remove_person_many(self, items: list[RemovePersonArgs]) -> list[dict[str, Any]]:
        """Runs `remove-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-person", items)

    def delete_area(self, *, area: str) -> None:
        """Runs the app's action."""
        return self.action("delete-area", area=area)

    def delete_area_many(self, items: list[DeleteAreaArgs]) -> list[dict[str, Any]]:
        """Runs `delete-area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-area", items)

    def new_project(self) -> None:
        """Runs the app's action."""
        return self.action("new-project")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"create_area", "create_area_many", "rename_area", "rename_area_many", "set_kind", "set_kind_many", "involve", "involve_many", "remove_person", "remove_person_many", "delete_area", "delete_area_many", "new_project"})


class Issues(Resource):
    """Issues — `tasks.issues`."""
    __surface__ = "so.tilo.tasks@188395cb119c73de"
    issues: list[Entry[Issue]]
    @property
    def before(self) -> int:
        """Read only."""
        return self.fields["before"]
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
        project: str | None = None,
        priority: Priority | None = None,
        estimate: int | None = None,
        due_ms: int | None = None,
        labels: list[str] | None = None,
        origin: Origin | None = None,
        rank: str | None = None,
        goal: Area | None = None,
    ) -> None:
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
        if project is not None:
            args["project"] = project
        if priority is not None:
            args["priority"] = priority
        if estimate is not None:
            args["estimate"] = estimate
        if due_ms is not None:
            args["due_ms"] = due_ms
        if labels is not None:
            args["labels"] = labels
        if origin is not None:
            args["origin"] = origin
        if rank is not None:
            args["rank"] = rank
        if goal is not None:
            args["goal"] = goal
        return self.action("create", **args)

    def create_many(self, items: list[CreateArgs]) -> list[dict[str, Any]]:
        """Runs `create` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create", items)

    def start(self, *, before: int) -> None:
        """Runs the app's action."""
        return self.action("start", before=before)

    def start_many(self, items: list[StartArgs]) -> list[dict[str, Any]]:
        """Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start", items)

    def rename(self,
        *,
        issue: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename", issue=issue, title=title)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def move_under(self,
        *,
        issue: str,
        parent: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if parent is not None:
            args["parent"] = parent
        return self.action("move-under", **args)

    def move_under_many(self, items: list[MoveUnderArgs]) -> list[dict[str, Any]]:
        """Runs `move-under` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-under", items)

    def assign(self,
        *,
        issue: str,
        assignee: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if assignee is not None:
            args["assignee"] = assignee
        return self.action("assign", **args)

    def assign_many(self, items: list[AssignArgs]) -> list[dict[str, Any]]:
        """Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("assign", items)

    def set_project(self,
        *,
        issue: str,
        project: str | None = None,
        goal: Area | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if project is not None:
            args["project"] = project
        if goal is not None:
            args["goal"] = goal
        return self.action("set-project", **args)

    def set_project_many(self, items: list[SetProjectArgs]) -> list[dict[str, Any]]:
        """Runs `set-project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-project", items)

    def attach(self,
        *,
        issue: str,
        name: str,
        bytes: Any,
        media_type: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        args["name"] = name
        args["bytes"] = bytes
        if media_type is not None:
            args["media_type"] = media_type
        return self.action("attach", **args)

    def attach_many(self, items: list[AttachArgs]) -> list[dict[str, Any]]:
        """Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("attach", items)

    def detach(self,
        *,
        issue: str,
        attachment: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("detach", issue=issue, attachment=attachment)

    def detach_many(self, items: list[DetachArgs]) -> list[dict[str, Any]]:
        """Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("detach", items)

    def add_label(self,
        *,
        issue: str,
        label: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-label", issue=issue, label=label)

    def add_label_many(self, items: list[AddLabelArgs]) -> list[dict[str, Any]]:
        """Runs `add-label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-label", items)

    def place(self,
        *,
        issue: str,
        status: Status,
        before: str | None = None,
        rank: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        args["status"] = status
        if before is not None:
            args["before"] = before
        if rank is not None:
            args["rank"] = rank
        return self.action("place", **args)

    def place_many(self, items: list[PlaceArgs]) -> list[dict[str, Any]]:
        """Runs `place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place", items)

    def comment(self,
        *,
        issue: str,
        text: str,
        origin: Origin | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        args["text"] = text
        if origin is not None:
            args["origin"] = origin
        return self.action("comment", **args)

    def comment_many(self, items: list[CommentArgs]) -> list[dict[str, Any]]:
        """Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("comment", items)

    def link(self,
        *,
        issue: str,
        app: str,
        kind: str,
        resource: str,
        label: str,
        record: str | None = None,
    ) -> None:
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
        """Runs `link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("link", items)

    def link_note(self,
        *,
        issue: str,
        note: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("link-note", issue=issue, note=note)

    def link_note_many(self, items: list[LinkNoteArgs]) -> list[dict[str, Any]]:
        """Runs `link-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("link-note", items)

    def delete(self, *, issue: str) -> None:
        """Runs the app's action."""
        return self.action("delete", issue=issue)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def plan(self,
        *,
        issue: str,
        area: Area | None = None,
        priority: Priority | None = None,
        estimate: int | None = None,
        due_ms: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["issue"] = issue
        if area is not None:
            args["area"] = area
        if priority is not None:
            args["priority"] = priority
        if estimate is not None:
            args["estimate"] = estimate
        if due_ms is not None:
            args["due_ms"] = due_ms
        return self.action("plan", **args)

    def plan_many(self, items: list[PlanArgs]) -> list[dict[str, Any]]:
        """Runs `plan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("plan", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"create", "create_many", "start", "start_many", "rename", "rename_many", "move_under", "move_under_many", "assign", "assign_many", "set_project", "set_project_many", "attach", "attach_many", "detach", "detach_many", "add_label", "add_label_many", "place", "place_many", "comment", "comment_many", "link", "link_many", "link_note", "link_note_many", "delete", "delete_many", "plan", "plan_many"})


