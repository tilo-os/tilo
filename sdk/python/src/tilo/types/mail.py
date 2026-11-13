"""Types for Mail (`so.tilo.mail`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Mailbox` is the app's `mail.mailbox` at `mail/<resource>`, read with
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
    "DraftAttachment",
    "DraftCopy",
    "DraftData",
    "DraftSender",
    "DraftState",
    "Folder",
    "HeldMessage",
    "IngestBaseline",
    "LocalFolder",
    "MessageState",
    "ServerCopy",
    "ServerUpdate",
    "PersonAccount",
    "Endpoint",
    "Person",
    "Projected",
    "KeptCopy",
    "NewCopy",
    "Mailbox",
    "Accounts",
    "Identities",
    "TrustedSenders",
    "Copies",
]

class DraftAttachment(TypedDict):
    content_id: str
    filename: str
    mime: str
    size: int

class DraftCopy(TypedDict):
    account: str
    locator: str
    written: str

class DraftData(TypedDict):
    sender: DraftSender | None
    to: str
    cc: str
    bcc: str
    subject: str
    body: str
    html_body: str | None
    calendar_ics: str | None
    attachments: list[DraftAttachment]

class DraftSender(TypedDict):
    account: str
    email: str

class DraftState(TypedDict):
    sender: Any | None
    to: str
    cc: str
    bcc: str
    subject: str
    body: str
    html_body: str | None
    calendar_ics: str | None
    attachments: list[Any]
    rich_body: str | None
    shown_as: str | None
    drafted: str | None
    in_reply_to: str | None
    notes: str
    server: DraftCopy | None

Folder = Literal["inbox", "starred", "sent", "drafts", "archive", "trash", "junk"]

HeldMessage = TypedDict("HeldMessage", {
    "message_id": "str",
    "from": "str",
    "to": "str",
    "cc": "str",
    "subject": "str",
    "body": "str",
    "minutes": "int",
    "in_reply_to": "str | None",
    "conversation": "str",
})

class IngestBaseline(TypedDict):
    id: str
    read: bool | None
    starred: bool | None
    folder: str | None
    account: str | None
    locator: str | None
    label: str | None

class LocalFolder(TypedDict):
    name: str

class MessageState(TypedDict):
    read: bool
    starred: bool
    folder: Folder
    labels: list[str]
    server: ServerCopy | None
    purged: bool
    label: str

class ServerCopy(TypedDict):
    account: str
    locator: str
    read: bool
    starred: bool
    folder: Folder
    removed: bool

ServerUpdate = dict[str, Any]
"""One of Flags, Move, Remove, SaveDraft: an object whose case the app tags it with."""

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

class KeptCopy(TypedDict):
    account: str
    content: str
    minutes: int
    folder: Folder
    uid: int
    uid_validity: int

class NewCopy(TypedDict):
    id: str
    account: str
    content: str
    minutes: int
    folder: Folder
    uid: int
    uid_validity: int

class Mailbox(Resource):
    """Mail — `mail.mailbox`."""
    __surface__ = "so.tilo.mail@cc453f20b5a856e1"
    messages: list[MessageState]
    folders: list[LocalFolder]
    draft: DraftState
    drafts: list[DraftState]
    held: list[HeldMessage]

    def mark_read(self,
        *,
        messages: list[str],
        read: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("mark-read", messages=messages, read=read)

    def archive(self, *, messages: list[str]) -> Run | None:
        """Runs the app's action."""
        return self.action("archive", messages=messages)

    def trash(self, *, messages: list[str]) -> Run | None:
        """Runs the app's action."""
        return self.action("trash", messages=messages)

    def move_messages(self,
        *,
        messages: list[str],
        to: Folder,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("move-messages", messages=messages, to=to)

    def purge(self, *, messages: list[str]) -> Run | None:
        """Runs the app's action."""
        return self.action("purge", messages=messages)

    def ingest(self, *, messages: list[IngestBaseline]) -> Run | None:
        """Runs the app's action."""
        return self.action("ingest", messages=messages)

    def server_applied(self, *, update: ServerUpdate) -> Run | None:
        """Runs the app's action."""
        return self.action("server-applied", update=update)

    def start_draft_body(self,
        *,
        text: str,
        html: str | None = None,
        draft: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["text"] = text
        if html is not None:
            args["html"] = html
        if draft is not None:
            args["draft"] = draft
        return self.action("start-draft-body", **args)

    def save_draft(self,
        *,
        message: DraftData,
        draft: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        if draft is not None:
            args["draft"] = draft
        return self.action("save-draft", **args)

    def create_draft(self,
        *,
        id: str,
        message: DraftData,
        in_reply_to: str | None = None,
        notes: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["message"] = message
        if in_reply_to is not None:
            args["in_reply_to"] = in_reply_to
        if notes is not None:
            args["notes"] = notes
        return self.action("create-draft", **args)

    def show_draft(self,
        *,
        message: str,
        draft: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        if draft is not None:
            args["draft"] = draft
        return self.action("show-draft", **args)

    def attach(self,
        *,
        content: str,
        name: str,
        size: int | None = None,
        mime: str | None = None,
        draft: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["content"] = content
        args["name"] = name
        if size is not None:
            args["size"] = size
        if mime is not None:
            args["mime"] = mime
        if draft is not None:
            args["draft"] = draft
        return self.action("attach", **args)

    def discard_draft(self,
        *,
        draft: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if draft is not None:
            args["draft"] = draft
        return self.action("discard-draft", **args)

    def send_message(self,
        *,
        account: str,
        message: DraftData,
        in_reply_to: str | None = None,
        draft: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["account"] = account
        args["message"] = message
        if in_reply_to is not None:
            args["in_reply_to"] = in_reply_to
        if draft is not None:
            args["draft"] = draft
        return self.action("send-message", **args)

    def deliver_message(self,
        *,
        from_: str,
        subject: str,
        body: str,
        to: str | None = None,
        cc: str | None = None,
        date: str | None = None,
        folder: str | None = None,
        read: bool | None = None,
        reply_to: str | None = None,
        conversation: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["from"] = from_
        if to is not None:
            args["to"] = to
        if cc is not None:
            args["cc"] = cc
        args["subject"] = subject
        args["body"] = body
        if date is not None:
            args["date"] = date
        if folder is not None:
            args["folder"] = folder
        if read is not None:
            args["read"] = read
        if reply_to is not None:
            args["reply_to"] = reply_to
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("deliver-message", **args)

    def draft_message(self,
        *,
        to: str,
        subject: str,
        body: str,
        cc: str | None = None,
        notes: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["subject"] = subject
        args["body"] = body
        if cc is not None:
            args["cc"] = cc
        if notes is not None:
            args["notes"] = notes
        return self.action("draft-message", **args)

    def cancel_send(self, *, id: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("cancel-send", id=id)

    def search_messages(self,
        *,
        query: str | None = None,
        folder: Folder | None = None,
        from_: str | None = None,
        subject: str | None = None,
        since: str | None = None,
        until: str | None = None,
        has_attachment: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if query is not None:
            args["query"] = query
        if folder is not None:
            args["folder"] = folder
        if from_ is not None:
            args["from"] = from_
        if subject is not None:
            args["subject"] = subject
        if since is not None:
            args["since"] = since
        if until is not None:
            args["until"] = until
        if has_attachment is not None:
            args["has_attachment"] = has_attachment
        return self.action("search-messages", **args)

    def export_content(self,
        *,
        format: str | None = None,
        message: str | None = None,
        folder: Folder | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        if message is not None:
            args["message"] = message
        if folder is not None:
            args["folder"] = folder
        return self.action("export-content", **args)


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.mail@cc453f20b5a856e1"
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
    __surface__ = "so.tilo.mail@cc453f20b5a856e1"
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


class TrustedSenders(Resource):
    """Trusted senders — `mail.trusted-senders`."""
    __surface__ = "so.tilo.mail@cc453f20b5a856e1"
    senders: list[str]

    def trust(self, *, sender: str) -> Run | None:
        """Runs the app's action."""
        return self.action("trust", sender=sender)

    def distrust(self, *, sender: str) -> Run | None:
        """Runs the app's action."""
        return self.action("distrust", sender=sender)


class Copies(Resource):
    """Mail on your devices — `mail.copies`."""
    __surface__ = "so.tilo.mail@cc453f20b5a856e1"
    @property
    def copies(self) -> list[KeptCopy]:
        """Read only."""
        return self.fields["copies"]

    def keep_copies(self, *, copies: list[NewCopy]) -> Run | None:
        """Runs the app's action."""
        return self.action("keep-copies", copies=copies)

    def forget_copies(self, *, messages: list[str]) -> Run | None:
        """Runs the app's action."""
        return self.action("forget-copies", messages=messages)


