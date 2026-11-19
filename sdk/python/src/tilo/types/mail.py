"""Types for Mail (`so.tilo.mail`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Mailbox` is the app's `mail.mailbox` at `mail/<resource>`, read with
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
    "DraftAttachment",
    "DraftCopy",
    "DraftData",
    "DraftSender",
    "DraftState",
    "Folder",
    "LocalFolder",
    "PreviousDraft",
    "PreviousFolder",
    "RetiredCopy",
    "ServerUpdate",
    "Delivery",
    "HeldMessage",
    "IngestBaseline",
    "MessageState",
    "ServerCopy",
    "PersonAccount",
    "Endpoint",
    "Person",
    "Projected",
    "KeptCopy",
    "NewCopy",
    "Mailbox",
    "Messages",
    "Accounts",
    "Identities",
    "TrustedSenders",
    "Copies",
    "MarkReadArgs",
    "ArchiveArgs",
    "TrashArgs",
    "MoveMessagesArgs",
    "PurgeArgs",
    "IngestArgs",
    "ServerAppliedArgs",
    "ImportPreviousMessageArgs",
    "PublishBrowserIdentityArgs",
    "SaveAccountArgs",
    "RemoveAccountArgs",
    "SetActiveAccountArgs",
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
    "TrustArgs",
    "DistrustArgs",
    "ImportLegacyArgs",
    "KeepCopiesArgs",
    "ImportCopiesArgs",
    "ForgetCopiesArgs",
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

class LocalFolder(TypedDict):
    name: str

class PreviousDraft(TypedDict):
    id: str
    message: DraftData
    in_reply_to: str | None
    notes: str

class PreviousFolder(TypedDict):
    id: str
    name: str

class RetiredCopy(TypedDict):
    account: str
    locator: str
    removed: bool

ServerUpdate = dict[str, Any]
"""One of Flags, Move, Remove, SaveDraft: an object whose case the app tags it with."""

Delivery = TypedDict("Delivery", {
    "from": "str",
    "to": "str | None",
    "cc": "str | None",
    "subject": "str",
    "body": "str",
    "date": "str | None",
    "folder": "Folder | None",
    "read": "bool | None",
    "reply_to": "str | None",
    "conversation": "str | None",
    "message_id": "str | None",
    "original": "Any | None",
    "answers": "str | None",
})

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
    "original": "Any | None",
})

class IngestBaseline(TypedDict):
    id: str
    read: bool | None
    starred: bool | None
    folder: str | None
    account: str | None
    locator: str | None
    label: str | None

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
    endpoints: list[Entry[Endpoint]]

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

MarkReadArgs = TypedDict("MarkReadArgs", {
    "messages": "list[str]",
    "read": "bool",
})
"""The arguments of `mark-read`."""

ArchiveArgs = TypedDict("ArchiveArgs", {
    "messages": "list[str]",
})
"""The arguments of `archive`."""

TrashArgs = TypedDict("TrashArgs", {
    "messages": "list[str]",
})
"""The arguments of `trash`."""

MoveMessagesArgs = TypedDict("MoveMessagesArgs", {
    "messages": "list[str]",
    "to": "Folder",
})
"""The arguments of `move-messages`."""

PurgeArgs = TypedDict("PurgeArgs", {
    "messages": "list[str]",
})
"""The arguments of `purge`."""

IngestArgs = TypedDict("IngestArgs", {
    "messages": "list[IngestBaseline]",
})
"""The arguments of `ingest`."""

ServerAppliedArgs = TypedDict("ServerAppliedArgs", {
    "updates": "list[ServerUpdate]",
})
"""The arguments of `server-applied`."""

ImportPreviousMessageArgs = TypedDict("ImportPreviousMessageArgs", {
    "id": "str",
    "state": "Any",
    "body": NotRequired["Any | None"],
})
"""The arguments of `import-previous-message`."""

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

SetActiveAccountArgs = TypedDict("SetActiveAccountArgs", {
    "account": "str",
})
"""The arguments of `set-active-account`."""

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

TrustArgs = TypedDict("TrustArgs", {
    "sender": "str",
})
"""The arguments of `trust`."""

DistrustArgs = TypedDict("DistrustArgs", {
    "sender": "str",
})
"""The arguments of `distrust`."""

ImportLegacyArgs = TypedDict("ImportLegacyArgs", {
    "senders": "list[str]",
})
"""The arguments of `import-legacy`."""

KeepCopiesArgs = TypedDict("KeepCopiesArgs", {
    "copies": "list[NewCopy]",
})
"""The arguments of `keep-copies`."""

ImportCopiesArgs = TypedDict("ImportCopiesArgs", {
    "copies": "list[NewCopy]",
})
"""The arguments of `import-copies`."""

ForgetCopiesArgs = TypedDict("ForgetCopiesArgs", {
    "messages": "list[str]",
    "account": NotRequired["str | None"],
})
"""The arguments of `forget-copies`."""

class Mailbox(Resource):
    """Mail — `mail.mailbox`."""
    __surface__ = "so.tilo.mail@f1aaa59c488f24c7"
    folders: list[Entry[LocalFolder]]
    draft: DraftState
    drafts: list[Entry[DraftState]]
    @property
    def retired(self) -> list[Entry[RetiredCopy]]:
        """Read only."""
        return self.fields["retired"]
    @property
    def imported(self) -> list[str]:
        """Read only."""
        return self.fields["imported"]

    def import_mailbox_entries(self,
        *,
        folders: list[PreviousFolder],
        drafts: list[PreviousDraft],
    ) -> None:
        """Runs the app's action."""
        return self.action("import-mailbox-entries", folders=folders, drafts=drafts)

    def server_applied(self, *, update: ServerUpdate) -> None:
        """Runs the app's action."""
        return self.action("server-applied", update=update)

    def start_draft_body(self,
        *,
        text: str,
        html: str | None = None,
        draft: str | None = None,
    ) -> None:
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
    ) -> None:
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
    ) -> Any:
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
    ) -> None:
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
    ) -> None:
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
    ) -> None:
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
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["account"] = account
        args["message"] = message
        if in_reply_to is not None:
            args["in_reply_to"] = in_reply_to
        if draft is not None:
            args["draft"] = draft
        return self.action("send-message", **args)

    def check_mail(self,
        *,
        account: str,
        folder: Folder,
    ) -> Any:
        """Runs the app's action."""
        return self.action("check-mail", account=account, folder=folder)

    def draft_message(self,
        *,
        to: str,
        subject: str,
        body: str,
        cc: str | None = None,
        notes: str | None = None,
    ) -> Any:
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

    def cancel_send(self, *, id: str) -> Any:
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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_mailbox_entries", "server_applied", "start_draft_body", "save_draft", "create_draft", "show_draft", "attach", "discard_draft", "send_message", "check_mail", "draft_message", "cancel_send", "search_messages", "export_content"})


