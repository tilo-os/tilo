"""Types for Contacts (`so.tilo.contacts`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `contacts/<resource>`, read with
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
    "VCardVersion",
    "CardFate",
    "MergeField",
    "Notice",
    "Labeled",
    "Address",
    "Contact",
    "Group",
    "ImportCard",
    "ImportReport",
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
    "KeepBirthdayYearArgs",
    "UngroupArgs",
    "GroupArgs",
    "DeleteArgs",
    "MergeIntoArgs",
    "DismissImportArgs",
    "KeepExistingArgs",
    "ConfirmArgs",
    "ChangeArgs",
    "ForgetArgs",
    "ShareArgs",
    "ShareCardArgs",
    "UnshareArgs",
]

VCardVersion = Literal["v4", "v3"]
"""The versions of the card format another address book reads."""

CardFate = Literal["taken", "held", "review", "skipped"]
"""What one card of a file is to the book."""

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

KeepBirthdayYearArgs = TypedDict("KeepBirthdayYearArgs", {
    "on": "bool",
})
"""The arguments of `keep_birthday_year`."""

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

DismissImportArgs = TypedDict("DismissImportArgs", {
})
"""The arguments of `dismiss_import`."""

KeepExistingArgs = TypedDict("KeepExistingArgs", {
    "p": "ImportCard",
})
"""The arguments of `keep_existing`."""

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

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
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


class Labeled(Resource):
    """Labeled — `tl.so.tilo.contacts.Labeled`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
    fzyd2a52: str  # label
    b4cilqkx: str  # value


class Address(Resource):
    """Address — `tl.so.tilo.contacts.Address`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
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
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
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
    def _32tuvb6l(self) -> bool:
        """birthday_year (`32tuvb6l` on the wire)."""
        return self.fields["32tuvb6l"]

    @_32tuvb6l.setter
    def _32tuvb6l(self, value: bool) -> None:
        self.edit("32tuvb6l", value=value)
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

    def keep_birthday_year(self, *, on: bool) -> None:
        """Keeps a birthday with its year or without it: a birthday kept without one is the day it falls on every year, as many address books keep one."""
        return self.action("keep_birthday_year", on=on)

    def keep_birthday_year_many(self, items: list[KeepBirthdayYearArgs]) -> list[dict[str, Any]]:
        """Runs `keep_birthday_year` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_birthday_year", items)

    def ungroup(self, *, name: str) -> None:
        """Takes a contact out of a group."""
        return self.action("ungroup", name=name)

    def ungroup_many(self, items: list[UngroupArgs]) -> list[dict[str, Any]]:
        """Runs `ungroup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ungroup", items)

    def export_contact(self,
        *,
        version: VCardVersion | None = None,
    ) -> Any:
        """A contact as a file: their card, named for them. The system's one Export… lists it and saves it where the person says."""
        args: dict[str, Any] = {}
        if version is not None:
            args["version"] = version
        return self.action("export_contact", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "rename", "rename_many", "favorite", "favorite_many", "add_phone", "remove_phone", "remove_phone_many", "add_email", "remove_email", "remove_email_many", "add_address", "remove_address", "remove_address_many", "keep_birthday_year", "keep_birthday_year_many", "ungroup", "ungroup_many", "export_contact"})


class Group(Resource):
    """Group — `tl.so.tilo.contacts.Group`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
    jn6l7z6v: str  # name


class ImportCard(Resource):
    """ImportCard — `tl.so.tilo.contacts.ImportCard`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
    op5ercnd: str  # card
    a4s7qvbf: str  # name
    kcnlll33: str  # uid
    rsywb3bl: Contact | None  # existing


