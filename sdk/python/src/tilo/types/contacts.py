"""Types for Contacts (`so.tilo.contacts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Book` is the app's `contacts.book` at `contacts/<resource>`, read with
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
    "Address",
    "Contact",
    "GroupEntry",
    "Labeled",
    "MergeField",
    "Endpoint",
    "Person",
    "Projected",
    "Book",
    "Identities",
    "NewContactArgs",
    "AddContactsArgs",
    "DeleteContactArgs",
    "MergeContactsArgs",
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

class Address(TypedDict):
    id: str
    label: str
    street: str
    city: str
    region: str
    postal: str
    country: str

class Contact(TypedDict):
    id: str
    first: str
    last: str
    org: str
    title: str
    website: str
    birthday: Any | None
    notes: str
    phones: list[Entry[Labeled]]
    emails: list[Entry[Labeled]]
    addresses: list[Entry[Address]]
    groups: list[Entry[GroupEntry]]
    favorite: bool
    photo: str

class GroupEntry(TypedDict):
    id: str
    value: str

class Labeled(TypedDict):
    id: str
    label: str
    value: str

MergeField = Literal["First", "Last", "Organization", "Title", "Website", "Birthday", "Notes"]

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

NewContactArgs = TypedDict("NewContactArgs", {
    "name": "str",
})
"""The arguments of `new-contact`."""

AddContactsArgs = TypedDict("AddContactsArgs", {
    "contacts": "list[Contact]",
})
"""The arguments of `add-contacts`."""

DeleteContactArgs = TypedDict("DeleteContactArgs", {
    "contact": "str",
})
"""The arguments of `delete-contact`."""

MergeContactsArgs = TypedDict("MergeContactsArgs", {
    "contact": "str",
    "source": "str",
    "take_from_source": "list[MergeField]",
})
"""The arguments of `merge-contacts`."""

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

class Book(Resource):
    """Contacts — `contacts.book`."""
    __surface__ = "so.tilo.contacts@df7802e5d648a10e"
    contacts: list[Entry[Contact]]
    @property
    def removed(self) -> list[str]:
        """Read only."""
        return self.fields["removed"]

    def new_contact(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-contact", name=name)

    def new_contact_many(self, items: list[NewContactArgs]) -> list[dict[str, Any]]:
        """Runs `new-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-contact", items)

    def add_contacts(self, *, contacts: list[Contact]) -> None:
        """Runs the app's action."""
        return self.action("add-contacts", contacts=contacts)

    def add_contacts_many(self, items: list[AddContactsArgs]) -> list[dict[str, Any]]:
        """Runs `add-contacts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-contacts", items)

    def delete_contact(self, *, contact: str) -> None:
        """Runs the app's action."""
        return self.action("delete-contact", contact=contact)

    def delete_contact_many(self, items: list[DeleteContactArgs]) -> list[dict[str, Any]]:
        """Runs `delete-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-contact", items)

    def merge_contacts(self,
        *,
        contact: str,
        source: str,
        take_from_source: list[MergeField],
    ) -> None:
        """Runs the app's action."""
        return self.action("merge-contacts", contact=contact, source=source, take_from_source=take_from_source)

    def merge_contacts_many(self, items: list[MergeContactsArgs]) -> list[dict[str, Any]]:
        """Runs `merge-contacts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("merge-contacts", items)

    def export_vcard_content(self,
        *,
        contact: str | None = None,
        name: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if contact is not None:
            args["contact"] = contact
        if name is not None:
            args["name"] = name
        return self.action("export-vcard-content", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_contact", "new_contact_many", "add_contacts", "add_contacts_many", "delete_contact", "delete_contact_many", "merge_contacts", "merge_contacts_many", "export_vcard_content"})


class Identities(Resource):
    """Contact identities — `tilo.contacts.identities`."""
    __surface__ = "so.tilo.contacts@df7802e5d648a10e"
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


