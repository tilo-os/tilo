"""Types for Messages (`so.tilo.messages`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Messages` is the app's `tilo.messages` at `messages/<resource>`, read with
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
    "Conversation",
    "Message",
    "ListedAccount",
    "Endpoint",
    "Person",
    "Projected",
    "Messages",
    "Accounts",
    "Identities",
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

Conversation = TypedDict("Conversation", {
    "with": "str",
    "messages": "list[Entry[Message]]",
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

class Messages(Resource):
    """Messages — `tilo.messages`."""
    __surface__ = "so.tilo.messages@afef5fd05110757d"
    @property
    def conversations(self) -> list[Entry[Conversation]]:
        """Read only."""
        return self.fields["conversations"]

    def deliver_message(self,
        *,
        from_: str,
        body: str,
        to: str | None = None,
        date: str | None = None,
        conversation: str | None = None,
    ) -> Any:
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
    ) -> Any:
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
    ) -> Any:
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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"deliver_message", "send_message", "reply", "list_groups", "read_group_membership", "read_group_history", "read_history", "read_older_history", "search_messages", "export_content"})


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.messages@afef5fd05110757d"
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
    __surface__ = "so.tilo.messages@afef5fd05110757d"
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


