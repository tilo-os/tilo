"""Types for Contacts (`so.tilo.contacts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Labeled` is the app's `tl.so.tilo.contacts.Labeled` at `contacts/<resource>`, read with
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
    "MergeField",
    "Labeled",
    "Address",
    "Contact",
    "Group",
    "Book",
    "Contacts",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "RenameArgs",
    "FavoriteArgs",
    "RemovePhoneArgs",
    "RemoveEmailArgs",
    "RemoveAddressArgs",
    "UngroupArgs",
    "GroupArgs",
    "DeleteArgs",
    "MergeIntoArgs",
    "ConfirmArgs",
    "ChangeArgs",
    "ForgetArgs",
    "ShareArgs",
    "ShareCardArgs",
    "UnshareArgs",
]

MergeField = Literal["name", "org", "role", "website", "birthday", "notes"]
"""What a merge may take from the card that goes: the values each card keeps one of. Numbers, mailboxes, addresses and groups always come over; these are the person's to choose."""

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

RenameArgs = TypedDict("RenameArgs", {
    "name": "str",
})
"""The arguments of `rename`."""

FavoriteArgs = TypedDict("FavoriteArgs", {
    "on": NotRequired["bool | None"],
})
"""The arguments of `favorite`."""

RemovePhoneArgs = TypedDict("RemovePhoneArgs", {
    "phone": "Labeled",
})
"""The arguments of `remove_phone`."""

RemoveEmailArgs = TypedDict("RemoveEmailArgs", {
    "mail": "Labeled",
})
"""The arguments of `remove_email`."""

RemoveAddressArgs = TypedDict("RemoveAddressArgs", {
    "address": "Address",
})
"""The arguments of `remove_address`."""

UngroupArgs = TypedDict("UngroupArgs", {
    "name": "str",
})
"""The arguments of `ungroup`."""

GroupArgs = TypedDict("GroupArgs", {
    "c": "Contact",
    "name": "str",
})
"""The arguments of `group`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "c": "Contact",
    "directory": NotRequired["Any | None"],
})
"""The arguments of `delete`."""

MergeIntoArgs = TypedDict("MergeIntoArgs", {
    "keep": "Contact",
    "goes": "Contact",
    "take": NotRequired["list[MergeField] | None"],
    "directory": NotRequired["Any | None"],
})
"""The arguments of `merge_into`."""

ConfirmArgs = TypedDict("ConfirmArgs", {
    "d": "Any",
    "person": "Any",
    "service": "str",
    "scope": "str",
    "account": "str",
})
"""The arguments of `confirm`."""

ChangeArgs = TypedDict("ChangeArgs", {
    "d": "Any",
    "person": "Any",
    "endpoint": "Any",
    "service": "str",
    "scope": "str",
    "account": "str",
})
"""The arguments of `change`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "d": "Any",
    "person": "Any",
    "endpoint": "Any",
})
"""The arguments of `forget`."""

ShareArgs = TypedDict("ShareArgs", {
    "d": "Any",
    "person": "Any",
    "c": "Contact",
})
"""The arguments of `share`."""

ShareCardArgs = TypedDict("ShareCardArgs", {
    "d": "Any",
    "c": "Contact",
})
"""The arguments of `share_card`."""

UnshareArgs = TypedDict("UnshareArgs", {
    "d": "Any",
    "person": "Any",
    "c": "Contact",
})
"""The arguments of `unshare`."""

class Labeled(Resource):
    """Labeled — `tl.so.tilo.contacts.Labeled`."""
    __surface__ = "so.tilo.contacts@1b03874977f50857"
    fzyd2a52: str  # label
    b4cilqkx: str  # value


class Address(Resource):
    """Address — `tl.so.tilo.contacts.Address`."""
    __surface__ = "so.tilo.contacts@1b03874977f50857"
    @property
    def _5iipld72(self) -> str:
        """label (`5iipld72` on the wire)."""
        return self.fields["5iipld72"]

    @_5iipld72.setter
    def _5iipld72(self, value: str) -> None:
        self.edit("5iipld72", value=value)
    zpxsouby: str  # street
    txsphfy2: str  # city
    g55zwvsh: str  # region
    xuayok2l: str  # postal
    xlbwdlve: str  # country