class Messages(Resource):
    """Messages — `mail.messages`."""
    __surface__ = "so.tilo.mail@f1aaa59c488f24c7"
    messages: list[Entry[MessageState]]
    @property
    def held(self) -> list[Entry[HeldMessage]]:
        """Read only."""
        return self.fields["held"]

    def mark_read(self,
        *,
        messages: list[str],
        read: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("mark-read", messages=messages, read=read)

    def mark_read_many(self, items: list[MarkReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark-read", items)

    def archive(self, *, messages: list[str]) -> None:
        """Runs the app's action."""
        return self.action("archive", messages=messages)

    def archive_many(self, items: list[ArchiveArgs]) -> list[dict[str, Any]]:
        """Runs `archive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("archive", items)

    def trash(self, *, messages: list[str]) -> None:
        """Runs the app's action."""
        return self.action("trash", messages=messages)

    def trash_many(self, items: list[TrashArgs]) -> list[dict[str, Any]]:
        """Runs `trash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trash", items)

    def move_messages(self,
        *,
        messages: list[str],
        to: Folder,
    ) -> None:
        """Runs the app's action."""
        return self.action("move-messages", messages=messages, to=to)

    def move_messages_many(self, items: list[MoveMessagesArgs]) -> list[dict[str, Any]]:
        """Runs `move-messages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-messages", items)

    def purge(self, *, messages: list[str]) -> None:
        """Runs the app's action."""
        return self.action("purge", messages=messages)

    def purge_many(self, items: list[PurgeArgs]) -> list[dict[str, Any]]:
        """Runs `purge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("purge", items)

    def ingest(self, *, messages: list[IngestBaseline]) -> None:
        """Runs the app's action."""
        return self.action("ingest", messages=messages)

    def ingest_many(self, items: list[IngestArgs]) -> list[dict[str, Any]]:
        """Runs `ingest` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ingest", items)

    def server_applied(self, *, updates: list[ServerUpdate]) -> None:
        """Runs the app's action."""
        return self.action("server-applied", updates=updates)

    def server_applied_many(self, items: list[ServerAppliedArgs]) -> list[dict[str, Any]]:
        """Runs `server-applied` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("server-applied", items)

    def import_previous_message(self,
        *,
        id: str,
        state: Any,
        body: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["state"] = state
        if body is not None:
            args["body"] = body
        return self.action("import-previous-message", **args)

    def import_previous_message_many(self, items: list[ImportPreviousMessageArgs]) -> list[dict[str, Any]]:
        """Runs `import-previous-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-previous-message", items)

    def deliver_message(self,
        *,
        from_: str,
        subject: str,
        body: str,
        to: str | None = None,
        cc: str | None = None,
        date: str | None = None,
        folder: Folder | None = None,
        read: bool | None = None,
        reply_to: str | None = None,
        conversation: str | None = None,
        message_id: str | None = None,
        original: Any | None = None,
        answers: str | None = None,
    ) -> Any:
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
        if message_id is not None:
            args["message_id"] = message_id
        if original is not None:
            args["original"] = original
        if answers is not None:
            args["answers"] = answers
        return self.action("deliver-message", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"mark_read", "mark_read_many", "archive", "archive_many", "trash", "trash_many", "move_messages", "move_messages_many", "purge", "purge_many", "ingest", "ingest_many", "server_applied", "server_applied_many", "import_previous_message", "import_previous_message_many", "deliver_message"})


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.mail@f1aaa59c488f24c7"
    accounts: list[Entry[PersonAccount]]
    active: dict[str, str]

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

    def set_active_account(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("set-active-account", account=account)

    def set_active_account_many(self, items: list[SetActiveAccountArgs]) -> list[dict[str, Any]]:
        """Runs `set-active-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-active-account", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"publish_browser_identity", "publish_browser_identity_many", "save_account", "save_account_many", "remove_account", "remove_account_many", "set_active_account", "set_active_account_many"})


class Identities(Resource):
    """Contact identities — `tilo.contacts.identities`."""
    __surface__ = "so.tilo.mail@f1aaa59c488f24c7"
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


class TrustedSenders(Resource):
    """Trusted senders — `mail.trusted-senders`."""
    __surface__ = "so.tilo.mail@f1aaa59c488f24c7"
    senders: list[str]
    @property
    def distrusted(self) -> list[str]:
        """Read only."""
        return self.fields["distrusted"]

    def trust(self, *, sender: str) -> None:
        """Runs the app's action."""
        return self.action("trust", sender=sender)

    def trust_many(self, items: list[TrustArgs]) -> list[dict[str, Any]]:
        """Runs `trust` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trust", items)

    def distrust(self, *, sender: str) -> None:
        """Runs the app's action."""
        return self.action("distrust", sender=sender)

    def distrust_many(self, items: list[DistrustArgs]) -> list[dict[str, Any]]:
        """Runs `distrust` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("distrust", items)

    def import_legacy(self, *, senders: list[str]) -> None:
        """Runs the app's action."""
        return self.action("import-legacy", senders=senders)

    def import_legacy_many(self, items: list[ImportLegacyArgs]) -> list[dict[str, Any]]:
        """Runs `import-legacy` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-legacy", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"trust", "trust_many", "distrust", "distrust_many", "import_legacy", "import_legacy_many"})


class Copies(Resource):
    """Workspace mail — `mail.copies`."""
    __surface__ = "so.tilo.mail@f1aaa59c488f24c7"
    @property
    def copies(self) -> list[Entry[KeptCopy]]:
        """Read only."""
        return self.fields["copies"]
    @property
    def removed(self) -> list[str]:
        """Read only."""
        return self.fields["removed"]

    def keep_copies(self, *, copies: list[NewCopy]) -> None:
        """Runs the app's action."""
        return self.action("keep-copies", copies=copies)

    def keep_copies_many(self, items: list[KeepCopiesArgs]) -> list[dict[str, Any]]:
        """Runs `keep-copies` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep-copies", items)

    def import_copies(self, *, copies: list[NewCopy]) -> None:
        """Runs the app's action."""
        return self.action("import-copies", copies=copies)

    def import_copies_many(self, items: list[ImportCopiesArgs]) -> list[dict[str, Any]]:
        """Runs `import-copies` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-copies", items)

    def forget_copies(self,
        *,
        messages: list[str],
        account: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["messages"] = messages
        if account is not None:
            args["account"] = account
        return self.action("forget-copies", **args)

    def forget_copies_many(self, items: list[ForgetCopiesArgs]) -> list[dict[str, Any]]:
        """Runs `forget-copies` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget-copies", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep_copies", "keep_copies_many", "import_copies", "import_copies_many", "forget_copies", "forget_copies_many"})