class ImportReport(Resource):
    """ImportReport — `tl.so.tilo.contacts.ImportReport`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
    ydus2juj: int  # imported
    bqy264j6: int  # unchanged
    jm2un2fr: int  # awaiting
    sikc2jgn: int  # skipped
    azh3qbqs: bool  # seen


class Book(Resource):
    """Book — `tl.so.tilo.contacts.Book`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
    qfj7l6eu: list[Entry[Contact]]  # contacts
    uyyz5pg2: list[Entry[Group]]  # groups
    zc4aqhgg: list[Entry[ImportCard]]  # review
    scky6ngy: ImportReport  # last_import

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

    def export_book(self,
        *,
        version: VCardVersion | None = None,
    ) -> Any:
        """Every contact as one vCard file, which another address book reads whole."""
        args: dict[str, Any] = {}
        if version is not None:
            args["version"] = version
        return self.action("export_book", **args)

    def export_book_v3(self) -> Any:
        """Every contact as one vCard 3.0 file, which an address book that reads no 4.0 card opens."""
        return self.action("export_book_v3")

    def export_vcard_content(self,
        *,
        name: str | None = None,
    ) -> Any:
        """Every contact as the text of one card file, or the one `name` calls: for an agent or a script that hands the cards on where a person's file chooser cannot, as the address its bytes are kept at. A name the book holds twice is refused rather than guessed."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("export_vcard_content", **args)

    def import_vcard(self, *, file: Any) -> ImportReport:
        """Imports the contacts a vCard file holds into the book, each new card a contact of its own, and answers what it did. A card whose id the book already holds unchanged is nobody new; one that says something else about the person waits in the book's review, and the contact stays as it is until someone says which card is theirs. A file beyond what this app reads is refused whole: what the refusal says is why."""
        return self.action("import_vcard", file=file)

    def add_contacts(self, *, cards: str) -> ImportReport:
        """The cards `cards`, the words of a vCard file, as contacts of this book, and what became of each: what an importer, an agent or a script hands a book in one call."""
        return self.action("add_contacts", cards=cards)

    def refuse_import(self, *, skipped: int) -> ImportReport:
        """The report of an import of `skipped` cards, and no others: what a file that could not be read answers."""
        return self.action("refuse_import", skipped=skipped)

    def report_import(self,
        *,
        imported: int,
        unchanged: int,
        awaiting: int,
        skipped: int,
    ) -> ImportReport:
        """What an import did, kept on the book for the window's banner to read and answer."""
        return self.action("report_import", imported=imported, unchanged=unchanged, awaiting=awaiting, skipped=skipped)

    def dismiss_import(self) -> None:
        """The person has read what the last import did: the banner stops showing it."""
        return self.action("dismiss_import")

    def dismiss_import_many(self, items: list[DismissImportArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_import", items)

    def file_card(self,
        *,
        card: str,
        uid: str | None = None,
    ) -> CardFate:
        """Brings one card of a vCard file to the book: takes it as a contact, finds the book already holds it unchanged, puts it up for review where it says something else about a contact the book holds by the same id, or refuses a card that names nobody. `uid` is the id the caller gives a card whose file names none, so the caller can find the contact it makes."""
        args: dict[str, Any] = {}
        args["card"] = card
        if uid is not None:
            args["uid"] = uid
        return self.action("file_card", **args)

    def take_card(self,
        *,
        card: str,
        id: str | None = None,
    ) -> Contact:
        """The contact a card makes in the book, under the id `id`: what a new card becomes, and what a card someone chose over the one the book held becomes. Its rows are filled after it stands in the book, as every resource's are."""
        args: dict[str, Any] = {}
        args["card"] = card
        if id is not None:
            args["id"] = id
        return self.action("take_card", **args)

    def add_card(self, *, card: str) -> Contact | None:
        """The contact a vCard card says, made in the book, or none for a card that names nobody and for one the book already holds: the single-card form of importing a file."""
        return self.action("add_card", card=card)

    def import_as_new(self, *, p: ImportCard) -> Contact:
        """Takes a card that waited for review as a contact of its own, beside the one the book holds under the id it named: the file's card is the person's, and the card the book kept keeps everything of its own. Its id is new, because the two cards are two now."""
        return self.action("import_as_new", p=p)

    def keep_existing(self, *, p: ImportCard) -> None:
        """Leaves the card the book holds as it is, and takes the file's card out of the review."""
        return self.action("keep_existing", p=p)

    def keep_existing_many(self, items: list[KeepExistingArgs]) -> list[dict[str, Any]]:
        """Runs `keep_existing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_existing", items)

    def find_for_address(self, *, address: str) -> Contact | None:
        """The contact the book holds for `address`, as an action: what a person, an agent or another app asks before they write to it. Nothing is changed and nothing is guessed from a name."""
        return self.action("find_for_address", address=address)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_contact", "named_or_new", "group", "group_many", "new_group", "delete", "delete_many", "merge_into", "merge_into_many", "export_book", "export_book_v3", "export_vcard_content", "import_vcard", "add_contacts", "refuse_import", "report_import", "dismiss_import", "dismiss_import_many", "file_card", "take_card", "add_card", "import_as_new", "keep_existing", "keep_existing_many", "find_for_address"})


class Contacts(Resource):
    """Contacts — `tl.so.tilo.contacts`."""
    __surface__ = "so.tilo.contacts@c32a407883c8217b"
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


