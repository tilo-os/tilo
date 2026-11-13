"""Types for Messages (`so.tilo.messages`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Messages` is the app's `tilo.messages` at `messages/<resource>`, read with
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
    "Conversation",
    "Message",
    "PersonAccount",
    "Endpoint",
    "Person",
    "Projected",
    "Messages",
    "Accounts",
    "Identities",
    "Drafts",
]

Conversation = TypedDict("Conversation", {
    "with": "str",
    "messages": "list[Message]",
})

Message = TypedDict("Message", {
    "from": "str",
    "to": "str",
    "body": "str",
    "date": "str",
    "at_ms": "int",
    "outgoing": "bool",
    "queued": "bool",
})

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

class Messages(Resource):
    """Messages — `tilo.messages`."""
    __surface__ = "so.tilo.messages@23bf6eb5fe40bef0"
    @property
    def conversations(self) -> list[Conversation]:
        """Read only."""
        return self.fields["conversations"]

    def deliver_message(self,
        *,
        from_: str,
        body: str,
        to: str | None = None,
        date: str | None = None,
        conversation: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["from"] = from_
        if to is not None:
            args["to"] = to
        args["body"] = body
        if date is not None:
            args["date"] = date
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("deliver-message", **args)

    def send_message(self,
        *,
        to: str,
        body: str,
        conversation: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["body"] = body
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("send-message", **args)

    def reply(self,
        *,
        conversation: str,
        body: str,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("reply", conversation=conversation, body=body)

    def list_groups(self,
        *,
        service: str,
        account: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("list-groups", service=service, account=account)

    def read_group_membership(self,
        *,
        service: str,
        account: str,
        group: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("read-group-membership", service=service, account=account, group=group)

    def read_group_history(self,
        *,
        service: str,
        account: str,
        group: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("read-group-history", service=service, account=account, group=group)

    def read_history(self, *, choice: str) -> None:
        """Runs the app's action."""
        return self.action("read-history", choice=choice)

    def read_older_history(self, *, choice: str) -> None:
        """Runs the app's action."""
        return self.action("read-older-history", choice=choice)

    def search_messages(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("search-messages", query=query)

    def export_content(self,
        *,
        format: str | None = None,
        conversation: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("export-content", **args)


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.messages@23bf6eb5fe40bef0"
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
    __surface__ = "so.tilo.messages@23bf6eb5fe40bef0"
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


class Drafts(Resource):
    """Message drafts — `messages.drafts`."""
    __surface__ = "so.tilo.messages@23bf6eb5fe40bef0"
    drafts: dict[str, str]

    def keep_draft(self,
        *,
        conversation: str,
        body: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("keep-draft", conversation=conversation, body=body)

    def drop_draft(self, *, conversation: str) -> Run | None:
        """Runs the app's action."""
        return self.action("drop-draft", conversation=conversation)


