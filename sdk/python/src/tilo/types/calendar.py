"""Types for Calendar (`so.tilo.calendar`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `calendar/<resource>`, read with
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
    "Color",
    "Priority",
    "Repeat",
    "Notice",
    "Attendee",
    "Account",
    "Project",
    "Task",
    "Event",
    "Calendar",
    "Calendars",
    "Busy",
    "Moment",
    "TiloCalendar",
    "SetProjectColorArgs",
    "RenameProjectArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "CheckTaskArgs",
    "DueTaskArgs",
    "DueTaskTodayArgs",
    "DueTaskTomorrowArgs",
    "DueTaskNextWeekArgs",
    "DueTaskClearArgs",
    "SetTaskPriorityArgs",
    "FileTaskArgs",
    "EventEditBlockArgs",
    "EventInsertBlockArgs",
    "EventDeleteBlockArgs",
    "FileEventArgs",
    "InviteByMailArgs",
    "GoToArgs",
    "RemoveTaskArgs",
    "MoveProjectArgs",
    "RemoveProjectArgs",
    "GoToDayArgs",
    "PreviousDayArgs",
    "NextDayArgs",
    "ThisDayArgs",
    "PreviousWeekArgs",
    "NextWeekArgs",
    "PreviousMonthArgs",
    "NextMonthArgs",
    "ThisWeekArgs",
]

Color = Literal["indigo", "emerald", "amber", "red", "blue", "violet", "slate"]
"""The colours a calendar, an event or a project can take, each a place in the theme."""

Priority = Literal["none", "low", "medium", "high"]
"""How much a task matters, most urgent last."""

Repeat = Literal["none", "daily", "weekly", "monthly", "yearly", "weekdays"]
"""How an event repeats: every day, week, month or year, or on chosen days of the week."""

SetProjectColorArgs = TypedDict("SetProjectColorArgs", {
    "color": "Color",
})
"""The arguments of `set_project_color`."""

RenameProjectArgs = TypedDict("RenameProjectArgs", {
    "name": "str",
})
"""The arguments of `rename_project`."""

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

CheckTaskArgs = TypedDict("CheckTaskArgs", {
    "done": NotRequired["bool | None"],
})
"""The arguments of `check_task`."""

DueTaskArgs = TypedDict("DueTaskArgs", {
    "due": NotRequired["str | None"],
})
"""The arguments of `due_task`."""

DueTaskTodayArgs = TypedDict("DueTaskTodayArgs", {
})
"""The arguments of `due_task_today`."""

DueTaskTomorrowArgs = TypedDict("DueTaskTomorrowArgs", {
})
"""The arguments of `due_task_tomorrow`."""

DueTaskNextWeekArgs = TypedDict("DueTaskNextWeekArgs", {
})
"""The arguments of `due_task_next_week`."""

DueTaskClearArgs = TypedDict("DueTaskClearArgs", {
})
"""The arguments of `due_task_clear`."""

SetTaskPriorityArgs = TypedDict("SetTaskPriorityArgs", {
    "priority": "Priority",
})
"""The arguments of `set_task_priority`."""

FileTaskArgs = TypedDict("FileTaskArgs", {
    "p": NotRequired["Project | None"],
})
"""The arguments of `file_task`."""

EventEditBlockArgs = TypedDict("EventEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

EventInsertBlockArgs = TypedDict("EventInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

EventDeleteBlockArgs = TypedDict("EventDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

FileEventArgs = TypedDict("FileEventArgs", {
    "p": NotRequired["Project | None"],
})
"""The arguments of `file_event`."""

InviteByMailArgs = TypedDict("InviteByMailArgs", {
    "g": "Attendee",
})
"""The arguments of `invite_by_mail`."""

GoToArgs = TypedDict("GoToArgs", {
})
"""The arguments of `go_to`."""

RemoveTaskArgs = TypedDict("RemoveTaskArgs", {
    "t": "Task",
})
"""The arguments of `remove_task`."""

MoveProjectArgs = TypedDict("MoveProjectArgs", {
    "p": "Project",
    "up": "bool",
})
"""The arguments of `move_project`."""

RemoveProjectArgs = TypedDict("RemoveProjectArgs", {
    "p": "Project",
})
"""The arguments of `remove_project`."""

GoToDayArgs = TypedDict("GoToDayArgs", {
    "at": "str",
})
"""The arguments of `go_to_day`."""

PreviousDayArgs = TypedDict("PreviousDayArgs", {
})
"""The arguments of `previous_day`."""

NextDayArgs = TypedDict("NextDayArgs", {
})
"""The arguments of `next_day`."""

ThisDayArgs = TypedDict("ThisDayArgs", {
})
"""The arguments of `this_day`."""

PreviousWeekArgs = TypedDict("PreviousWeekArgs", {
})
"""The arguments of `previous_week`."""

NextWeekArgs = TypedDict("NextWeekArgs", {
})
"""The arguments of `next_week`."""

PreviousMonthArgs = TypedDict("PreviousMonthArgs", {
})
"""The arguments of `previous_month`."""

NextMonthArgs = TypedDict("NextMonthArgs", {
})
"""The arguments of `next_month`."""

ThisWeekArgs = TypedDict("ThisWeekArgs", {
})
"""The arguments of `this_week`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
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