class Contact(Resource):
    """Contact — `tl.so.tilo.contacts.Contact`."""
    __surface__ = "so.tilo.contacts@1b03874977f50857"
    zut4kvim: str  # name
    rfhxkccf: str  # org
    @property
    def _7wq5grnt(self) -> str:
        """role (`7wq5grnt` on the wire)."""
        return self.fields["7wq5grnt"]

    @_7wq5grnt.setter
    def _7wq5grnt(self, value: str) -> None:
        self.edit("7wq5grnt", value=value)
    dg7b4wfh: str  # website
    fc62qk5v: str | None  # birthday
    @property
    def _4lfazbdq(self) -> str:
        """notes (`4lfazbdq` on the wire)."""
        return self.fields["4lfazbdq"]

    @_4lfazbdq.setter
    def _4lfazbdq(self, value: str) -> None:
        self.edit("4lfazbdq", value=value)
    lurugxra: list[Entry[Labeled]]  # phones
    hfbs4gxe: list[Entry[Labeled]]  # emails
    orxuftuv: list[Entry[Address]]  # addresses
    vea52w55: list[str]  # groups
    ur3qshor: bool  # favorite
    kvhikhu4: str  # photo
    szrijd7k: str  # person
    q3um3t7j: str  # uid

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

    def rename(self, *, name: str) -> None:
        """Renames a contact."""
        return self.action("rename", name=name)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def favorite(self,
        *,
        on: bool | None = None,
    ) -> None:
        """Adds a contact to favorites, or takes them off."""
        args: dict[str, Any] = {}
        if on is not None:
            args["on"] = on
        return self.action("favorite", **args)

    def favorite_many(self, items: list[FavoriteArgs]) -> list[dict[str, Any]]:
        """Runs `favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("favorite", items)

    def add_phone(self,
        *,
        number: str | None = None,
        label: str | None = None,
    ) -> Labeled:
        """Gives a contact a phone number, as the number `number` and the kind `label` where the caller knows them already. Addressed from the card's own button, both are empty for the person to fill in."""
        args: dict[str, Any] = {}
        if number is not None:
            args["number"] = number
        if label is not None:
            args["label"] = label
        return self.action("add_phone", **args)

    def remove_phone(self, *, phone: Labeled) -> None:
        """Takes one of a contact's phone numbers away."""
        return self.action("remove_phone", phone=phone)

    def remove_phone_many(self, items: list[RemovePhoneArgs]) -> list[dict[str, Any]]:
        """Runs `remove_phone` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_phone", items)

    def add_email(self,
        *,
        address: str | None = None,
        label: str | None = None,
    ) -> Labeled:
        """Gives a contact a mailbox, as the `address` and the kind `label` where the caller knows them already. Addressed from the card's own button, both are empty for the person to fill in."""
        args: dict[str, Any] = {}
        if address is not None:
            args["address"] = address
        if label is not None:
            args["label"] = label
        return self.action("add_email", **args)

    def remove_email(self, *, mail: Labeled) -> None:
        """Takes one of a contact's mailboxes away."""
        return self.action("remove_email", mail=mail)

    def remove_email_many(self, items: list[RemoveEmailArgs]) -> list[dict[str, Any]]:
        """Runs `remove_email` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_email", items)

    def add_address(self) -> Address:
        """Gives a contact a postal address to fill in."""
        return self.action("add_address")

    def remove_address(self, *, address: Address) -> None:
        """Takes one of a contact's addresses away."""
        return self.action("remove_address", address=address)

    def remove_address_many(self, items: list[RemoveAddressArgs]) -> list[dict[str, Any]]:
        """Runs `remove_address` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_address", items)

    def ungroup(self, *, name: str) -> None:
        """Takes a contact out of a group."""
        return self.action("ungroup", name=name)

    def ungroup_many(self, items: list[UngroupArgs]) -> list[dict[str, Any]]:
        """Runs `ungroup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ungroup", items)

    def export_contact(self) -> Any:
        """A contact as a file: their card, named for them."""
        return self.action("export_contact")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "rename", "rename_many", "favorite", "favorite_many", "add_phone", "remove_phone", "remove_phone_many", "add_email", "remove_email", "remove_email_many", "add_address", "remove_address", "remove_address_many", "ungroup", "ungroup_many", "export_contact"})


class Group(Resource):
    """Group — `tl.so.tilo.contacts.Group`."""
    __surface__ = "so.tilo.contacts@1b03874977f50857"
    jn6l7z6v: str  # name


class Book(Resource):
    """Book — `tl.so.tilo.contacts.Book`."""
    __surface__ = "so.tilo.contacts@1b03874977f50857"
    qfj7l6eu: list[Entry[Contact]]  # contacts
    uyyz5pg2: list[Entry[Group]]  # groups

    def new_contact(self,
        *,
        name: str | None = None,
    ) -> Contact:
        """A new contact, named by what is typed where they were made; a book's entry gives the name it takes, so nothing is left called 'No name yet' by a person who typed one."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_contact", **args)

    def named_or_new(self, *, name: str) -> Contact:
        """Finds the contact the workspace keeps under `name`, or adds one: what brings someone in from another service — a task given to them before they joined — to one card, however many imports name them."""
        return self.action("named_or_new", name=name)

    def group(self,
        *,
        c: Contact,
        name: str,
    ) -> None:
        """Puts a contact in a group, making the group the book's place if it is new: the same group however it is spelled, so 'Work' and 'work' are one place."""
        return self.action("group", c=c, name=name)

    def group_many(self, items: list[GroupArgs]) -> list[dict[str, Any]]:
        """Runs `group` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("group", items)

    def new_group(self, *, name: str) -> Group:
        """Starts a group the book offers as a place, with nobody in it yet."""
        return self.action("new_group", name=name)

    def delete(self,
        *,
        c: Contact,
        directory: Any | None = None,
    ) -> None:
        """Deletes a contact from the book, and with it the person the workspace's apps know of it where the window hands the directory: what Mail, Calendar and Messages reach it by goes too."""
        args: dict[str, Any] = {}
        args["c"] = c
        if directory is not None:
            args["directory"] = directory
        return self.action("delete", **args)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def merge_into(self,
        *,
        keep: Contact,
        goes: Contact,
        take: list[MergeField] | None = None,
        directory: Any | None = None,
    ) -> None:
        """Merges the card that goes into the one that stays: every number, mailbox, address and group of both is the card that stays, the fields `take` names come from the card that goes, and the card that goes is deleted. One change, so undo brings both cards back."""
        args: dict[str, Any] = {}
        args["keep"] = keep
        args["goes"] = goes
        if take is not None:
            args["take"] = take
        if directory is not None:
            args["directory"] = directory
        return self.action("merge_into", **args)

    def merge_into_many(self, items: list[MergeIntoArgs]) -> list[dict[str, Any]]:
        """Runs `merge_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("merge_into", items)

    def export_book(self) -> Any:
        """Every contact as one vCard file, which another address book reads whole."""
        return self.action("export_book")

    def import_vcard(self, *, file: Any) -> int:
        """Imports the contacts a vCard file holds into the book, each card a contact of its own, and answers how many were added. A file whose cards cannot be read adds nothing and changes nothing: what it says is the refusal."""
        return self.action("import_vcard", file=file)

    def add_contacts(self, *, cards: str) -> int:
        """The cards `cards`, the words of a vCard file, as contacts of this book, and how many named somebody: what an importer, an agent or a script hands a book in one call."""
        return self.action("add_contacts", cards=cards)

    def add_card(self, *, card: str) -> Contact | None:
        """The contact a vCard card says, made in the book, or none for a card that names nobody. Its rows are filled after it stands in the book, as every resource's are."""
        return self.action("add_card", card=card)

    def find_for_address(self, *, address: str) -> Contact | None:
        """The contact the book holds for `address`, as an action: what a person, an agent or another app asks before they write to it. Nothing is changed and nothing is guessed from a name."""
        return self.action("find_for_address", address=address)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_contact", "named_or_new", "group", "group_many", "new_group", "delete", "delete_many", "merge_into", "merge_into_many", "export_book", "import_vcard", "add_contacts", "add_card", "find_for_address"})


