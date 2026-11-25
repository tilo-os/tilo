"""Types for Tasks (`so.tilo.tasks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `tasks/<resource>`, read with
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
    "Status",
    "WaitKind",
    "StoppedWhy",
    "BudgetBy",
    "Priority",
    "Notice",
    "Person",
    "Project",
    "Wait",
    "Stopped",
    "Resumed",
    "Work",
    "Origin",
    "Task",
    "Related",
    "Tasks",
    "TiloTasks",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "SetKindArgs",
    "RenameArgs",
    "InvolveArgs",
    "RemovePersonArgs",
    "RemoveProjectArgs",
    "TaskEditBlockArgs",
    "TaskInsertBlockArgs",
    "TaskDeleteBlockArgs",
    "RetitleArgs",
    "MoveToArgs",
    "IndentArgs",
    "OutdentArgs",
    "SetStatusArgs",
    "CheckArgs",
    "RemoveEmptyStepArgs",
    "DueTodayArgs",
    "DueTomorrowArgs",
    "DueNextWeekArgs",
    "ScheduleArgs",
    "SetTimeArgs",
    "DueClearArgs",
    "RemindAtArgs",
    "RemindClearArgs",
    "RemindArgs",
    "SetPriorityNoneArgs",
    "SetPriorityLowArgs",
    "SetPriorityMediumArgs",
    "SetPriorityHighArgs",
    "SetPriorityUrgentArgs",
    "EstimateArgs",
    "FileArgs",
    "UnfileArgs",
    "AssignArgs",
    "RelateArgs",
    "UnrelateArgs",
    "LabelArgs",
    "UnlabelArgs",
    "AttachArgs",
    "DetachArgs",
    "WaitArgs",
    "ResumeArgs",
    "SetPlaceArgs",
    "SetBudgetArgs",
    "SetPiecesAtOnceArgs",
    "SessionBeganArgs",
    "SessionEndedArgs",
    "StopArgs",
    "ShareWithArgs",
    "SetStatusManyArgs",
    "AssignManyArgs",
    "LabelManyArgs",
]

Kind = Literal["project", "area"]
"""Which kind of project this is: a finite goal whose work ends with it, or an ongoing responsibility that has no end, only what is done next."""

Status = Literal["backlog", "todo", "in_progress", "in_review", "done", "cancelled"]
"""Where a task stands in its work: a board's columns, fixed so that every task of the workspace reads in the same place in the same column. Backlog is written down and not planned yet; todo is the next thing to pick up; in progress is being worked on; in review is done by whoever worked on it and waits for a person to look; done is checked by its person; cancelled is not to be done."""

WaitKind = Literal["time", "reply", "change", "answer", "people", "pieces"]
"""What a wait waits for: the kinds a session of an agent tells apart. A moment ends at its time; a reply in a mail thread or a conversation; a change in a resource of the workspace; an answer of the person the work is for; answers of several people; and its pieces, the tasks under it, which let it go on when none of them is still to be worked on."""

StoppedWhy = Literal["failed", "budget", "time", "lost_worker", "person"]
"""Why a task's work stopped: it could not go on, its budget in money or in working time is spent, its agent sessions were interrupted and did not come back, or a person stopped it."""

BudgetBy = Literal["person", "agent"]
"""Whose budget a task's work spends: its person's, or the agent working on it."""

Priority = Literal["none", "low", "medium", "high", "urgent"]
"""How much a task matters, most urgent last."""

