"""Types for Calendar (`so.tilo.calendar`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Calendar` is the app's `calendar.calendar` at `calendar/<resource>`, read with
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
    "Attendee",
    "AttendeeData",
    "Event",
    "EventData",
    "InviteStatus",
    "OccurrenceOverride",
    "OccurrenceOverrideData",
    "Palette",
    "Priority",
    "Project",
    "RemoteAt",
    "RemoteCopy",
    "Removal",
    "Repeat",
    "Task",
    "TaskData",
    "Upload",
    "PersonAccount",
    "Endpoint",
    "Person",
    "Projected",
    "Calendar",
    "Accounts",
    "Identities",
]

class Attendee(TypedDict):
    id: str
    name: str
    email: str
    status: InviteStatus

class AttendeeData(TypedDict):
    id: str
    name: str
    email: str
    status: str

class Event(TypedDict):
    id: str
    title: str
    start: str
    end: str
    end_later_fold: bool
    all_day: bool
    location: str
    color: Palette
    recurrence: Repeat
    recurrence_interval: int
    recurrence_until: str | None
    recurrence_count: int | None
    recurrence_exdates: list[str]
    occurrence_overrides: list[OccurrenceOverride]
    timezone: str
    organizer: str | None
    attendees: list[Attendee]
    reminder_min: int | None
    project: str | None
    calendar_id: str | None

class EventData(TypedDict):
    id: str
    title: str
    start: str
    end: str
    end_later_fold: bool
    all_day: bool
    location: str
    color: int
    rrule: str
    recurrence_interval: int
    recurrence_until: str
    recurrence_count: int
    recurrence_exdates: list[str]
    occurrence_overrides: list[OccurrenceOverrideData]
    timezone: str
    organizer: str
    attendees: list[AttendeeData]
    reminder: int
    project: str
    calendar_id: str

InviteStatus = Literal["needs_action", "accepted", "tentative", "declined"]

class OccurrenceOverride(TypedDict):
    original_date: str
    original_time: str | None
    cancelled: bool
    start: str | None
    end: str | None
    end_later_fold: bool
    title: str | None
    location: str | None

class OccurrenceOverrideData(TypedDict):
    original_date: str
    original_time: str
    cancelled: bool
    start: str
    end: str
    end_later_fold: bool
    title: str
    location: str

Palette = Literal["indigo", "emerald", "amber", "red", "blue", "violet", "slate"]

Priority = Literal["none", "low", "medium", "high"]

class Project(TypedDict):
    id: str
    name: str
    color: Palette
    order: int

class RemoteAt(TypedDict):
    calendar: str
    href: str
    etag: str | None

class RemoteCopy(TypedDict):
    calendar: str
    href: str
    etag: str | None
    holds: str
    task: bool

class Removal(TypedDict):
    item: str
    calendar: str
    href: str
    etag: str | None

Repeat = Literal["none", "daily", "weekdays", "weekly", "monthly", "yearly"]

class Task(TypedDict):
    id: str
    title: str
    done: bool
    due: str | None
    priority: Priority
    project: str | None
    parent: str | None

class TaskData(TypedDict):
    id: str
    title: str
    done: bool
    due: str
    priority: int
    project: str
    parent: str

class Upload(TypedDict):
    event: str
    calendar: str

class PersonAccount(TypedDict):
    type_id: str
    display_name: str
    config: Any
    enabled: bool
    tint: int

class Endpoint(TypedDict):
    service: str
    recipient: str
    scope: str
    revision: str
    removed: bool

class Person(TypedDict):
    label: str
    removed: bool
    phones: list[str]
    emails: list[str]
    photo: str
    endpoints: list[Endpoint]

class Projected(TypedDict):
    person: str
    label: str
    phones: list[str]
    emails: list[str]
    photo: str

class Calendar(Resource):
    """Calendar — `calendar.calendar`."""
    __surface__ = "so.tilo.calendar@0489bf8990ba967c"
    projects: list[Project]
    events: list[Event]
    tasks: list[Task]
    notes: dict[str, str]
    @property
    def remote(self) -> dict[str, RemoteCopy]:
        """Read only."""
        return self.fields["remote"]
    @property
    def sync_tokens(self) -> dict[str, str]:
        """Read only."""
        return self.fields["sync_tokens"]

    def new_event(self, *, event: EventData) -> Run | None:
        """Runs the app's action."""
        return self.action("new-event", event=event)

    def new_task(self, *, task: TaskData) -> Run | None:
        """Runs the app's action."""
        return self.action("new-task", task=task)

    def new_project(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-project", name=name)

    def delete_event(self, *, event: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-event", event=event)

    def reschedule(self,
        *,
        event: str,
        start: str,
        end: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("reschedule", event=event, start=start, end=end)

    def set_start(self,
        *,
        event: str,
        start: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-start", event=event, start=start)

    def move_to_calendar(self,
        *,
        event: str,
        color: Palette,
        calendar: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["event"] = event
        if calendar is not None:
            args["calendar"] = calendar
        args["color"] = color
        return self.action("move-to-calendar", **args)

    def invite(self,
        *,
        event: str,
        email: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("invite", event=event, email=email, name=name)

    def delete_task(self, *, task: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-task", task=task)

    def delete_project(self, *, project: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-project", project=project)

    def import_items(self,
        *,
        events: list[EventData],
        tasks: list[TaskData],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("import-items", events=events, tasks=tasks)

    def adopt_remote_event(self,
        *,
        event: EventData,
        at: RemoteAt,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("adopt-remote-event", event=event, at=at)

    def adopt_remote_task(self,
        *,
        task: TaskData,
        at: RemoteAt,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("adopt-remote-task", task=task, at=at)

    def drop_remote_item(self, *, item: str) -> Run | None:
        """Runs the app's action."""
        return self.action("drop-remote-item", item=item)

    def server_applied(self,
        *,
        event: str,
        at: RemoteAt,
        holds: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("server-applied", event=event, at=at, holds=holds)

    def server_removed(self, *, item: str) -> Run | None:
        """Runs the app's action."""
        return self.action("server-removed", item=item)

    def pulled(self,
        *,
        calendar: str,
        token: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("pulled", calendar=calendar, token=token)

    def export_ics_content(self,
        *,
        event: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if event is not None:
            args["event"] = event
        return self.action("export-ics-content", **args)


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.calendar@0489bf8990ba967c"
    accounts: list[PersonAccount]
    active: dict[str, str]

    def publish_browser_identity(self,
        *,
        root: str,
        origin: str,
        display_name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("publish-browser-identity", root=root, origin=origin, display_name=display_name)

    def save_account(self,
        *,
        id: str,
        type_id: str,
        display_name: str,
        config: Any,
        enabled: bool,
        tint: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("save-account", id=id, type_id=type_id, display_name=display_name, config=config, enabled=enabled, tint=tint)

    def remove_account(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-account", account=account)

    def set_active_account(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("set-active-account", account=account)


class Identities(Resource):
    """Contact identities — `tilo.contacts.identities`."""
    __surface__ = "so.tilo.calendar@0489bf8990ba967c"
    people: list[Person]

    def project_contact(self,
        *,
        person: str,
        label: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("project-contact", person=person, label=label)

    def remove_contact(self, *, person: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-contact", person=person)

    def confirm_endpoint(self,
        *,
        person: str,
        endpoint: str,
        service: str,
        recipient: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("confirm-endpoint", person=person, endpoint=endpoint, service=service, recipient=recipient)

    def confirm_account(self,
        *,
        person: str,
        endpoint: str,
        service: str,
        scope: str,
        recipient: str,
        revision: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("confirm-account", person=person, endpoint=endpoint, service=service, scope=scope, recipient=recipient, revision=revision)

    def replace_endpoint(self,
        *,
        person: str,
        previous: str,
        endpoint: str,
        service: str,
        recipient: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("replace-endpoint", person=person, previous=previous, endpoint=endpoint, service=service, recipient=recipient)

    def replace_account(self,
        *,
        person: str,
        previous: str,
        endpoint: str,
        service: str,
        scope: str,
        recipient: str,
        revision: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("replace-account", person=person, previous=previous, endpoint=endpoint, service=service, scope=scope, recipient=recipient, revision=revision)

    def project_phones(self,
        *,
        person: str,
        phones: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("project-phones", person=person, phones=phones)

    def project_emails(self,
        *,
        person: str,
        emails: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("project-emails", person=person, emails=emails)

    def project_contacts(self,
        *,
        people: list[Projected],
        removed: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("project-contacts", people=people, removed=removed)

    def forget_endpoint(self,
        *,
        person: str,
        endpoint: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("forget-endpoint", person=person, endpoint=endpoint)


