"""Types for Mail (`so.tilo.lang.mail`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Account` is the app's `tl.so.tilo.lang.mail.Account` at `mail/<resource>`, read with
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
    "Account",
    "Mailbox",
    "Thread",
    "Label",
    "Message",
    "Draft",
    "Mail",
    "LangMail",
    "SetSignatureArgs",
    "MarkAllReadArgs",
    "ArchiveArgs",
    "ToInboxArgs",
    "MoveToArgs",
    "DeleteArgs",
    "FlagArgs",
    "SnoozeArgs",
    "SnoozeAnHourArgs",
    "SnoozeUntilTomorrowArgs",
    "UnsnoozeArgs",
    "LabelArgs",
    "MuteArgs",
    "MarkReadArgs",
    "SendFromArgs",
    "SetHoldArgs",
    "RenameLabelArgs",
    "DeleteLabelArgs",
    "DiscardArgs",
]

SetSignatureArgs = TypedDict("SetSignatureArgs", {
    "signature": "str",
})
"""The arguments of `set_signature`."""

MarkAllReadArgs = TypedDict("MarkAllReadArgs", {
})
"""The arguments of `mark_all_read`."""

ArchiveArgs = TypedDict("ArchiveArgs", {
})
"""The arguments of `archive`."""

ToInboxArgs = TypedDict("ToInboxArgs", {
})
"""The arguments of `to_inbox`."""

MoveToArgs = TypedDict("MoveToArgs", {
    "to": "Mailbox",
})
"""The arguments of `move_to`."""

DeleteArgs = TypedDict("DeleteArgs", {
})
"""The arguments of `delete`."""

FlagArgs = TypedDict("FlagArgs", {
    "flagged": NotRequired["bool | None"],
})
"""The arguments of `flag`."""

SnoozeArgs = TypedDict("SnoozeArgs", {
    "until": "str",
})
"""The arguments of `snooze`."""

SnoozeAnHourArgs = TypedDict("SnoozeAnHourArgs", {
})
"""The arguments of `snooze_an_hour`."""

SnoozeUntilTomorrowArgs = TypedDict("SnoozeUntilTomorrowArgs", {
})
"""The arguments of `snooze_until_tomorrow`."""

UnsnoozeArgs = TypedDict("UnsnoozeArgs", {
})
"""The arguments of `unsnooze`."""

LabelArgs = TypedDict("LabelArgs", {
    "name": "str",
    "on": NotRequired["bool | None"],
})
"""The arguments of `label`."""

MuteArgs = TypedDict("MuteArgs", {
    "muted": NotRequired["bool | None"],
})
"""The arguments of `mute`."""

MarkReadArgs = TypedDict("MarkReadArgs", {
    "unread": NotRequired["bool | None"],
})
"""The arguments of `mark_read`."""

SendFromArgs = TypedDict("SendFromArgs", {
    "a": "Account",
})
"""The arguments of `send_from`."""

SetHoldArgs = TypedDict("SetHoldArgs", {
    "seconds": "int",
})
"""The arguments of `set_hold`."""

RenameLabelArgs = TypedDict("RenameLabelArgs", {
    "l": "Label",
    "name": "str",
})
"""The arguments of `rename_label`."""

DeleteLabelArgs = TypedDict("DeleteLabelArgs", {
    "l": "Label",
})
"""The arguments of `delete_label`."""

DiscardArgs = TypedDict("DiscardArgs", {
    "d": "Draft",
})
"""The arguments of `discard`."""

class Account(Resource):
    """Account — `tl.so.tilo.lang.mail.Account`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    yujnud5c: str  # key
    _4lo3ei5n: str  # address
    j6nqqrit: str  # name
    b5k7mvka: str  # trouble
    _33f3rqw2: bool  # online
    hzbwof46: str  # fetched
    jxc5rufs: str  # signature
    ehmoyzk2: list[Entry[Mailbox]]  # mailboxes
    qybqg4dl: list[Entry[Thread]]  # threads

    def set_signature(self, *, signature: str) -> None:
        """Sets or clears the workspace account's signature."""
        return self.action("set_signature", signature=signature)

    def set_signature_many(self, items: list[SetSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `set_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_signature", items)

    def check_now(self) -> None:
        """Asks the account's server for its mail now: what a person presses when the window says the server could not be reached, or that what it fetched could not be saved. The account says how it stands once it answers."""
        return self.action("check_now")

    def new_message(self) -> Draft:
        """Starts a message from the account."""
        return self.action("new_message")

    def mark_all_read(self) -> None:
        """Marks every conversation of the account read."""
        return self.action("mark_all_read")

    def mark_all_read_many(self, items: list[MarkAllReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark_all_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_all_read", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_signature", "set_signature_many", "check_now", "new_message", "mark_all_read", "mark_all_read_many"})


class Mailbox(Resource):
    """Mailbox — `tl.so.tilo.lang.mail.Mailbox`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    mh47w3p6: str  # key
    tlasysm5: str  # name
    xhiqdk3p: str  # role
    dtd4czyv: int  # unread
    rxoe5vzb: int  # total


class Thread(Resource):
    """Thread — `tl.so.tilo.lang.mail.Thread`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    ecmpoary: str  # key
    zu23m2dm: str  # account
    u732lnuk: str  # subject
    o3knjgos: str  # latest
    sxcvmlon: str  # snippet
    vyc7aj7v: str  # senders
    izafodav: int  # count
    _56lwowdg: bool  # unread
    abe6c5jl: bool  # flagged
    _3vusnp7i: list[str]  # boxes
    ueyfpzep: list[Entry[Message]]  # messages
    gy7z7du3: list[str]  # labels
    bdoh33hs: str  # snoozed_until
    z7qxz544: bool  # muted

    def reply(self) -> Draft:
        """Answers the conversation's newest message, to whoever wrote it, quoting it under what is written."""
        return self.action("reply")

    def reply_all(self) -> Draft:
        """Answers the conversation's newest message, to everyone it went to, quoting it."""
        return self.action("reply_all")

    def forward(self) -> Draft:
        """Sends the conversation's newest message on, with its sender and what it says."""
        return self.action("forward")

    def archive(self) -> None:
        """Archives the conversation: its messages leave the inbox."""
        return self.action("archive")

    def archive_many(self, items: list[ArchiveArgs]) -> list[dict[str, Any]]:
        """Runs `archive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("archive", items)

    def to_inbox(self) -> None:
        """Moves the conversation back to the inbox, and unmutes it: what a person brings back is what they want to see again."""
        return self.action("to_inbox")

    def to_inbox_many(self, items: list[ToInboxArgs]) -> list[dict[str, Any]]:
        """Runs `to_inbox` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("to_inbox", items)

    def move_to(self, *, to: Mailbox) -> None:
        """Moves the conversation to another mailbox of its account."""
        return self.action("move_to", to=to)

    def move_to_many(self, items: list[MoveToArgs]) -> list[dict[str, Any]]:
        """Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to", items)

    def delete(self) -> None:
        """Deletes the conversation: its messages go to the trash."""
        return self.action("delete")

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def flag(self,
        *,
        flagged: bool | None = None,
    ) -> None:
        """Flags or unflags the conversation."""
        args: dict[str, Any] = {}
        if flagged is not None:
            args["flagged"] = flagged
        return self.action("flag", **args)

    def flag_many(self, items: list[FlagArgs]) -> list[dict[str, Any]]:
        """Runs `flag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("flag", items)

    def snooze(self, *, until: str) -> None:
        """Puts the conversation away until a moment: it leaves the inbox, and comes back once that moment has come, with no window open. The moment is a field, so it survives closing and reopening, and an agent snoozes the same way."""
        return self.action("snooze", until=until)

    def snooze_many(self, items: list[SnoozeArgs]) -> list[dict[str, Any]]:
        """Runs `snooze` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("snooze", items)

    def snooze_an_hour(self) -> None:
        """Puts the conversation away for an hour, the snooze people reach for most."""
        return self.action("snooze_an_hour")

    def snooze_an_hour_many(self, items: list[SnoozeAnHourArgs]) -> list[dict[str, Any]]:
        """Runs `snooze_an_hour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("snooze_an_hour", items)

    def snooze_until_tomorrow(self) -> None:
        """Puts the conversation away until tomorrow morning, at nine, in the person's own zone."""
        return self.action("snooze_until_tomorrow")

    def snooze_until_tomorrow_many(self, items: list[SnoozeUntilTomorrowArgs]) -> list[dict[str, Any]]:
        """Runs `snooze_until_tomorrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("snooze_until_tomorrow", items)

    def unsnooze(self) -> None:
        """Brings a snoozed conversation back now, whatever its moment was: it is in the inbox again."""
        return self.action("unsnooze")

    def unsnooze_many(self, items: list[UnsnoozeArgs]) -> list[dict[str, Any]]:
        """Runs `unsnooze` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unsnooze", items)

    def label(self,
        *,
        name: str,
        on: bool | None = None,
    ) -> None:
        """Gives the conversation a label, or takes one away: `on` says which. The label joins the workspace's labels the first time it is given, so it has a filter of its own from then on."""
        args: dict[str, Any] = {}
        args["name"] = name
        if on is not None:
            args["on"] = on
        return self.action("label", **args)

    def label_many(self, items: list[LabelArgs]) -> list[dict[str, Any]]:
        """Runs `label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("label", items)

    def mute(self,
        *,
        muted: bool | None = None,
    ) -> None:
        """Mutes the conversation, or unmutes it: a muted one stays out of the inbox until it is brought back, whatever its new messages do."""
        args: dict[str, Any] = {}
        if muted is not None:
            args["muted"] = muted
        return self.action("mute", **args)

    def mute_many(self, items: list[MuteArgs]) -> list[dict[str, Any]]:
        """Runs `mute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mute", items)

    def mark_read(self,
        *,
        unread: bool | None = None,
    ) -> None:
        """Marks the conversation read, or unread again."""
        args: dict[str, Any] = {}
        if unread is not None:
            args["unread"] = unread
        return self.action("mark_read", **args)

    def mark_read_many(self, items: list[MarkReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_read", items)

    def answer(self, *, body: str) -> Draft:
        """Answers the conversation with what was typed: the message goes to whoever wrote its newest, as a reply to it, quoted."""
        return self.action("answer", body=body)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reply", "reply_all", "forward", "archive", "archive_many", "to_inbox", "to_inbox_many", "move_to", "move_to_many", "delete", "delete_many", "flag", "flag_many", "snooze", "snooze_many", "snooze_an_hour", "snooze_an_hour_many", "snooze_until_tomorrow", "snooze_until_tomorrow_many", "unsnooze", "unsnooze_many", "label", "label_many", "mute", "mute_many", "mark_read", "mark_read_many", "answer"})


class Label(Resource):
    """Label — `tl.so.tilo.lang.mail.Label`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    ammoua3h: str  # name


class Message(Resource):
    """Message — `tl.so.tilo.lang.mail.Message`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    kzjq6hvc: str  # key
    kdxoo3c6: str  # account
    ntqgnz4f: str  # subject
    l3nc65iv: str  # from
    mo6l36a3: str  # from_name
    _46k23iwq: str  # to
    bgzxugi5: str  # cc
    tfxwkfn2: str  # received
    sbrol5fc: str  # snippet
    _4bchz3ch: str  # body
    anuds3qt: str  # message_id
    asu6ymha: str  # references
    _4zhzrkne: str  # attachments
    ubfcf6ep: bool  # unread
    vxxb4sqr: bool  # flagged
    xlejsz5j: str  # mailbox

    def reply_to(self) -> Draft:
        """Answers one message of the conversation, to whoever wrote it, quoting it."""
        return self.action("reply_to")

    def reply_all_to(self) -> Draft:
        """Answers one message of the conversation, to everyone it went to but the person answering."""
        return self.action("reply_all_to")

    def forward_message(self) -> Draft:
        """Sends one message of the conversation on, with its sender, when and what it says."""
        return self.action("forward_message")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reply_to", "reply_all_to", "forward_message"})


class Draft(Resource):
    """Draft — `tl.so.tilo.lang.mail.Draft`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    m2g6zrle: str  # from
    hd3o4gkq: str  # to
    cwukhrvn: str  # cc
    tyxmravh: str  # subject
    d5u2c7jm: str  # body
    w6twdkob: str  # in_reply_to
    w26tzvt4: str  # references
    yfyt2cfh: bool  # sent
    lgrwf5yf: str  # send_at
    h2nvr2op: bool  # recalled

    def cancel_later(self) -> None:
        """Brings a message back to the outbox: it was sent to go at a moment still to come, and it is a draft again, whatever the moment."""
        return self.action("cancel_later")

    def send_later(self, *, at: str) -> None:
        """Sends the message at a moment instead of now: it waits in the outbox until then, as the person's hold makes a message wait, and goes with no window open. Until it goes it can be taken back, and bringing it back leaves it a draft."""
        return self.action("send_later", at=at)

    def send_in_the_morning(self) -> None:
        """Sends the message at nine tomorrow, the moment people reach for most when they write in the evening and do not want it to arrive then."""
        return self.action("send_in_the_morning")

    def send_from(self, *, a: Account) -> None:
        """Sends the message from another workspace account."""
        return self.action("send_from", a=a)

    def send_from_many(self, items: list[SendFromArgs]) -> list[dict[str, Any]]:
        """Runs `send_from` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send_from", items)

    def send(self) -> None:
        """Sends the message from its account. It leaves the system, so it goes once, and waits in the outbox until the server took it; it is held back for the person's hold first, so they can take it back. The effect is an argument the action records, so the wait survives a window closing and an agent takes it back the same way."""
        return self.action("send")

    def undo_send(self) -> None:
        """Takes back a message that was sent and has not gone yet: it never leaves, and the message is a draft again, as it was. Once it went it cannot be taken back."""
        return self.action("undo_send")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"cancel_later", "send_later", "send_in_the_morning", "send_from", "send_from_many", "send", "undo_send"})


class Mail(Resource):
    """Mail — `tl.so.tilo.lang.mail.Mail`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"
    blig7xcq: list[Entry[Account]]  # accounts
    gkuo27e6: list[Entry[Draft]]  # drafts
    z3zcztpz: list[Entry[Label]]  # labels
    bag7bf25: list[Entry[Thread]]  # recovered_mail
    fezdozrd: list[str]  # recovered_conversations
    swilesl6: list[str]  # recovered_signatures
    _5upxgb6g: list[str]  # recovered_drafts
    _4ow5ydy5: int  # hold

    def set_hold(self, *, seconds: int) -> None:
        """Sets how many seconds a sent message waits before it goes, so it can be taken back: no wait at all at zero."""
        return self.action("set_hold", seconds=seconds)

    def set_hold_many(self, items: list[SetHoldArgs]) -> list[dict[str, Any]]:
        """Runs `set_hold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_hold", items)

    def recover_previous(self,
        *,
        drafts: list[list[str]] | None = None,
        labels: list[str] | None = None,
        conversations: list[list[list[str]]] | None = None,
        signatures: list[list[str]] | None = None,
    ) -> int:
        """Copies data from the earlier private mailbox into this workspace."""
        args: dict[str, Any] = {}
        if drafts is not None:
            args["drafts"] = drafts
        if labels is not None:
            args["labels"] = labels
        if conversations is not None:
            args["conversations"] = conversations
        if signatures is not None:
            args["signatures"] = signatures
        return self.action("recover_previous", **args)

    def new_draft(self) -> Draft:
        """Starts a message from the first account."""
        return self.action("new_draft")

    def rename_label(self,
        *,
        l: Label,
        name: str,
    ) -> None:
        """Renames one of the workspace's labels, all the conversations that carry it with it."""
        return self.action("rename_label", l=l, name=name)

    def rename_label_many(self, items: list[RenameLabelArgs]) -> list[dict[str, Any]]:
        """Runs `rename_label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_label", items)

    def delete_label(self, *, l: Label) -> None:
        """Deletes one of the workspace's labels, and takes it off every conversation that carries it."""
        return self.action("delete_label", l=l)

    def delete_label_many(self, items: list[DeleteLabelArgs]) -> list[dict[str, Any]]:
        """Runs `delete_label` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_label", items)

    def discard(self, *, d: Draft) -> None:
        """Throws a message being written away."""
        return self.action("discard", d=d)

    def discard_many(self, items: list[DiscardArgs]) -> list[dict[str, Any]]:
        """Runs `discard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("discard", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_hold", "set_hold_many", "recover_previous", "new_draft", "rename_label", "rename_label_many", "delete_label", "delete_label_many", "discard", "discard_many"})


class LangMail(Resource):
    """Mail — `tl.so.tilo.lang.mail`."""
    __surface__ = "so.tilo.lang.mail@75c79c95f9de22c7"