class Contacts(Resource):
    """Contacts — `tl.so.tilo.contacts`."""
    __surface__ = "so.tilo.contacts@1b03874977f50857"
    def confirm(self,
        *,
        d: Any,
        person: Any,
        service: str,
        scope: str,
        account: str,
    ) -> None:
        """Binds `account` of `service` to the person: the directory's `confirm-endpoint`, or `confirm-account` where the service names an account scope."""
        return self.action("confirm", d=d, person=person, service=service, scope=scope, account=account)

    def confirm_many(self, items: list[ConfirmArgs]) -> list[dict[str, Any]]:
        """Runs `confirm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("confirm", items)

    def change(self,
        *,
        d: Any,
        person: Any,
        endpoint: Any,
        service: str,
        scope: str,
        account: str,
    ) -> None:
        """Changes the account an association names, under the directory's rule that a changed address needs review before anything is sent to it."""
        return self.action("change", d=d, person=person, endpoint=endpoint, service=service, scope=scope, account=account)

    def change_many(self, items: list[ChangeArgs]) -> list[dict[str, Any]]:
        """Runs `change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("change", items)

    def forget(self,
        *,
        d: Any,
        person: Any,
        endpoint: Any,
    ) -> None:
        """Takes an association away: messages from that account stop reaching this contact, and what was already received is unchanged."""
        return self.action("forget", d=d, person=person, endpoint=endpoint)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    def share(self,
        *,
        d: Any,
        person: Any,
        c: Contact,
    ) -> None:
        """Shares this card with the workspace's apps, under the person it is: the name, the numbers and the mailboxes another app reads of a contact before it writes to them. The card keeps that person's id, so it stops being shared, or is shared again, as the same person."""
        return self.action("share", d=d, person=person, c=c)

    def share_many(self, items: list[ShareArgs]) -> list[dict[str, Any]]:
        """Runs `share` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("share", items)

    def share_card(self,
        *,
        d: Any,
        c: Contact,
    ) -> None:
        """Shares a card the workspace's apps do not know yet: a person of its own is made for it, so Mail, Calendar and Messages can name it and find it by its numbers and mailboxes."""
        return self.action("share_card", d=d, c=c)

    def share_card_many(self, items: list[ShareCardArgs]) -> list[dict[str, Any]]:
        """Runs `share_card` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("share_card", items)

    def unshare(self,
        *,
        d: Any,
        person: Any,
        c: Contact,
    ) -> None:
        """Takes the contact out of the directory, leaving its card in the book: what the apps read of it stops naming it, and the card is no person of the directory's until it is shared again."""
        return self.action("unshare", d=d, person=person, c=c)

    def unshare_many(self, items: list[UnshareArgs]) -> list[dict[str, Any]]:
        """Runs `unshare` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unshare", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"confirm", "confirm_many", "change", "change_many", "forget", "forget_many", "share", "share_many", "share_card", "share_card_many", "unshare", "unshare_many"})


