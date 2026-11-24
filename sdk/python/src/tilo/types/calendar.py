"""Types for Calendar (`so.tilo.calendar`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Calendar` is the app's `calendar.calendar` at `calendar/<resource>`, read with
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
    "ListedAccount",
    "Endpoint",
    "Person",
    "Projected",
    "Calendar",
    "Accounts",
    "Identities",
    "NewEventArgs",
    "PublishBrowserIdentityArgs",
    "SaveAccountArgs",
    "RemoveAccountArgs",
    "ProjectContactArgs",
    "RemoveContactArgs",
    "ConfirmEndpointArgs",
    "ConfirmAccountArgs",
    "ReplaceEndpointArgs",
    "ReplaceAccountArgs",
    "ProjectPhonesArgs",
    "ProjectEmailsArgs",
    "ProjectContactsArgs",
    "ForgetEndpointArgs",
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
    occurrence_overrides: list[Entry[OccurrenceOverride]]
    timezone: str
    organizer: str | None
    attendees: list[Entry[Attendee]]
    reminder_min: int | None
    project: str | None
    calendar_id: str | None
    categories: list[str]

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
    categories: list[str]

InviteStatus = Literal["NeedsAction", "Accepted", "Tentative", "Declined"]

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

Priority = Literal["None", "Low", "Medium", "High"]

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

Repeat = Literal["None", "Daily", "Weekdays", "Weekly", "Monthly", "Yearly"]

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

class ListedAccount(TypedDict):
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
    endpoints: list[Entry[Endpoint]]

class Projected(TypedDict):
    person: str
    label: str
    phones: list[str]
    emails: list[str]
    photo: str

NewEventArgs = TypedDict("NewEventArgs", {
    "event": "EventData",
})
"""The arguments of `new-event`."""

PublishBrowserIdentityArgs = TypedDict("PublishBrowserIdentityArgs", {
    "root": "str",
    "origin": "str",
    "display_name": "str",
})
"""The arguments of `publish-browser-identity`."""

SaveAccountArgs = TypedDict("SaveAccountArgs", {
    "id": "str",
    "type_id": "str",
    "display_name": "str",
    "config": "Any",
    "enabled": "bool",
    "tint": "int",
})
"""The arguments of `save-account`."""

RemoveAccountArgs = TypedDict("RemoveAccountArgs", {
    "account": "str",
})
"""The arguments of `remove-account`."""

ProjectContactArgs = TypedDict("ProjectContactArgs", {
    "person": "str",
    "label": "str",
})
"""The arguments of `project-contact`."""

RemoveContactArgs = TypedDict("RemoveContactArgs", {
    "person": "str",
})
"""The arguments of `remove-contact`."""

ConfirmEndpointArgs = TypedDict("ConfirmEndpointArgs", {
    "person": "str",
    "endpoint": "str",
    "service": "str",
    "recipient": "str",
})
"""The arguments of `confirm-endpoint`."""

ConfirmAccountArgs = TypedDict("ConfirmAccountArgs", {
    "person": "str",
    "endpoint": "str",
    "service": "str",
    "scope": "str",
    "recipient": "str",
    "revision": "str",
})
"""The arguments of `confirm-account`."""

ReplaceEndpointArgs = TypedDict("ReplaceEndpointArgs", {
    "person": "str",
    "previous": "str",
    "endpoint": "str",
    "service": "str",
    "recipient": "str",
})
"""The arguments of `replace-endpoint`."""

ReplaceAccountArgs = TypedDict("ReplaceAccountArgs", {
    "person": "str",
    "previous": "str",
    "endpoint": "str",
    "service": "str",
    "scope": "str",
    "recipient": "str",
    "revision": "str",
})
"""The arguments of `replace-account`."""

ProjectPhonesArgs = TypedDict("ProjectPhonesArgs", {
    "person": "str",
    "phones": "list[str]",
})
"""The arguments of `project-phones`."""

ProjectEmailsArgs = TypedDict("ProjectEmailsArgs", {
    "person": "str",
    "emails": "list[str]",
})
"""The arguments of `project-emails`."""

ProjectContactsArgs = TypedDict("ProjectContactsArgs", {
    "people": "list[Projected]",
    "removed": "list[str]",
})
"""The arguments of `project-contacts`."""

ForgetEndpointArgs = TypedDict("ForgetEndpointArgs", {
    "person": "str",
    "endpoint": "str",
})
"""The arguments of `forget-endpoint`."""

class Calendar(Resource):
    """Calendar — `calendar.calendar`."""
    __surface__ = "so.tilo.calendar@c920d7ea26877f48"
    projects: list[Entry[Project]]
    events: list[Entry[Event]]
    tasks: list[Entry[Task]]
    notes: dict[str, str]
    @property
    def remote(self) -> dict[str, RemoteCopy]:
        """Read only."""
        return self.fields["remote"]
    @property
    def sync_tokens(self) -> dict[str, str]:
        """Read only."""
        return self.fields["sync_tokens"]

    def new_event(self, *, event: EventData) -> None:
        """Runs the app's action."""
        return self.action("new-event", event=event)

    def new_event_many(self, items: list[NewEventArgs]) -> list[dict[str, Any]]:
        """Runs `new-event` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-event", items)

    def new_task(self, *, task: TaskData) -> None:
        """Runs the app's action."""
        return self.action("new-task", task=task)

    def new_project(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-project", name=name)

    def delete_event(self, *, event: str) -> None:
        """Runs the app's action."""
        return self.action("delete-event", event=event)

    def reschedule(self,
        *,
        event: str,
        start: str,
        end: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("reschedule", event=event, start=start, end=end)

    def set_start(self,
        *,
        event: str,
        start: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-start", event=event, start=start)

    def move_to_calendar(self,
        *,
        event: str,
        color: Palette,
        calendar: str | None = None,
    ) -> None:
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
    ) -> None:
        """Runs the app's action."""
        return self.action("invite", event=event, email=email, name=name)

    def set_categories(self,
        *,
        event: str,
        categories: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("set-categories", event=event, categories=categories)

    def add_invitation(self, *, ics: str) -> Any:
        """Runs the app's action."""
        return self.action("add-invitation", ics=ics)

    def delete_task(self, *, task: str) -> None:
        """Runs the app's action."""
        return self.action("delete-task", task=task)

    def delete_project(self, *, project: str) -> None:
        """Runs the app's action."""
        return self.action("delete-project", project=project)

    def import_items(self,
        *,
        events: list[EventData],
        tasks: list[TaskData],
    ) -> None:
        """Runs the app's action."""
        return self.action("import-items", events=events, tasks=tasks)

    def adopt_remote_event(self,
        *,
        event: EventData,
        at: RemoteAt,
    ) -> None:
        """Runs the app's action."""
        return self.action("adopt-remote-event", event=event, at=at)

    def adopt_remote_task(self,
        *,
        task: TaskData,
        at: RemoteAt,
    ) -> None:
        """Runs the app's action."""
        return self.action("adopt-remote-task", task=task, at=at)

    def drop_remote_item(self, *, item: str) -> None:
        """Runs the app's action."""
        return self.action("drop-remote-item", item=item)

    def server_applied(self,
        *,
        event: str,
        at: RemoteAt,
        holds: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("server-applied", event=event, at=at, holds=holds)

    def server_removed(self, *, item: str) -> None:
        """Runs the app's action."""
        return self.action("server-removed", item=item)

    def pulled(self,
        *,
        calendar: str,
        token: str,
    ) -> None:
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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_event", "new_event_many", "new_task", "new_project", "delete_event", "reschedule", "set_start", "move_to_calendar", "invite", "set_categories", "add_invitation", "delete_task", "delete_project", "import_items", "adopt_remote_event", "adopt_remote_task", "drop_remote_item", "server_applied", "server_removed", "pulled", "export_ics_content"})


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.calendar@c920d7ea26877f48"
    accounts: list[Entry[ListedAccount]]

    def publish_browser_identity(self,
        *,
        root: str,
        origin: str,
        display_name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("publish-browser-identity", root=root, origin=origin, display_name=display_name)

    def publish_browser_identity_many(self, items: list[PublishBrowserIdentityArgs]) -> list[dict[str, Any]]:
        """Runs `publish-browser-identity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("publish-browser-identity", items)

    def save_account(self,
        *,
        id: str,
        type_id: str,
        display_name: str,
        config: Any,
        enabled: bool,
        tint: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("save-account", id=id, type_id=type_id, display_name=display_name, config=config, enabled=enabled, tint=tint)

    def save_account_many(self, items: list[SaveAccountArgs]) -> list[dict[str, Any]]:
        """Runs `save-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save-account", items)

    def remove_account(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("remove-account", account=account)

    def remove_account_many(self, items: list[RemoveAccountArgs]) -> list[dict[str, Any]]:
        """Runs `remove-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-account", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"publish_browser_identity", "publish_browser_identity_many", "save_account", "save_account_many", "remove_account", "remove_account_many"})


class Identities(Resource):
    """Contact identities — `tilo.contacts.identities`."""
    __surface__ = "so.tilo.calendar@c920d7ea26877f48"
    people: list[Entry[Person]]

    def project_contact(self,
        *,
        person: str,
        label: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("project-contact", person=person, label=label)

    def project_contact_many(self, items: list[ProjectContactArgs]) -> list[dict[str, Any]]:
        """Runs `project-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("project-contact", items)

    def remove_contact(self, *, person: str) -> None:
        """Runs the app's action."""
        return self.action("remove-contact", person=person)

    def remove_contact_many(self, items: list[RemoveContactArgs]) -> list[dict[str, Any]]:
        """Runs `remove-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-contact", items)

    def confirm_endpoint(self,
        *,
        person: str,
        endpoint: str,
        service: str,
        recipient: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("confirm-endpoint", person=person, endpoint=endpoint, service=service, recipient=recipient)

    def confirm_endpoint_many(self, items: list[ConfirmEndpointArgs]) -> list[dict[str, Any]]:
        """Runs `confirm-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("confirm-endpoint", items)

    def confirm_account(self,
        *,
        person: str,
        endpoint: str,
        service: str,
        scope: str,
        recipient: str,
        revision: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("confirm-account", person=person, endpoint=endpoint, service=service, scope=scope, recipient=recipient, revision=revision)

    def confirm_account_many(self, items: list[ConfirmAccountArgs]) -> list[dict[str, Any]]:
        """Runs `confirm-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("confirm-account", items)

    def replace_endpoint(self,
        *,
        person: str,
        previous: str,
        endpoint: str,
        service: str,
        recipient: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("replace-endpoint", person=person, previous=previous, endpoint=endpoint, service=service, recipient=recipient)

    def replace_endpoint_many(self, items: list[ReplaceEndpointArgs]) -> list[dict[str, Any]]:
        """Runs `replace-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-endpoint", items)

    def replace_account(self,
        *,
        person: str,
        previous: str,
        endpoint: str,
        service: str,
        scope: str,
        recipient: str,
        revision: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("replace-account", person=person, previous=previous, endpoint=endpoint, service=service, scope=scope, recipient=recipient, revision=revision)

    def replace_account_many(self, items: list[ReplaceAccountArgs]) -> list[dict[str, Any]]:
        """Runs `replace-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-account", items)

    def project_phones(self,
        *,
        person: str,
        phones: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("project-phones", person=person, phones=phones)

    def project_phones_many(self, items: list[ProjectPhonesArgs]) -> list[dict[str, Any]]:
        """Runs `project-phones` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("project-phones", items)

    def project_emails(self,
        *,
        person: str,
        emails: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("project-emails", person=person, emails=emails)

    def project_emails_many(self, items: list[ProjectEmailsArgs]) -> list[dict[str, Any]]:
        """Runs `project-emails` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("project-emails", items)

    def project_contacts(self,
        *,
        people: list[Projected],
        removed: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("project-contacts", people=people, removed=removed)

    def project_contacts_many(self, items: list[ProjectContactsArgs]) -> list[dict[str, Any]]:
        """Runs `project-contacts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("project-contacts", items)

    def forget_endpoint(self,
        *,
        person: str,
        endpoint: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("forget-endpoint", person=person, endpoint=endpoint)

    def forget_endpoint_many(self, items: list[ForgetEndpointArgs]) -> list[dict[str, Any]]:
        """Runs `forget-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget-endpoint", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"project_contact", "project_contact_many", "remove_contact", "remove_contact_many", "confirm_endpoint", "confirm_endpoint_many", "confirm_account", "confirm_account_many", "replace_endpoint", "replace_endpoint_many", "replace_account", "replace_account_many", "project_phones", "project_phones_many", "project_emails", "project_emails_many", "project_contacts", "project_contacts_many", "forget_endpoint", "forget_endpoint_many"})


