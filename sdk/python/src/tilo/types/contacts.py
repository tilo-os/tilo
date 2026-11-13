"""Types for Contacts (`so.tilo.contacts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Book` is the app's `contacts.book` at `contacts/<resource>`, read with
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
    phones: list[Labeled]
    emails: list[Labeled]
    addresses: list[Address]
    groups: list[GroupEntry]
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
    endpoints: list[Endpoint]

class Projected(TypedDict):
    person: str
    label: str
    phones: list[str]
    emails: list[str]
    photo: str

class Book(Resource):
    """Contacts — `contacts.book`."""
    __surface__ = "so.tilo.contacts@862e8f23951f9266"
    contacts: list[Contact]
    @property
    def removed(self) -> list[str]:
        """Read only."""
        return self.fields["removed"]

    def new_contact(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-contact", name=name)

    def add_contacts(self, *, contacts: list[Contact]) -> Run | None:
        """Runs the app's action."""
        return self.action("add-contacts", contacts=contacts)

    def delete_contact(self, *, contact: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-contact", contact=contact)

    def merge_contacts(self,
        *,
        contact: str,
        source: str,
        take_from_source: list[MergeField],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("merge-contacts", contact=contact, source=source, take_from_source=take_from_source)

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


class Identities(Resource):
    """Contact identities — `tilo.contacts.identities`."""
    __surface__ = "so.tilo.contacts@862e8f23951f9266"
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