class Attendee(Resource):
    """Attendee — `tl.so.tilo.calendar.Attendee`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    oiolhfhy: str  # name
    zr5l6sfu: str  # email
    wyfwksgq: str  # status
    fweteite: str  # role


class Account(Resource):
    """Account — `tl.so.tilo.calendar.Account`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    f44j5f26: str  # key
    gxn236ef: str  # address
    rfeeyuff: str  # name
    turzovgi: str  # server
    vupfntcd: bool  # read_only
    cgmndfzg: str  # trouble
    yfntdxb7: bool  # online
    @property
    def _5ebgjcbp(self) -> bool:
        """checking (`5ebgjcbp` on the wire)."""
        return self.fields["5ebgjcbp"]

    @_5ebgjcbp.setter
    def _5ebgjcbp(self, value: bool) -> None:
        self.edit("5ebgjcbp", value=value)
    qmpjl3oc: str  # fetched

    def sync_account(self) -> None:
        """Asks one account's server for its calendars now: what 'Sync again' over its notice does."""
        return self.action("sync_account")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"sync_account"})


class Project(Resource):
    """Project — `tl.so.tilo.calendar.Project`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    iivr4dfm: str  # name
    @property
    def _5kprlx3s(self) -> Color:
        """color (`5kprlx3s` on the wire)."""
        return self.fields["5kprlx3s"]

    @_5kprlx3s.setter
    def _5kprlx3s(self, value: Color) -> None:
        self.edit("5kprlx3s", value=value)
    fsndgah6: int  # order

    def set_project_color(self, *, color: Color) -> None:
        """Sets the colour a project is drawn in."""
        return self.action("set_project_color", color=color)

    def set_project_color_many(self, items: list[SetProjectColorArgs]) -> list[dict[str, Any]]:
        """Runs `set_project_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_project_color", items)

    def rename_project(self, *, name: str) -> None:
        """Renames a project."""
        return self.action("rename_project", name=name)

    def rename_project_many(self, items: list[RenameProjectArgs]) -> list[dict[str, Any]]:
        """Runs `rename_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_project", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_project_color", "set_project_color_many", "rename_project", "rename_project_many"})


