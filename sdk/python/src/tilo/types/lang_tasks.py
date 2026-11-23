"""Types for Tasks (`so.tilo.lang.tasks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Task` is the app's `tl.so.tilo.lang.tasks.Task` at `tasks/<resource>`, read with
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
    "Priority",
    "Task",
    "Project",
    "Section",
    "Tasks",
    "LangTasks",
    "CheckArgs",
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
    "FileArgs",
    "UnfileArgs",
    "AssignArgs",
    "IndentArgs",
    "OutdentArgs",
    "PlaceArgs",
    "RenameArgs",
    "RenameSectionArgs",
    "RemoveSectionArgs",
    "MoveToArgs",
    "RemoveProjectArgs",
]

Priority = Literal["none", "low", "medium", "high", "urgent"]
"""How much a task matters, most urgent last."""

CheckArgs = TypedDict("CheckArgs", {
    "done": NotRequired["bool | None"],
})
"""The arguments of `check`."""

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

FileArgs = TypedDict("FileArgs", {
    "p": "Project",
})
"""The arguments of `file`."""

UnfileArgs = TypedDict("UnfileArgs", {
})
"""The arguments of `unfile`."""

AssignArgs = TypedDict("AssignArgs", {
    "who": NotRequired["str | None"],
})
"""The arguments of `assign`."""

IndentArgs = TypedDict("IndentArgs", {
    "above": NotRequired["Task | None"],
})
"""The arguments of `indent`."""

OutdentArgs = TypedDict("OutdentArgs", {
})
"""The arguments of `outdent`."""

PlaceArgs = TypedDict("PlaceArgs", {
    "s": NotRequired["Section | None"],
    "at": "int",
})
"""The arguments of `place`."""

RenameArgs = TypedDict("RenameArgs", {
    "name": "str",
})
"""The arguments of `rename`."""

RenameSectionArgs = TypedDict("RenameSectionArgs", {
    "name": "str",
})
"""The arguments of `rename_section`."""

RemoveSectionArgs = TypedDict("RemoveSectionArgs", {
})
"""The arguments of `remove_section`."""

MoveToArgs = TypedDict("MoveToArgs", {
    "moved": "Task",
    "parent": NotRequired["Task | None"],
    "before": NotRequired["Task | None"],
})
"""The arguments of `move_to`."""

RemoveProjectArgs = TypedDict("RemoveProjectArgs", {
    "p": "Project",
})
"""The arguments of `remove_project`."""

class Task(Resource):
    """Task — `tl.so.tilo.lang.tasks.Task`."""
    __surface__ = "so.tilo.lang.tasks@e57366bbdfb4b3bf"
    oidkt4g5: str  # title
    fgvmlm5n: bool  # done
    g6kykjp2: str | None  # due
    qadph346: str | None  # at
    aeilucpg: str | None  # remind
    ftjnco6i: str  # notes
    @property
    def _2eyau2d6(self) -> str | None:
        """assignee (`2eyau2d6` on the wire)."""
        return self.fields["2eyau2d6"]

    @_2eyau2d6.setter
    def _2eyau2d6(self, value: str | None) -> None:
        self.edit("2eyau2d6", value=value)
    fbioszhs: Project | None  # project
    v56iknh2: Section | None  # section
    @property
    def _4sriymst(self) -> Priority:
        """priority (`4sriymst` on the wire)."""
        return self.fields["4sriymst"]

    @_4sriymst.setter
    def _4sriymst(self, value: Priority) -> None:
        self.edit("4sriymst", value=value)
    @property
    def _3pf74ifm(self) -> Task | None:
        """parent (`3pf74ifm` on the wire)."""
        return self.fields["3pf74ifm"]

    @_3pf74ifm.setter
    def _3pf74ifm(self, value: Task | None) -> None:
        self.edit("3pf74ifm", value=value)

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
        """Sets the time of day a task is due at: the day it is on stays, and a task with no day is due on the day the moment names. A reminder that was set for the old moment moves with it, as a day moved by `schedule` keeps its time of day: a task due Wednesday at six, reminded at six, made seven is due at seven and reminded then."""
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
        """Sets the moment whoever is doing a task is reminded of it. A reminder on a day after the task is due moves the task to that day, its moment with it: being told of a task once its day has gone is no reminder, so 'move the reminder to Friday' moves both."""
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
        """Tells whoever is doing a task, or the one who made it, what it is and when it is due: once, and not of a task already done. Choosing the notice opens the task."""
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
    ) -> None:
        """Gives a task to someone, or to nobody."""
        args: dict[str, Any] = {}
        if who is not None:
            args["who"] = who
        return self.action("assign", **args)

    def assign_many(self, items: list[AssignArgs]) -> list[dict[str, Any]]:
        """Runs `assign` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("assign", items)

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

    def place(self,
        *,
        at: int,
        s: Section | None = None,
    ) -> None:
        """Puts a task into the list `s` of its project's board (none for the first) at the place `at` among the tasks already there: what a card's drag names."""
        args: dict[str, Any] = {}
        if s is not None:
            args["s"] = s
        args["at"] = at
        return self.action("place", **args)

    def place_many(self, items: list[PlaceArgs]) -> list[dict[str, Any]]:
        """Runs `place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"check", "check_many", "due_today", "due_today_many", "due_tomorrow", "due_tomorrow_many", "due_next_week", "due_next_week_many", "schedule", "schedule_many", "set_time", "set_time_many", "due_clear", "due_clear_many", "remind_at", "remind_at_many", "remind_clear", "remind_clear_many", "remind", "remind_many", "set_priority_none", "set_priority_none_many", "set_priority_low", "set_priority_low_many", "set_priority_medium", "set_priority_medium_many", "set_priority_high", "set_priority_high_many", "set_priority_urgent", "set_priority_urgent_many", "file", "file_many", "unfile", "unfile_many", "assign", "assign_many", "indent", "indent_many", "outdent", "outdent_many", "place", "place_many"})


class Project(Resource):
    """Project — `tl.so.tilo.lang.tasks.Project`."""
    __surface__ = "so.tilo.lang.tasks@e57366bbdfb4b3bf"
    sgnau7vd: str  # name
    c5pgzhqt: str  # notes

    def new_section(self) -> Section:
        """Adds a section at the end of a project's board."""
        return self.action("new_section")

    def add_to(self,
        *,
        title: str,
        s: Section | None = None,
    ) -> Task:
        """Adds a task to a project, at the end of the list `s` of its board (none for the first). It is the project's primary action, so its key brings the person to the entry that adds there."""
        args: dict[str, Any] = {}
        args["title"] = title
        if s is not None:
            args["s"] = s
        return self.action("add_to", **args)

    def rename(self, *, name: str) -> None:
        """Renames a project."""
        return self.action("rename", name=name)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_section", "add_to", "rename", "rename_many"})