EditBlockArgs = TypedDict("EditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

InsertBlockArgs = TypedDict("InsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

DeleteBlockArgs = TypedDict("DeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

SetKindArgs = TypedDict("SetKindArgs", {
    "kind": "Kind",
})
"""The arguments of `set_kind`."""

RenameArgs = TypedDict("RenameArgs", {
    "name": "str",
})
"""The arguments of `rename`."""

InvolveArgs = TypedDict("InvolveArgs", {
    "name": "str",
    "role": NotRequired["str | None"],
})
"""The arguments of `involve`."""

RemovePersonArgs = TypedDict("RemovePersonArgs", {
    "person": "Person",
})
"""The arguments of `remove_person`."""

RemoveProjectArgs = TypedDict("RemoveProjectArgs", {
})
"""The arguments of `remove_project`."""

TaskEditBlockArgs = TypedDict("TaskEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

TaskInsertBlockArgs = TypedDict("TaskInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

TaskDeleteBlockArgs = TypedDict("TaskDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

RetitleArgs = TypedDict("RetitleArgs", {
    "title": "str",
})
"""The arguments of `retitle`."""

MoveToArgs = TypedDict("MoveToArgs", {
    "parent": NotRequired["Task | None"],
    "before": NotRequired["Task | None"],
})
"""The arguments of `move_to`."""

IndentArgs = TypedDict("IndentArgs", {
    "above": NotRequired["Task | None"],
})
"""The arguments of `indent`."""

OutdentArgs = TypedDict("OutdentArgs", {
})
"""The arguments of `outdent`."""

SetStatusArgs = TypedDict("SetStatusArgs", {
    "status": "Status",
    "at": NotRequired["int | None"],
})
"""The arguments of `set_status`."""

CheckArgs = TypedDict("CheckArgs", {
    "done": NotRequired["bool | None"],
})
"""The arguments of `check`."""

RemoveEmptyStepArgs = TypedDict("RemoveEmptyStepArgs", {
})
"""The arguments of `remove_empty_step`."""

DueTodayArgs = TypedDict("DueTodayArgs", {
})
"""The arguments of `due_today`."""

DueTomorrowArgs = TypedDict("DueTomorrowArgs", {
})
"""The arguments of `due_tomorrow`."""

DueNextWeekArgs = TypedDict("DueNextWeekArgs", {
})
"""The arguments of `due_next_week`."""

ScheduleArgs = TypedDict("ScheduleArgs", {
    "due": NotRequired["str | None"],
})
"""The arguments of `schedule`."""

SetTimeArgs = TypedDict("SetTimeArgs", {
    "at": NotRequired["str | None"],
})
"""The arguments of `set_time`."""

DueClearArgs = TypedDict("DueClearArgs", {
})
"""The arguments of `due_clear`."""

RemindAtArgs = TypedDict("RemindAtArgs", {
    "when": "str",
})
"""The arguments of `remind_at`."""

RemindClearArgs = TypedDict("RemindClearArgs", {
})
"""The arguments of `remind_clear`."""

RemindArgs = TypedDict("RemindArgs", {
})
"""The arguments of `remind`."""

SetPriorityNoneArgs = TypedDict("SetPriorityNoneArgs", {
})
"""The arguments of `set_priority_none`."""

SetPriorityLowArgs = TypedDict("SetPriorityLowArgs", {
})
"""The arguments of `set_priority_low`."""

SetPriorityMediumArgs = TypedDict("SetPriorityMediumArgs", {
})
"""The arguments of `set_priority_medium`."""

SetPriorityHighArgs = TypedDict("SetPriorityHighArgs", {
})
"""The arguments of `set_priority_high`."""

SetPriorityUrgentArgs = TypedDict("SetPriorityUrgentArgs", {
})
"""The arguments of `set_priority_urgent`."""

EstimateArgs = TypedDict("EstimateArgs", {
    "points": NotRequired["int | None"],
})
"""The arguments of `estimate`."""

FileArgs = TypedDict("FileArgs", {
    "p": "Project",
})
"""The arguments of `file`."""

UnfileArgs = TypedDict("UnfileArgs", {
})
"""The arguments of `unfile`."""

AssignArgs = TypedDict("AssignArgs", {
    "who": NotRequired["str | None"],
    "contact": NotRequired["Any | None"],
})
"""The arguments of `assign`."""

RelateArgs = TypedDict("RelateArgs", {
    "other": "Task",
    "how": "str",
})
"""The arguments of `relate`."""

UnrelateArgs = TypedDict("UnrelateArgs", {
    "relation": "Related",
})
"""The arguments of `unrelate`."""

LabelArgs = TypedDict("LabelArgs", {
    "name": "str",
})
"""The arguments of `label`."""

UnlabelArgs = TypedDict("UnlabelArgs", {
    "name": "str",
})
"""The arguments of `unlabel`."""

AttachArgs = TypedDict("AttachArgs", {
    "file": "Any",
})
"""The arguments of `attach`."""

DetachArgs = TypedDict("DetachArgs", {
    "attachment": "Any",
})
"""The arguments of `detach`."""

WaitArgs = TypedDict("WaitArgs", {
    "what": "str",
    "until": NotRequired["str | None"],
    "kind": NotRequired["WaitKind | None"],
    "on": NotRequired["str | None"],
})
"""The arguments of `wait`."""

ResumeArgs = TypedDict("ResumeArgs", {
    "cause": NotRequired["str | None"],
})
"""The arguments of `resume`."""

SetPlaceArgs = TypedDict("SetPlaceArgs", {
    "folder": NotRequired["str | None"],
    "note": NotRequired["str | None"],
})
"""The arguments of `set_place`."""

SetBudgetArgs = TypedDict("SetBudgetArgs", {
    "micros": NotRequired["int | None"],
    "millis": NotRequired["int | None"],
    "by": NotRequired["BudgetBy | None"],
})
"""The arguments of `set_budget`."""

SetPiecesAtOnceArgs = TypedDict("SetPiecesAtOnceArgs", {
    "pieces": NotRequired["int | None"],
})
"""The arguments of `set_pieces_at_once`."""

SessionBeganArgs = TypedDict("SessionBeganArgs", {
})
"""The arguments of `session_began`."""

SessionEndedArgs = TypedDict("SessionEndedArgs", {
    "spent_micros": NotRequired["int | None"],
    "worked_ms": NotRequired["int | None"],
})
"""The arguments of `session_ended`."""

StopArgs = TypedDict("StopArgs", {
    "why": "StoppedWhy",
    "words": NotRequired["str | None"],
})
"""The arguments of `stop`."""

ShareWithArgs = TypedDict("ShareWithArgs", {
    "from": "Task",
})
"""The arguments of `share_with`."""

SetStatusManyArgs = TypedDict("SetStatusManyArgs", {
    "chosen": "list[Task]",
    "status": "Status",
    "at": NotRequired["int | None"],
})
"""The arguments of `set_status_many`."""

AssignManyArgs = TypedDict("AssignManyArgs", {
    "chosen": "list[Task]",
    "who": NotRequired["str | None"],
})
"""The arguments of `assign_many`."""

LabelManyArgs = TypedDict("LabelManyArgs", {
    "chosen": "list[Task]",
    "name": "str",
})
"""The arguments of `label_many`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
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


class Person(Resource):
    """Person — `tl.so.tilo.tasks.Person`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    ktw2jimm: str  # name
    hgptkohm: str  # role


class Project(Resource):
    """Project — `tl.so.tilo.tasks.Project`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    sgnau7vd: str  # name
    rsoasw3n: Kind  # kind
    c5pgzhqt: str  # notes
    mboq6kfe: list[Entry[Person]]  # people

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[EditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[InsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[DeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def set_kind(self, *, kind: Kind) -> None:
        """Sets whether a project is one with an end, or an ongoing responsibility."""
        return self.action("set_kind", kind=kind)

    def set_kind_many(self, items: list[SetKindArgs]) -> list[dict[str, Any]]:
        """Runs `set_kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_kind", items)

    def rename(self, *, name: str) -> None:
        """Renames a project."""
        return self.action("rename", name=name)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def involve(self,
        *,
        name: str,
        role: str | None = None,
    ) -> None:
        """Adds someone involved in a project, with the role they have there."""
        args: dict[str, Any] = {}
        args["name"] = name
        if role is not None:
            args["role"] = role
        return self.action("involve", **args)

    def involve_many(self, items: list[InvolveArgs]) -> list[dict[str, Any]]:
        """Runs `involve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("involve", items)

    def remove_person(self, *, person: Person) -> None:
        """Takes someone off a project."""
        return self.action("remove_person", person=person)

    def remove_person_many(self, items: list[RemovePersonArgs]) -> list[dict[str, Any]]:
        """Runs `remove_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_person", items)

    def remove_project(self) -> None:
        """Deletes a project; its tasks go back to the inbox."""
        return self.action("remove_project")

    def remove_project_many(self, items: list[RemoveProjectArgs]) -> list[dict[str, Any]]:
        """Runs `remove_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_project", items)

    def add_to(self,
        *,
        title: str,
        status: Status | None = None,
    ) -> Task:
        """Adds a task to a project at `status`. It is the project's primary action, so its key brings the person to the entry that adds there."""
        args: dict[str, Any] = {}
        args["title"] = title
        if status is not None:
            args["status"] = status
        return self.action("add_to", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "set_kind", "set_kind_many", "rename", "rename_many", "involve", "involve_many", "remove_person", "remove_person_many", "remove_project", "remove_project_many", "add_to"})


class Wait(Resource):
    """Wait — `tl.so.tilo.tasks.Wait`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    @property
    def _4fz6j3rg(self) -> WaitKind:
        """kind (`4fz6j3rg` on the wire)."""
        return self.fields["4fz6j3rg"]

    @_4fz6j3rg.setter
    def _4fz6j3rg(self, value: WaitKind) -> None:
        self.edit("4fz6j3rg", value=value)
    @property
    def _4ijm32x7(self) -> str:
        """what (`4ijm32x7` on the wire)."""
        return self.fields["4ijm32x7"]

    @_4ijm32x7.setter
    def _4ijm32x7(self, value: str) -> None:
        self.edit("4ijm32x7", value=value)
    dy6nui5s: str | None  # until
    osxm3ega: str  # event


class Stopped(Resource):
    """Stopped — `tl.so.tilo.tasks.Stopped`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    ps5lbw2w: StoppedWhy  # why
    @property
    def _5yqkyqzr(self) -> str:
        """words (`5yqkyqzr` on the wire)."""
        return self.fields["5yqkyqzr"]

    @_5yqkyqzr.setter
    def _5yqkyqzr(self, value: str) -> None:
        self.edit("5yqkyqzr", value=value)
    kdugvhf2: str  # at


class Resumed(Resource):
    """Resumed — `tl.so.tilo.tasks.Resumed`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    @property
    def _66a5tqcs(self) -> str:
        """cause (`66a5tqcs` on the wire)."""
        return self.fields["66a5tqcs"]

    @_66a5tqcs.setter
    def _66a5tqcs(self, value: str) -> None:
        self.edit("66a5tqcs", value=value)
    ys7neba6: str  # at


class Work(Resource):
    """Work — `tl.so.tilo.tasks.Work`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    finhtl5k: str  # folder
    ckijp22f: str  # note
    @property
    def _3hituttb(self) -> list[Wait]:
        """waits (`3hituttb` on the wire)."""
        return self.fields["3hituttb"]

    @_3hituttb.setter
    def _3hituttb(self, value: list[Wait]) -> None:
        self.edit("3hituttb", value=value)
    fxyk36kk: str | None  # until
    pcmbdaod: str | None  # since
    lkkui5te: int | None  # budget_micros
    o5tuxt2b: int | None  # budget_ms
    byxiukp2: BudgetBy  # budget_by
    @property
    def _4c4aw4xd(self) -> int:
        """spent_micros (`4c4aw4xd` on the wire)."""
        return self.fields["4c4aw4xd"]

    @_4c4aw4xd.setter
    def _4c4aw4xd(self, value: int) -> None:
        self.edit("4c4aw4xd", value=value)
    virxpj23: int  # worked_ms
    hztvc5dj: int  # sessions
    nuikd6fz: str | None  # session_since
    qublsm3d: int  # lost
    nax7z7kw: Stopped | None  # stopped
    @property
    def _55waklcc(self) -> Resumed | None:
        """resumed (`55waklcc` on the wire)."""
        return self.fields["55waklcc"]

    @_55waklcc.setter
    def _55waklcc(self, value: Resumed | None) -> None:
        self.edit("55waklcc", value=value)
    huwemkpv: int | None  # pieces_at_once


class Origin(Resource):
    """Origin — `tl.so.tilo.tasks.Origin`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    @property
    def _3saq3nij(self) -> str:
        """author (`3saq3nij` on the wire)."""
        return self.fields["3saq3nij"]

    @_3saq3nij.setter
    def _3saq3nij(self, value: str) -> None:
        self.edit("3saq3nij", value=value)
    duzjakqx: str | None  # handle
    g6jrgwt6: str  # source
    gncjc3dz: str | None  # url
    qg5qvqsi: str | None  # reference
    xuslp5pg: str | None  # created
    no47vg4q: str | None  # updated

    def import_issue(self,
        *,
        title: str,
        key: str,
        status: Status | None = None,
    ) -> Task:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["key"] = key
        if status is not None:
            args["status"] = status
        return self.action("import_issue", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_issue"})


class Task(Resource):
    """Task — `tl.so.tilo.tasks.Task`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    @property
    def _2y3yxgz4(self) -> int:
        """number (`2y3yxgz4` on the wire)."""
        return self.fields["2y3yxgz4"]

    @_2y3yxgz4.setter
    def _2y3yxgz4(self, value: int) -> None:
        self.edit("2y3yxgz4", value=value)
    oidkt4g5: str  # title
    ftjnco6i: str  # notes
    @property
    def _2xssorsk(self) -> Status:
        """status (`2xssorsk` on the wire)."""
        return self.fields["2xssorsk"]

    @_2xssorsk.setter
    def _2xssorsk(self, value: Status) -> None:
        self.edit("2xssorsk", value=value)
    fgvmlm5n: bool  # done
    g6kykjp2: str | None  # due
    qadph346: str | None  # at
    aeilucpg: str | None  # remind
    @property
    def _2eyau2d6(self) -> str | None:
        """assignee (`2eyau2d6` on the wire)."""
        return self.fields["2eyau2d6"]

    @_2eyau2d6.setter
    def _2eyau2d6(self, value: str | None) -> None:
        self.edit("2eyau2d6", value=value)
    j3fym73f: Any | None  # contact
    ptyivutv: str | None  # assigned_by
    r5bd6ss3: str | None  # assigned_at
    tj32pwsz: list[str]  # labels
    fbioszhs: Project | None  # project
    @property
    def _4sriymst(self) -> Priority:
        """priority (`4sriymst` on the wire)."""
        return self.fields["4sriymst"]

    @_4sriymst.setter
    def _4sriymst(self, value: Priority) -> None:
        self.edit("4sriymst", value=value)
    nc5peol7: int | None  # estimate
    @property
    def _3pf74ifm(self) -> Task | None:
        """parent (`3pf74ifm` on the wire)."""
        return self.fields["3pf74ifm"]

    @_3pf74ifm.setter
    def _3pf74ifm(self, value: Task | None) -> None:
        self.edit("3pf74ifm", value=value)
    zjuuqsza: list[Any]  # attachments
    sqy7cbtd: Origin | None  # origin
    o6ijslz7: str | None  # url
    @property
    def _3uvt7tnr(self) -> Work | None:
        """work (`3uvt7tnr` on the wire)."""
        return self.fields["3uvt7tnr"]

    @_3uvt7tnr.setter
    def _3uvt7tnr(self, value: Work | None) -> None:
        self.edit("3uvt7tnr", value=value)
    uxlestrl: str  # waiting_for
    m6raucmj: str | None  # waiting_until
    vtgi2fsa: str  # waiting_on
    @property
    def _5ltuj6t2(self) -> list[Entry[Related]]:
        """related (`5ltuj6t2` on the wire)."""
        return self.fields["5ltuj6t2"]

    @_5ltuj6t2.setter
    def _5ltuj6t2(self, value: list[Entry[Related]]) -> None:
        self.edit("5ltuj6t2", value=value)

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[TaskEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[TaskInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[TaskDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def retitle(self, *, title: str) -> None:
        """Sets what a task is called."""
        return self.action("retitle", title=title)

    def retitle_many(self, items: list[RetitleArgs]) -> list[dict[str, Any]]:
        """Runs `retitle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retitle", items)

    def move_to(self,
        *,
        parent: Task | None = None,
        before: Task | None = None,
    ) -> None:
        """Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), or where `before` stands. What is put under a closed task opens it."""
        args: dict[str, Any] = {}
        if parent is not None:
            args["parent"] = parent
        if before is not None:
            args["before"] = before
        return self.action("move_to", **args)

    def move_to_many(self, items: list[MoveToArgs]) -> list[dict[str, Any]]:
        """Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to", items)

    def indent(self,
        *,
        above: Task | None = None,
    ) -> None:
        """Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does; with none named, under the task above it in the list."""
        args: dict[str, Any] = {}
        if above is not None:
            args["above"] = above
        return self.action("indent", **args)

    def indent_many(self, items: list[IndentArgs]) -> list[dict[str, Any]]:
        """Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("indent", items)

    def outdent(self) -> None:
        """Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline."""
        return self.action("outdent")

    def outdent_many(self, items: list[OutdentArgs]) -> list[dict[str, Any]]:
        """Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("outdent", items)

    def set_status(self,
        *,
        status: Status,
        at: int | None = None,
    ) -> None:
        """Puts a task in a column of the board, at the place `at` among the tasks already there: what a card's drag names. A drop in its own column only moves it among the column's tasks."""
        args: dict[str, Any] = {}
        args["status"] = status
        if at is not None:
            args["at"] = at
        return self.action("set_status", **args)

    def set_status_many(self, items: list[SetStatusArgs]) -> list[dict[str, Any]]:
        """Runs `set_status` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_status", items)

    def check(self,
        *,
        done: bool | None = None,
    ) -> None:
        """Checks a task off with every task under it, or opens it again with the tasks it is under."""
        args: dict[str, Any] = {}
        if done is not None:
            args["done"] = done
        return self.action("check", **args)

    def check_many(self, items: list[CheckArgs]) -> list[dict[str, Any]]:
        """Runs `check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("check", items)

    def add_subtask(self, *, title: str) -> Task:
        """Adds a subtask under a task, in its project and column, and opens a closed one: there is work under it now."""
        return self.action("add_subtask", title=title)

    def remove_empty_step(self) -> None:
        """Takes away the subtask the entry left empty: a field that began one and was left blank is no step of the work, so nothing empty stands under the task."""
        return self.action("remove_empty_step")

    def remove_empty_step_many(self, items: list[RemoveEmptyStepArgs]) -> list[dict[str, Any]]:
        """Runs `remove_empty_step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_empty_step", items)

    def due_today(self) -> None:
        """Sets the day a task is due by, from the presets or a day the person picks."""
        return self.action("due_today")

    def due_today_many(self, items: list[DueTodayArgs]) -> list[dict[str, Any]]:
        """Runs `due_today` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_today", items)

    def due_tomorrow(self) -> None:
        """Makes a task due tomorrow."""
        return self.action("due_tomorrow")

    def due_tomorrow_many(self, items: list[DueTomorrowArgs]) -> list[dict[str, Any]]:
        """Runs `due_tomorrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_tomorrow", items)

    def due_next_week(self) -> None:
        """Makes a task due at the start of next week."""
        return self.action("due_next_week")

    def due_next_week_many(self, items: list[DueNextWeekArgs]) -> list[dict[str, Any]]:
        """Runs `due_next_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_next_week", items)

    def schedule(self,
        *,
        due: str | None = None,
    ) -> None:
        """Makes a task due on a day the person picks. Its moment and its reminder move with it, at the same time of day: a task due Wednesday at six, reminded at six, moved to Friday is due Friday at six and reminded then."""
        args: dict[str, Any] = {}
        if due is not None:
            args["due"] = due
        return self.action("schedule", **args)

    def schedule_many(self, items: list[ScheduleArgs]) -> list[dict[str, Any]]:
        """Runs `schedule` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("schedule", items)

    def set_time(self,
        *,
        at: str | None = None,
    ) -> None:
        """Sets the time of day a task is due at: the day it is on stays, and a task with no day is due on the day the moment names. A reminder that was set for the old moment moves with it, as a day moved by `schedule` keeps its time of day."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("set_time", **args)

    def set_time_many(self, items: list[SetTimeArgs]) -> list[dict[str, Any]]:
        """Runs `set_time` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_time", items)

    def due_clear(self) -> None:
        """Clears the day a task is due by: it waits in the inbox, undated."""
        return self.action("due_clear")

    def due_clear_many(self, items: list[DueClearArgs]) -> list[dict[str, Any]]:
        """Runs `due_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_clear", items)

    def remind_at(self, *, when: str) -> None:
        """Sets the moment whoever is doing a task is reminded of it. A reminder on a day after the task is due moves the task to that day, its moment with it."""
        return self.action("remind_at", when=when)

    def remind_at_many(self, items: list[RemindAtArgs]) -> list[dict[str, Any]]:
        """Runs `remind_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remind_at", items)

    def remind_clear(self) -> None:
        """Clears a task's reminder: it is not told of again."""
        return self.action("remind_clear")

    def remind_clear_many(self, items: list[RemindClearArgs]) -> list[dict[str, Any]]:
        """Runs `remind_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remind_clear", items)

    def remind(self) -> None:
        """Tells whoever is doing a task, or the one who made it, what it is and when it is due: once, and not of a task already closed. Choosing the notice opens the task."""
        return self.action("remind")

    def remind_many(self, items: list[RemindArgs]) -> list[dict[str, Any]]:
        """Runs `remind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remind", items)

    def set_priority_none(self) -> None:
        """Sets how much a task matters."""
        return self.action("set_priority_none")

    def set_priority_none_many(self, items: list[SetPriorityNoneArgs]) -> list[dict[str, Any]]:
        """Runs `set_priority_none` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_priority_none", items)

    def set_priority_low(self) -> None:
        """Marks a task as low priority."""
        return self.action("set_priority_low")

    def set_priority_low_many(self, items: list[SetPriorityLowArgs]) -> list[dict[str, Any]]:
        """Runs `set_priority_low` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_priority_low", items)

    def set_priority_medium(self) -> None:
        """Marks a task as medium priority."""
        return self.action("set_priority_medium")

    def set_priority_medium_many(self, items: list[SetPriorityMediumArgs]) -> list[dict[str, Any]]:
        """Runs `set_priority_medium` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_priority_medium", items)

    def set_priority_high(self) -> None:
        """Marks a task as high priority."""
        return self.action("set_priority_high")

    def set_priority_high_many(self, items: list[SetPriorityHighArgs]) -> list[dict[str, Any]]:
        """Runs `set_priority_high` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_priority_high", items)

    def set_priority_urgent(self) -> None:
        """Marks a task as urgent."""
        return self.action("set_priority_urgent")

    def set_priority_urgent_many(self, items: list[SetPriorityUrgentArgs]) -> list[dict[str, Any]]:
        """Runs `set_priority_urgent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_priority_urgent", items)

    def estimate(self,
        *,
        points: int | None = None,
    ) -> None:
        """Sets the points a task is worth, or none."""
        args: dict[str, Any] = {}
        if points is not None:
            args["points"] = points
        return self.action("estimate", **args)

    def estimate_many(self, items: list[EstimateArgs]) -> list[dict[str, Any]]:
        """Runs `estimate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("estimate", items)

    def file(self, *, p: Project) -> None:
        """Files a task under a project the person picks."""
        return self.action("file", p=p)

    def file_many(self, items: list[FileArgs]) -> list[dict[str, Any]]:
        """Runs `file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("file", items)

    def unfile(self) -> None:
        """Takes a task out of its project, back to the inbox."""
        return self.action("unfile")

    def unfile_many(self, items: list[UnfileArgs]) -> list[dict[str, Any]]:
        """Runs `unfile` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unfile", items)

    def assign(self,
        *,
        who: str | None = None,
        contact: Any | None = None,
    ) -> None:
        """Gives a task to someone, or to nobody: a member of the workspace by their actor, or someone the workspace keeps in touch with by their contact. Who gave it to an agent, and when, stay on the task: the assignment is the change the agent's own turn is triggered by."""
        args: dict[str, Any] = {}
        if who is not None:
            args["who"] = who
        if contact is not None:
            args["contact"] = contact
        return self.action("assign", **args)

    def assign_many(self, items: list[AssignArgs]) -> list[dict[str, Any]]:
        """Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("assign", items)

    def relate(self,
        *,
        other: Task,
        how: str,
    ) -> None:
        """Links a task to another, with how they relate: the workspace's own link between two tasks, which the page of the task it relates to draws too."""
        return self.action("relate", other=other, how=how)

    def relate_many(self, items: list[RelateArgs]) -> list[dict[str, Any]]:
        """Runs `relate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("relate", items)

    def unrelate(self, *, relation: Related) -> None:
        """Takes a relation off a task."""
        return self.action("unrelate", relation=relation)

    def unrelate_many(self, items: list[UnrelateArgs]) -> list[dict[str, Any]]:
        """Runs `unrelate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unrelate", items)

    def label(self, *, name: str) -> None:
        """Gives a task a label."""
        return self.action("label", name=name)

    def label_many(self, items: list[LabelArgs]) -> list[dict[str, Any]]:
        """Runs `label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("label", items)

    def unlabel(self, *, name: str) -> None:
        """Takes a label off a task."""
        return self.action("unlabel", name=name)

    def unlabel_many(self, items: list[UnlabelArgs]) -> list[dict[str, Any]]:
        """Runs `unlabel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unlabel", items)

    def attach(self, *, file: Any) -> None:
        """Attaches a file to a task."""
        return self.action("attach", file=file)

    def attach_many(self, items: list[AttachArgs]) -> list[dict[str, Any]]:
        """Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("attach", items)

    def detach(self, *, attachment: Any) -> None:
        """Takes an attachment off a task."""
        return self.action("detach", attachment=attachment)

    def detach_many(self, items: list[DetachArgs]) -> list[dict[str, Any]]:
        """Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("detach", items)

    def open_attachment(self, *, attachment: Any) -> None:
        """Opens the attachment `attachment` in the app that opens its kind: the file a task holds reaches that app as the same content both read, never as a path neither has."""
        return self.action("open_attachment", attachment=attachment)

    def write_note(self) -> Any:
        """Writes a task's note in Notes: a note named like the task, holding what it remembers, made from the task so it reads under it."""
        return self.action("write_note")

    def wait(self,
        *,
        what: str,
        until: str | None = None,
        kind: WaitKind | None = None,
        on: str | None = None,
    ) -> None:
        """Puts a task on hold until what it waits for comes, or until `until` at the latest: whoever is doing it goes on with it then, a person or an agent alike. `on` names the automation that ends the wait early when it runs."""
        args: dict[str, Any] = {}
        args["what"] = what
        if until is not None:
            args["until"] = until
        if kind is not None:
            args["kind"] = kind
        if on is not None:
            args["on"] = on
        return self.action("wait", **args)

    def wait_many(self, items: list[WaitArgs]) -> list[dict[str, Any]]:
        """Runs `wait` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("wait", items)

    def resume(self,
        *,
        cause: str | None = None,
    ) -> None:
        """Ends every wait a task is on: what it waited for came, which `cause` says in a few words, and whoever is doing it goes on with it. It is also how a person starts its work over after its sessions were lost too often: the losses no longer stand in a row, and a session may begin."""
        args: dict[str, Any] = {}
        if cause is not None:
            args["cause"] = cause
        return self.action("resume", **args)

    def resume_many(self, items: list[ResumeArgs]) -> list[dict[str, Any]]:
        """Runs `resume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resume", items)

    def set_place(self,
        *,
        folder: str | None = None,
        note: str | None = None,
    ) -> None:
        """Sets where a task's working material is: the folder of its work, and the note its sessions write."""
        args: dict[str, Any] = {}
        if folder is not None:
            args["folder"] = folder
        if note is not None:
            args["note"] = note
        return self.action("set_place", **args)

    def set_place_many(self, items: list[SetPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `set_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_place", items)

    def set_budget(self,
        *,
        micros: int | None = None,
        millis: int | None = None,
        by: BudgetBy | None = None,
    ) -> None:
        """Sets what a task's work may spend and how long it may work, and whose budget it charges."""
        args: dict[str, Any] = {}
        if micros is not None:
            args["micros"] = micros
        if millis is not None:
            args["millis"] = millis
        if by is not None:
            args["by"] = by
        return self.action("set_budget", **args)

    def set_budget_many(self, items: list[SetBudgetArgs]) -> list[dict[str, Any]]:
        """Runs `set_budget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_budget", items)

    def set_pieces_at_once(self,
        *,
        pieces: int | None = None,
    ) -> None:
        """How many of a task's pieces may be in work at once."""
        args: dict[str, Any] = {}
        if pieces is not None:
            args["pieces"] = pieces
        return self.action("set_pieces_at_once", **args)

    def set_pieces_at_once_many(self, items: list[SetPiecesAtOnceArgs]) -> list[dict[str, Any]]:
        """Runs `set_pieces_at_once` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_pieces_at_once", items)

    def session_began(self) -> None:
        """Says a session on a task began: counted, and its start kept for how long it works."""
        return self.action("session_began")

    def session_began_many(self, items: list[SessionBeganArgs]) -> list[dict[str, Any]]:
        """Runs `session_began` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("session_began", items)

    def session_ended(self,
        *,
        spent_micros: int | None = None,
        worked_ms: int | None = None,
    ) -> None:
        """Says a session on a task ended: what it spent and how long it worked are added to the task."""
        args: dict[str, Any] = {}
        if spent_micros is not None:
            args["spent_micros"] = spent_micros
        if worked_ms is not None:
            args["worked_ms"] = worked_ms
        return self.action("session_ended", **args)

    def session_ended_many(self, items: list[SessionEndedArgs]) -> list[dict[str, Any]]:
        """Runs `session_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("session_ended", items)

    def stop(self,
        *,
        why: StoppedWhy,
        words: str | None = None,
    ) -> None:
        """Says a task's work stopped, and why, in the words its person reads."""
        args: dict[str, Any] = {}
        args["why"] = why
        if words is not None:
            args["words"] = words
        return self.action("stop", **args)

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def duplicate(self) -> Task:
        """Copies a task, with every task under it, just after the one it copies."""
        return self.action("duplicate")

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    def share_with(self, *, from_: Task) -> None:
        """Gives the copy `to` what `from` holds in its set, its list and its collection: what only an action may change."""
        args: dict[str, Any] = {}
        args["from"] = from_
        return self.action("share_with", **args)

    def share_with_many(self, items: list[ShareWithArgs]) -> list[dict[str, Any]]:
        """Runs `share_with` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("share_with", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "retitle", "retitle_many", "move_to", "move_to_many", "indent", "indent_many", "outdent", "outdent_many", "set_status", "set_status_many", "check", "check_many", "add_subtask", "remove_empty_step", "remove_empty_step_many", "due_today", "due_today_many", "due_tomorrow", "due_tomorrow_many", "due_next_week", "due_next_week_many", "schedule", "schedule_many", "set_time", "set_time_many", "due_clear", "due_clear_many", "remind_at", "remind_at_many", "remind_clear", "remind_clear_many", "remind", "remind_many", "set_priority_none", "set_priority_none_many", "set_priority_low", "set_priority_low_many", "set_priority_medium", "set_priority_medium_many", "set_priority_high", "set_priority_high_many", "set_priority_urgent", "set_priority_urgent_many", "estimate", "estimate_many", "file", "file_many", "unfile", "unfile_many", "assign", "assign_many", "relate", "relate_many", "unrelate", "unrelate_many", "label", "label_many", "unlabel", "unlabel_many", "attach", "attach_many", "detach", "detach_many", "open_attachment", "write_note", "wait", "wait_many", "resume", "resume_many", "set_place", "set_place_many", "set_budget", "set_budget_many", "set_pieces_at_once", "set_pieces_at_once_many", "session_began", "session_began_many", "session_ended", "session_ended_many", "stop", "stop_many", "duplicate", "share_with", "share_with_many"})


class Related(Resource):
    """Related — `tl.so.tilo.tasks.Related`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    h62xalud: Task  # task
    @property
    def _27gmtaed(self) -> str:
        """how (`27gmtaed` on the wire)."""
        return self.fields["27gmtaed"]

    @_27gmtaed.setter
    def _27gmtaed(self, value: str) -> None:
        self.edit("27gmtaed", value=value)


class Tasks(Resource):
    """Tasks — `tl.so.tilo.tasks.Tasks`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    lg5gedbh: list[Entry[Task]]  # items
    mdhllpbo: list[Entry[Project]]  # projects
    c7peh6l3: list[Entry[Origin]]  # origins
    auinwnh3: int  # numbers

    def new_task(self,
        *,
        p: Project | None = None,
        status: Status | None = None,
        due: str | None = None,
        at: str | None = None,
        assignee: str | None = None,
        title: str | None = None,
        remind: str | None = None,
        notes: str | None = None,
    ) -> Task:
        """Adds a task to the inbox or to `p` at `status`: the entry at a list's end fills `title` with the words nothing else there takes, and the day, moment and person they name, so 'Call the landlord fri 10am @Bo' is a task due Friday at ten, for Bo."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        if status is not None:
            args["status"] = status
        if due is not None:
            args["due"] = due
        if at is not None:
            args["at"] = at
        if assignee is not None:
            args["assignee"] = assignee
        if title is not None:
            args["title"] = title
        if remind is not None:
            args["remind"] = remind
        if notes is not None:
            args["notes"] = notes
        return self.action("new_task", **args)

    def new_project(self,
        *,
        notes: str | None = None,
    ) -> Project:
        """Starts a project, with what it is for when the caller says."""
        args: dict[str, Any] = {}
        if notes is not None:
            args["notes"] = notes
        return self.action("new_project", **args)

    def add_origin(self,
        *,
        author: str,
        source: str,
        handle: str | None = None,
        url: str | None = None,
        reference: str | None = None,
        created: str | None = None,
        updated: str | None = None,
    ) -> Origin:
        """Starts a task's work on another tracker's issue: its origin, its page there and its number, and a task of the same origin already here is that task, so importing it again makes none. Names where a task imported here was first written: one origin per place it came from."""
        args: dict[str, Any] = {}
        args["author"] = author
        args["source"] = source
        if handle is not None:
            args["handle"] = handle
        if url is not None:
            args["url"] = url
        if reference is not None:
            args["reference"] = reference
        if created is not None:
            args["created"] = created
        if updated is not None:
            args["updated"] = updated
        return self.action("add_origin", **args)

    def clear_done(self) -> int:
        """Deletes every finished task and answers how many went: a finished branch goes whole, and a done task with a step still open under it stays."""
        return self.action("clear_done")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_task", "new_project", "add_origin", "clear_done"})


class TiloTasks(Resource):
    """Tasks — `tl.so.tilo.tasks`."""
    __surface__ = "so.tilo.tasks@b4879eb6dcf9c567"
    def new_tasks(self,
        *,
        titles: list[str],
        p: Project | None = None,
        done: bool | None = None,
    ) -> list[Task]:
        """Adds every task of `titles` at once, as one change: none of them is made when one cannot be (a blank title), so a person's batch is whole or nothing. With `done`, they are work already finished, written down to keep: done, as checking each off would leave it."""
        args: dict[str, Any] = {}
        args["titles"] = titles
        if p is not None:
            args["p"] = p
        if done is not None:
            args["done"] = done
        return self.action("new_tasks", **args)

    def set_status_many(self,
        *,
        chosen: list[Task],
        status: Status,
        at: int | None = None,
    ) -> None:
        """Puts every chosen task in one column (or project) at once, as one change."""
        args: dict[str, Any] = {}
        args["chosen"] = chosen
        args["status"] = status
        if at is not None:
            args["at"] = at
        return self.action("set_status_many", **args)

    def set_status_many_many(self, items: list[SetStatusManyArgs]) -> list[dict[str, Any]]:
        """Runs `set_status_many` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_status_many", items)

    def assign_many(self,
        *,
        chosen: list[Task],
        who: str | None = None,
    ) -> None:
        """Gives every chosen task to one person (or nobody), as one change."""
        args: dict[str, Any] = {}
        args["chosen"] = chosen
        if who is not None:
            args["who"] = who
        return self.action("assign_many", **args)

    def assign_many_many(self, items: list[AssignManyArgs]) -> list[dict[str, Any]]:
        """Runs `assign_many` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("assign_many", items)

    def label_many(self,
        *,
        chosen: list[Task],
        name: str,
    ) -> None:
        """Puts every chosen task under one label, as one change."""
        return self.action("label_many", chosen=chosen, name=name)

    def label_many_many(self, items: list[LabelManyArgs]) -> list[dict[str, Any]]:
        """Runs `label_many` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("label_many", items)

    def import_files(self,
        *,
        files: list[Any],
        p: Project | None = None,
    ) -> list[Task]:
        """Imports files as tasks: one task for each file, under the project `p` when one is named, the file attached to it. A file of the same name already carried by a task is not imported twice."""
        args: dict[str, Any] = {}
        args["files"] = files
        if p is not None:
            args["p"] = p
        return self.action("import_files", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_tasks", "set_status_many", "set_status_many_many", "assign_many", "assign_many_many", "label_many", "label_many_many", "import_files"})