class Task(Resource):
    """Task — `tl.so.tilo.calendar.Task`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    r2vx64eh: str  # title
    s6raiajy: bool  # done
    kibl2yut: str | None  # due
    @property
    def _2o4hdoux(self) -> str | None:
        """remind (`2o4hdoux` on the wire)."""
        return self.fields["2o4hdoux"]

    @_2o4hdoux.setter
    def _2o4hdoux(self, value: str | None) -> None:
        self.edit("2o4hdoux", value=value)
    ivfbe5bc: str  # notes
    whecqzmz: Priority  # priority
    en7w5zf7: Project | None  # project
    xna3jvyc: Task | None  # parent

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

    def check_task(self,
        *,
        done: bool | None = None,
    ) -> None:
        """Checks a task off, or opens it again."""
        args: dict[str, Any] = {}
        if done is not None:
            args["done"] = done
        return self.action("check_task", **args)

    def check_task_many(self, items: list[CheckTaskArgs]) -> list[dict[str, Any]]:
        """Runs `check_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("check_task", items)

    def due_task(self,
        *,
        due: str | None = None,
    ) -> None:
        """Sets the day a task is due by."""
        args: dict[str, Any] = {}
        if due is not None:
            args["due"] = due
        return self.action("due_task", **args)

    def due_task_many(self, items: list[DueTaskArgs]) -> list[dict[str, Any]]:
        """Runs `due_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_task", items)

    def due_task_today(self) -> None:
        """Makes a task due today."""
        return self.action("due_task_today")

    def due_task_today_many(self, items: list[DueTaskTodayArgs]) -> list[dict[str, Any]]:
        """Runs `due_task_today` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_task_today", items)

    def due_task_tomorrow(self) -> None:
        """Makes a task due tomorrow."""
        return self.action("due_task_tomorrow")

    def due_task_tomorrow_many(self, items: list[DueTaskTomorrowArgs]) -> list[dict[str, Any]]:
        """Runs `due_task_tomorrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_task_tomorrow", items)

    def due_task_next_week(self) -> None:
        """Makes a task due at the start of next week."""
        return self.action("due_task_next_week")

    def due_task_next_week_many(self, items: list[DueTaskNextWeekArgs]) -> list[dict[str, Any]]:
        """Runs `due_task_next_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_task_next_week", items)

    def due_task_clear(self) -> None:
        """Clears the day a task is due by."""
        return self.action("due_task_clear")

    def due_task_clear_many(self, items: list[DueTaskClearArgs]) -> list[dict[str, Any]]:
        """Runs `due_task_clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("due_task_clear", items)

    def set_task_priority(self, *, priority: Priority) -> None:
        """Sets how much a task matters."""
        return self.action("set_task_priority", priority=priority)

    def set_task_priority_many(self, items: list[SetTaskPriorityArgs]) -> list[dict[str, Any]]:
        """Runs `set_task_priority` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_task_priority", items)

    def file_task(self,
        *,
        p: Project | None = None,
    ) -> None:
        """Files a task under a project, or under none for the inbox."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        return self.action("file_task", **args)

    def file_task_many(self, items: list[FileTaskArgs]) -> list[dict[str, Any]]:
        """Runs `file_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("file_task", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "check_task", "check_task_many", "due_task", "due_task_many", "due_task_today", "due_task_today_many", "due_task_tomorrow", "due_task_tomorrow_many", "due_task_next_week", "due_task_next_week_many", "due_task_clear", "due_task_clear_many", "set_task_priority", "set_task_priority_many", "file_task", "file_task_many"})


class Event(Resource):
    """Event — `tl.so.tilo.calendar.Event`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    qqkexjof: str  # key
    yclpjnie: str  # href
    lpmkddba: str  # etag
    @property
    def _5b3dmvgx(self) -> str:
        """title (`5b3dmvgx` on the wire)."""
        return self.fields["5b3dmvgx"]

    @_5b3dmvgx.setter
    def _5b3dmvgx(self, value: str) -> None:
        self.edit("5b3dmvgx", value=value)
    f36rh6hq: str  # start
    ahyl252g: str  # end
    yyqvqgb4: bool  # all_day
    dmlhzgmz: str  # timezone
    mvt5vjkt: str  # location
    gqmucsa7: str  # notes
    igx7zgee: str  # body
    ypsmw6x6: list[Entry[Attendee]]  # attendees
    ayba7si7: list[str]  # categories
    phwonegh: str  # recurrence
    @property
    def _6oxxhsol(self) -> Repeat:
        """repeat (`6oxxhsol` on the wire)."""
        return self.fields["6oxxhsol"]

    @_6oxxhsol.setter
    def _6oxxhsol(self, value: Repeat) -> None:
        self.edit("6oxxhsol", value=value)
    zjilh6px: int  # recurrence_every
    uyipmxcj: str | None  # recurrence_until
    zzjroxh5: int | None  # recurrence_count
    dn2juryi: list[str]  # recurrence_weekdays
    xgv6mdnt: list[str]  # exdates
    js6qgkt2: str  # day
    jz3mybhh: str  # end_day
    zwcmfzky: str  # color
    qe2xcyxt: str  # project
    p2upd4ee: Project | None  # filed_under
    l44x7h7l: str  # organizer
    n3y6kpek: bool  # cancelled
    @property
    def _7mqdwjrk(self) -> Calendar | None:
        """calendar (`7mqdwjrk` on the wire)."""
        return self.fields["7mqdwjrk"]

    @_7mqdwjrk.setter
    def _7mqdwjrk(self, value: Calendar | None) -> None:
        self.edit("7mqdwjrk", value=value)
    @property
    def _72habrff(self) -> str:
        """remind (`72habrff` on the wire)."""
        return self.fields["72habrff"]

    @_72habrff.setter
    def _72habrff(self, value: str) -> None:
        self.edit("72habrff", value=value)
    @property
    def _2ukd2xzk(self) -> Event | None:
        """recurrence_of (`2ukd2xzk` on the wire)."""
        return self.fields["2ukd2xzk"]

    @_2ukd2xzk.setter
    def _2ukd2xzk(self, value: Event | None) -> None:
        self.edit("2ukd2xzk", value=value)
    x6t7zpds: str  # original_start

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[EventEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[EventInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[EventDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def dropped(self,
        *,
        start: str,
        place: str,
    ) -> None:
        """Moves an event where it was dropped: a day chip moves to that day keeping its time of day, and an hour moves to the hour it was dropped at. A repeating event waits for its own answer."""
        return self.action("dropped", start=start, place=place)

    def ended(self, *, end_at: str) -> None:
        """Ends an event where its edge was dropped. A repeating event waits for its own answer."""
        return self.action("ended", end_at=end_at)

    def file_event(self,
        *,
        p: Project | None = None,
    ) -> None:
        """Files an event under a project, or files it under none."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        return self.action("file_event", **args)

    def file_event_many(self, items: list[FileEventArgs]) -> list[dict[str, Any]]:
        """Runs `file_event` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("file_event", items)

    def set_event_color(self, *, color: Color) -> None:
        """Sets the colour an event is drawn in."""
        return self.action("set_event_color", color=color)

    def clear_event_color(self) -> None:
        """Takes an event's own colour off: it is drawn in its calendar's."""
        return self.action("clear_event_color")

    def set_categories(self, *, categories: list[str]) -> None:
        """Sets the categories an event is filed under in its person's words."""
        return self.action("set_categories", categories=categories)

    def set_zone(self, *, zone: str) -> None:
        """Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for the same time of day everywhere."""
        return self.action("set_zone", zone=zone)

    def add_category(self, *, category: str) -> None:
        """Files an event under one more category."""
        return self.action("add_category", category=category)

    def remove_category(self, *, category: str) -> None:
        """Takes a category off an event."""
        return self.action("remove_category", category=category)

    def organize(self, *, who: str) -> None:
        """Makes someone the organiser of an event."""
        return self.action("organize", who=who)

    def remind_at_start(self) -> None:
        """Reminds the people coming when the event starts."""
        return self.action("remind_at_start")

    def remind_before(self, *, minutes: int) -> None:
        """Reminds them a given number of minutes before it starts."""
        return self.action("remind_before", minutes=minutes)

    def remind_off(self) -> None:
        """Tells nobody, once more."""
        return self.action("remind_off")

    def repeat_rule(self,
        *,
        how: Repeat,
        every: int | None = None,
        weekdays: list[str] | None = None,
        until: str | None = None,
        count: int | None = None,
    ) -> None:
        """Repeats an event, its rule in iCalendar's words, how often, and where it ends."""
        args: dict[str, Any] = {}
        args["how"] = how
        if every is not None:
            args["every"] = every
        if weekdays is not None:
            args["weekdays"] = weekdays
        if until is not None:
            args["until"] = until
        if count is not None:
            args["count"] = count
        return self.action("repeat_rule", **args)

    def repeat_weekdays(self, *, days: list[str]) -> None:
        """Repeats an event on chosen days of the week from the day it stands on."""
        return self.action("repeat_weekdays", days=days)

    def toggle_weekday(self, *, day: str) -> None:
        """Adds a weekday to a weekly repeat, or takes it off: the days chosen are the rule's `BYDAY`, and the repeat keeps the day it ends on, if it ends on one."""
        return self.action("toggle_weekday", day=day)

    def repeat_off(self) -> None:
        """Stops an event repeating from the day it stands on."""
        return self.action("repeat_off")

    def skip_date(self, *, day: str) -> None:
        """Skips one date of a repeating event."""
        return self.action("skip_date", day=day)

    def restore_date(self, *, day: str) -> None:
        """Gives a skipped date of a repeating event back."""
        return self.action("restore_date", day=day)

    def move_occurrence(self,
        *,
        at: str,
        start: str,
        end: str,
    ) -> None:
        """Moves one date of a repeating event: an event of its own stands in its place and the series keeps every other date."""
        return self.action("move_occurrence", at=at, start=start, end=end)

    def cancel_occurrence(self, *, at: str) -> None:
        """Cancels one date of a repeating event."""
        return self.action("cancel_occurrence", at=at)

    def restore_occurrence(self, *, at: str) -> None:
        """Gives a cancelled or moved date of a series back."""
        return self.action("restore_occurrence", at=at)

    def move_following(self,
        *,
        at: str,
        start: str,
        end: str,
    ) -> None:
        """Moves a repeating event from one date on: the series ends the day before, and a new one begins where it was moved to, with every date after it."""
        return self.action("move_following", at=at, start=start, end=end)

    def move_to_day(self, *, day: str) -> None:
        """Moves an event to a day, keeping the time of day it starts and how long it lasts, and its reminder at the same distance from its start."""
        return self.action("move_to_day", day=day)

    def change_day(self, *, at: str) -> None:
        """Moves an event to the day a moment names, keeping the time of day it starts."""
        return self.action("change_day", at=at)

    def change_start(self, *, at: str) -> None:
        """Sets the time of day an event starts, its end keeping how long it lasts. A day the moment names is the day it moves to."""
        return self.action("change_start", at=at)

    def change_end(self, *, at: str) -> None:
        """Sets when an event ends."""
        return self.action("change_end", at=at)

    def move_end_day(self, *, at: str) -> None:
        """Ends an event on the day a moment names, keeping the time of day it ends at: a day after the one it starts on makes it run through every day between, and a whole-day event ends at the midnight after its last day, which is the day it shows on."""
        return self.action("move_end_day", at=at)

    def end_on_day(self, *, end_day: str) -> None:
        """Ends an event on a day, keeping the time of day it ends at, as `move_end_day` does with the day a moment names."""
        return self.action("end_on_day", end_day=end_day)

    def repeat(self, *, how: Repeat) -> None:
        """Repeats an event every day, week, month or year from the day it stands on, on the days chosen of a week, and no longer on the days it named before."""
        return self.action("repeat", how=how)

    def repeat_every(self, *, every: int) -> None:
        """How often it repeats, in the unit it repeats in: every two weeks is 2."""
        return self.action("repeat_every", every=every)

    def repeat_until(self,
        *,
        until: str | None = None,
    ) -> None:
        """The last day it repeats by, or none for a series that does not end."""
        args: dict[str, Any] = {}
        if until is not None:
            args["until"] = until
        return self.action("repeat_until", **args)

    def repeat_until_at(self, *, at: str) -> None:
        """The last day it repeats by, as the day a moment names."""
        return self.action("repeat_until_at", at=at)

    def repeat_count(self,
        *,
        count: int | None = None,
    ) -> None:
        """How many times it repeats in all, or none for a series that does not end."""
        args: dict[str, Any] = {}
        if count is not None:
            args["count"] = count
        return self.action("repeat_count", **args)

    def file_in(self, *, k: Calendar) -> None:
        """Files an event under a calendar the person picks."""
        return self.action("file_in", k=k)

    def invite(self,
        *,
        name: str,
        email: str,
    ) -> Attendee:
        """Invites someone to an event, by the name and address the person gives."""
        return self.action("invite", name=name, email=email)

    def answer(self,
        *,
        g: Attendee,
        status: str,
    ) -> None:
        """Records someone's answer to an invitation: `accepted`, `declined` or `tentative`."""
        return self.action("answer", g=g, status=status)

    def answer_and_tell(self,
        *,
        g: Attendee,
        said: str,
    ) -> None:
        """Answers the person's own invitation to an account's event, telling the organiser through the account's server: what settles the RSVP the invitation asked for."""
        return self.action("answer_and_tell", g=g, said=said)

    def uninvite(self, *, g: Attendee) -> None:
        """Takes someone off an event's guests: nothing is sent to them."""
        return self.action("uninvite", g=g)

    def invite_by_mail(self, *, g: Attendee) -> None:
        """Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail shows the person, to read and send. Nothing is sent by this."""
        return self.action("invite_by_mail", g=g)

    def invite_by_mail_many(self, items: list[InviteByMailArgs]) -> list[dict[str, Any]]:
        """Runs `invite_by_mail` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("invite_by_mail", items)

    def ring_due(self) -> None:
        """Rings an event's reminder when its moment comes, and nothing for an event that has none."""
        return self.action("ring_due")

    def remind_now(self) -> None:
        """Tells the people coming that an event starts, once, and clears its reminder."""
        return self.action("remind_now")

    def go_to(self) -> None:
        """Goes to the week an event is on."""
        return self.action("go_to")

    def go_to_many(self, items: list[GoToArgs]) -> list[dict[str, Any]]:
        """Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to", items)

    def repeat_every_text(self, *, typed: str) -> None:
        """Repeats an event how often the person typed, in the unit it repeats in: 2 for every other."""
        return self.action("repeat_every_text", typed=typed)

    def repeat_count_text(self, *, typed: str) -> None:
        """Repeats an event as many times as the person typed."""
        return self.action("repeat_count_text", typed=typed)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "dropped", "ended", "file_event", "file_event_many", "set_event_color", "clear_event_color", "set_categories", "set_zone", "add_category", "remove_category", "organize", "remind_at_start", "remind_before", "remind_off", "repeat_rule", "repeat_weekdays", "toggle_weekday", "repeat_off", "skip_date", "restore_date", "move_occurrence", "cancel_occurrence", "restore_occurrence", "move_following", "move_to_day", "change_day", "change_start", "change_end", "move_end_day", "end_on_day", "repeat", "repeat_every", "repeat_until", "repeat_until_at", "repeat_count", "file_in", "invite", "answer", "answer_and_tell", "uninvite", "invite_by_mail", "invite_by_mail_many", "ring_due", "remind_now", "go_to", "go_to_many", "repeat_every_text", "repeat_count_text"})


class Calendar(Resource):
    """Calendar — `tl.so.tilo.calendar.Calendar`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    gzrwihvd: str  # key
    pwrd7qqx: str  # account
    dn3prfqt: str  # sync_token
    kc5ls3bi: str  # name
    kaeu2eyo: str  # color
    xm42jvvk: bool  # writable

    def set_calendar_color(self, *, color: Color) -> None:
        """Sets the colour a calendar is drawn in."""
        return self.action("set_calendar_color", color=color)

    def rename_calendar(self, *, name: str) -> None:
        """Renames a calendar."""
        return self.action("rename_calendar", name=name)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_calendar_color", "rename_calendar"})


class Calendars(Resource):
    """Calendars — `tl.so.tilo.calendar.Calendars`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    g3gq3jmz: list[Entry[Event]]  # events
    ozulkrgt: list[Entry[Calendar]]  # calendars
    lubytvuv: list[Entry[Account]]  # accounts
    jum24lyb: list[Entry[Task]]  # tasks
    japv422q: list[Entry[Project]]  # projects
    i5l2mswn: str  # anchor_at

    def new_event(self,
        *,
        title: str | None = None,
        day: str | None = None,
        at: str | None = None,
        minutes: int | None = None,
    ) -> Event:
        """Starts an event on a day the person points at, at nine in the morning for an hour, or at the moment the words name."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if day is not None:
            args["day"] = day
        if at is not None:
            args["at"] = at
        if minutes is not None:
            args["minutes"] = minutes
        return self.action("new_event", **args)

    def new_between(self,
        *,
        start: str,
        end: str,
    ) -> Event:
        """Makes an event of the hours dragged across, from where the drag began to where it ended."""
        return self.action("new_between", start=start, end=end)

    def new_task(self,
        *,
        p: Project | None = None,
        due: str | None = None,
        title: str | None = None,
    ) -> Task:
        """Adds a task to the inbox or to a project."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        if due is not None:
            args["due"] = due
        if title is not None:
            args["title"] = title
        return self.action("new_task", **args)

    def add_step(self,
        *,
        t: Task,
        title: str,
    ) -> Task:
        """Adds a step under a task, in its project, and opens the task if it was done: there is work under it now."""
        return self.action("add_step", t=t, title=title)

    def remove_task(self, *, t: Task) -> None:
        """Deletes a task and the steps under it."""
        return self.action("remove_task", t=t)

    def remove_task_many(self, items: list[RemoveTaskArgs]) -> list[dict[str, Any]]:
        """Runs `remove_task` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_task", items)

    def new_project(self) -> Project:
        """Starts a project."""
        return self.action("new_project")

    def move_project(self,
        *,
        p: Project,
        up: bool,
    ) -> None:
        """Moves a project one place earlier or later among the projects."""
        return self.action("move_project", p=p, up=up)

    def move_project_many(self, items: list[MoveProjectArgs]) -> list[dict[str, Any]]:
        """Runs `move_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_project", items)

    def remove_project(self, *, p: Project) -> None:
        """Deletes a project; its events and tasks go back to none."""
        return self.action("remove_project", p=p)

    def remove_project_many(self, items: list[RemoveProjectArgs]) -> list[dict[str, Any]]:
        """Runs `remove_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_project", items)

    def go_to_day(self, *, at: str) -> None:
        """Stands the window on the day a moment names."""
        return self.action("go_to_day", at=at)

    def go_to_day_many(self, items: list[GoToDayArgs]) -> list[dict[str, Any]]:
        """Runs `go_to_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to_day", items)

    def previous_day(self) -> None:
        """Shows the day before."""
        return self.action("previous_day")

    def previous_day_many(self, items: list[PreviousDayArgs]) -> list[dict[str, Any]]:
        """Runs `previous_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("previous_day", items)

    def next_day(self) -> None:
        """Shows the day after."""
        return self.action("next_day")

    def next_day_many(self, items: list[NextDayArgs]) -> list[dict[str, Any]]:
        """Runs `next_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("next_day", items)

    def this_day(self) -> None:
        """Shows today."""
        return self.action("this_day")

    def this_day_many(self, items: list[ThisDayArgs]) -> list[dict[str, Any]]:
        """Runs `this_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("this_day", items)

    def sync(self) -> None:
        """Asks each account's server for its calendars now, so what it holds comes in as it does when the server is heard from."""
        return self.action("sync")

    def new_calendar(self) -> Calendar:
        """Adds a calendar of the workspace's own."""
        return self.action("new_calendar")

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    def discard_draft(self, *, e: Event) -> None:
        """Throws away an event nothing was written in yet: what a person's New event leaves when they decide against it."""
        return self.action("discard_draft", e=e)

    def previous_week(self) -> None:
        """The window shows the week before."""
        return self.action("previous_week")

    def previous_week_many(self, items: list[PreviousWeekArgs]) -> list[dict[str, Any]]:
        """Runs `previous_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("previous_week", items)

    def next_week(self) -> None:
        """The window shows the week after."""
        return self.action("next_week")

    def next_week_many(self, items: list[NextWeekArgs]) -> list[dict[str, Any]]:
        """Runs `next_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("next_week", items)

    def previous_month(self) -> None:
        """The window shows the month before."""
        return self.action("previous_month")

    def previous_month_many(self, items: list[PreviousMonthArgs]) -> list[dict[str, Any]]:
        """Runs `previous_month` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("previous_month", items)

    def next_month(self) -> None:
        """The window shows the month after."""
        return self.action("next_month")

    def next_month_many(self, items: list[NextMonthArgs]) -> list[dict[str, Any]]:
        """Runs `next_month` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("next_month", items)

    def this_week(self) -> None:
        """The window shows this week."""
        return self.action("this_week")

    def this_week_many(self, items: list[ThisWeekArgs]) -> list[dict[str, Any]]:
        """Runs `this_week` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("this_week", items)

    def busy(self,
        *,
        from_: str,
        to: str,
    ) -> list[Busy]:
        """When the workspace's calendars are taken between two days, both included: every occurrence of each entry that is not called off, as the calendar shows it, in the order they start. What a member, or their agent, reads to find a time that suits; somebody who is no member of the workspace reads nothing."""
        args: dict[str, Any] = {}
        args["from"] = from_
        args["to"] = to
        return self.action("busy", **args)

    def export_calendar(self,
        *,
        e: Event | None = None,
    ) -> Any:
        """The calendar — every event and task it holds, or only one event — as an iCalendar file, for a person to save or an agent to hand on. Nothing is written by this. The system's one Export… lists it, and saves the file where the person says."""
        args: dict[str, Any] = {}
        if e is not None:
            args["e"] = e
        return self.action("export_calendar", **args)

    def take_invitation(self, *, text: str) -> str:
        """Takes an invitation into the calendar: the event it asks for is kept by the account's server where one can be written to it, so the organiser hears the answer, and by this workspace otherwise. What the server took comes back with the account's next sync."""
        return self.action("take_invitation", text=text)

    def import_ics(self, *, file: Any) -> str:
        """Imports the events and tasks of an iCalendar file, and answers how many it took: what a person's own file offers, and what another calendar exported. A line naming a zone this build cannot read is read in the person's zone."""
        return self.action("import_ics", file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_event", "new_between", "new_task", "add_step", "remove_task", "remove_task_many", "new_project", "move_project", "move_project_many", "remove_project", "remove_project_many", "go_to_day", "go_to_day_many", "previous_day", "previous_day_many", "next_day", "next_day_many", "this_day", "this_day_many", "sync", "new_calendar", "discard_draft", "previous_week", "previous_week_many", "next_week", "next_week_many", "previous_month", "previous_month_many", "next_month", "next_month_many", "this_week", "this_week_many", "busy", "export_calendar", "take_invitation", "import_ics"})


class Busy(Resource):
    """Busy — `tl.so.tilo.calendar.Busy`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    @property
    def _76f4x76t(self) -> str:
        """start (`76f4x76t` on the wire)."""
        return self.fields["76f4x76t"]

    @_76f4x76t.setter
    def _76f4x76t(self, value: str) -> None:
        self.edit("76f4x76t", value=value)
    @property
    def _25wuuu2f(self) -> str:
        """end (`25wuuu2f` on the wire)."""
        return self.fields["25wuuu2f"]

    @_25wuuu2f.setter
    def _25wuuu2f(self, value: str) -> None:
        self.edit("25wuuu2f", value=value)
    f5uwvzvm: bool  # all_day


class Moment(Resource):
    """Moment — `tl.so.tilo.calendar.Moment`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"
    kd3noj7y: Event  # event
    ejfozpye: str  # at
    @property
    def _7hfwgky5(self) -> str:
        """day (`7hfwgky5` on the wire)."""
        return self.fields["7hfwgky5"]

    @_7hfwgky5.setter
    def _7hfwgky5(self, value: str) -> None:
        self.edit("7hfwgky5", value=value)


class TiloCalendar(Resource):
    """Calendar — `tl.so.tilo.calendar`."""
    __surface__ = "so.tilo.calendar@698effea1e53ec9b"