class Section(Resource):
    """Section — `tl.so.tilo.lang.tasks.Section`."""
    __surface__ = "so.tilo.lang.tasks@e57366bbdfb4b3bf"
    yfd266wj: str  # name
    ohbjjsu7: Project  # project

    def rename_section(self, *, name: str) -> None:
        """Renames a section."""
        return self.action("rename_section", name=name)

    def rename_section_many(self, items: list[RenameSectionArgs]) -> list[dict[str, Any]]:
        """Runs `rename_section` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_section", items)

    def remove_section(self) -> None:
        """Deletes a section; its tasks go to the project's first list."""
        return self.action("remove_section")

    def remove_section_many(self, items: list[RemoveSectionArgs]) -> list[dict[str, Any]]:
        """Runs `remove_section` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_section", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rename_section", "rename_section_many", "remove_section", "remove_section_many"})


class Tasks(Resource):
    """Tasks — `tl.so.tilo.lang.tasks.Tasks`."""
    __surface__ = "so.tilo.lang.tasks@e57366bbdfb4b3bf"
    lg5gedbh: list[Entry[Task]]  # items
    mdhllpbo: list[Entry[Project]]  # projects
    nk5542m7: list[Entry[Section]]  # sections

    def new_task(self,
        *,
        p: Project | None = None,
        s: Section | None = None,
        due: str | None = None,
        at: str | None = None,
        assignee: str | None = None,
        title: str | None = None,
        remind: str | None = None,
    ) -> Task:
        """Adds a task to the inbox or to `p`, in the list `s` of its board: the entry at a list's end fills `title` with the words nothing else there takes, and the day, moment and person they name, so 'Call the landlord fri 10am @Bo' is a task due Friday at ten, for Bo."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        if s is not None:
            args["s"] = s
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
        return self.action("new_task", **args)

    def new_project(self) -> Project:
        """Starts a project."""
        return self.action("new_project")

    def add_subtask(self,
        *,
        t: Task,
        title: str,
    ) -> Task:
        """Adds a subtask under a task, in its project and section, and opens a done one: there is work under it now."""
        return self.action("add_subtask", t=t, title=title)

    def move_to(self,
        *,
        moved: Task,
        parent: Task | None = None,
        before: Task | None = None,
    ) -> None:
        """Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for none), or where `before` stands. What is put under a done task opens it."""
        args: dict[str, Any] = {}
        args["moved"] = moved
        if parent is not None:
            args["parent"] = parent
        if before is not None:
            args["before"] = before
        return self.action("move_to", **args)

    def move_to_many(self, items: list[MoveToArgs]) -> list[dict[str, Any]]:
        """Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to", items)

    def duplicate(self, *, t: Task) -> Task:
        """Copies a task, with every task under it, just after the one it copies."""
        return self.action("duplicate", t=t)

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    def clear_done(self) -> int:
        """Deletes every finished task and answers how many went: a finished branch goes whole, and a done task with a step still open under it stays. What was done is the list a person reads back, so this is no command of the bar."""
        return self.action("clear_done")

    def remove_project(self, *, p: Project) -> None:
        """Deletes a project and its sections; its tasks go back to the inbox."""
        return self.action("remove_project", p=p)

    def remove_project_many(self, items: list[RemoveProjectArgs]) -> list[dict[str, Any]]:
        """Runs `remove_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_project", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_task", "new_project", "add_subtask", "move_to", "move_to_many", "duplicate", "clear_done", "remove_project", "remove_project_many"})


class LangTasks(Resource):
    """Tasks — `tl.so.tilo.lang.tasks`."""
    __surface__ = "so.tilo.lang.tasks@e57366bbdfb4b3bf"

