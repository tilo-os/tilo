"""Types for Mail (`so.tilo.mail`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Account` is the app's `tl.so.tilo.mail.Account` at `mail/<resource>`, read with
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
    "TiloMail",
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
    "AttachArgs",
    "DetachArgs",
    "SendFromArgs",
    "SendAsArgs",
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

AttachArgs = TypedDict("AttachArgs", {
    "file": "Any",
})
"""The arguments of `attach`."""

DetachArgs = TypedDict("DetachArgs", {
    "name": "str",
})
"""The arguments of `detach`."""

SendFromArgs = TypedDict("SendFromArgs", {
    "a": "Account",
})
"""The arguments of `send_from`."""

SendAsArgs = TypedDict("SendAsArgs", {
    "identity": "str",
})
"""The arguments of `send_as`."""

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
    """Account — `tl.so.tilo.mail.Account`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    yujnud5c: str  # key
    @property
    def _4lo3ei5n(self) -> str:
        """address (`4lo3ei5n` on the wire)."""
        return self.fields["4lo3ei5n"]

    @_4lo3ei5n.setter
    def _4lo3ei5n(self, value: str) -> None:
        self.edit("4lo3ei5n", value=value)
    j6nqqrit: str  # name
    b5k7mvka: str  # trouble
    @property
    def _33f3rqw2(self) -> bool:
        """online (`33f3rqw2` on the wire)."""
        return self.fields["33f3rqw2"]

    @_33f3rqw2.setter
    def _33f3rqw2(self, value: bool) -> None:
        self.edit("33f3rqw2", value=value)
    hzbwof46: str  # fetched
    jxc5rufs: str  # signature
    yinmk5zb: str  # identities
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
    """Mailbox — `tl.so.tilo.mail.Mailbox`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    mh47w3p6: str  # key
    tlasysm5: str  # name
    xhiqdk3p: str  # role
    dtd4czyv: int  # unread
    rxoe5vzb: int  # total
    al2vjuet: bool  # more

    def older_messages(self) -> None:
        """Asks the account's server for the messages older than the oldest this workspace holds in the mailbox, so a folder longer than one page can be read back to its first message. They come in as new mail does, and the mailbox stops offering them once its first message is here. A mailbox whose server holds nothing older answers nothing, as a list reads to its end asking whether a page is there."""
        return self.action("older_messages")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"older_messages"})


class Thread(Resource):
    """Thread — `tl.so.tilo.mail.Thread`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    ecmpoary: str  # key
    zu23m2dm: str  # account
    u732lnuk: str  # subject
    o3knjgos: str  # latest
    sxcvmlon: str  # snippet
    vyc7aj7v: str  # senders
    izafodav: int  # count
    @property
    def _56lwowdg(self) -> bool:
        """unread (`56lwowdg` on the wire)."""
        return self.fields["56lwowdg"]

    @_56lwowdg.setter
    def _56lwowdg(self, value: bool) -> None:
        self.edit("56lwowdg", value=value)
    abe6c5jl: bool  # flagged
    @property
    def _3vusnp7i(self) -> list[str]:
        """boxes (`3vusnp7i` on the wire)."""
        return self.fields["3vusnp7i"]

    @_3vusnp7i.setter
    def _3vusnp7i(self, value: list[str]) -> None:
        self.edit("3vusnp7i", value=value)
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
        """Sends the conversation's newest message on, with its sender, what it says and the files it came with."""
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

    def purge(self) -> None:
        """Deletes the conversation's trashed messages for good, from its server too, and the conversation with them when no message of it is left. This is the one deletion there is no trash to take back from: what is deleted for good is gone from the server, and the platform takes back this app's record of it only."""
        return self.action("purge")

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
    __actions__ = frozenset({"reply", "reply_all", "forward", "archive", "archive_many", "to_inbox", "to_inbox_many", "move_to", "move_to_many", "delete", "delete_many", "purge", "flag", "flag_many", "snooze", "snooze_many", "snooze_an_hour", "snooze_an_hour_many", "snooze_until_tomorrow", "snooze_until_tomorrow_many", "unsnooze", "unsnooze_many", "label", "label_many", "mute", "mute_many", "mark_read", "mark_read_many", "answer"})


class Label(Resource):
    """Label — `tl.so.tilo.mail.Label`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    ammoua3h: str  # name


class Message(Resource):
    """Message — `tl.so.tilo.mail.Message`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    kzjq6hvc: str  # key
    kdxoo3c6: str  # account
    ntqgnz4f: str  # subject
    l3nc65iv: str  # from
    mo6l36a3: str  # from_name
    @property
    def _46k23iwq(self) -> str:
        """to (`46k23iwq` on the wire)."""
        return self.fields["46k23iwq"]

    @_46k23iwq.setter
    def _46k23iwq(self, value: str) -> None:
        self.edit("46k23iwq", value=value)
    bgzxugi5: str  # cc
    tfxwkfn2: str  # received
    sbrol5fc: str  # snippet
    @property
    def _4bchz3ch(self) -> str:
        """body (`4bchz3ch` on the wire)."""
        return self.fields["4bchz3ch"]

    @_4bchz3ch.setter
    def _4bchz3ch(self, value: str) -> None:
        self.edit("4bchz3ch", value=value)
    plb4mg2p: str  # html
    anuds3qt: str  # message_id
    asu6ymha: str  # references
    yltl32r3: list[Any]  # attachments
    ubfcf6ep: bool  # unread
    vxxb4sqr: bool  # flagged
    xlejsz5j: str  # mailbox
    hgysjvfy: str  # original
    wuefabwr: str  # parts

    def export_markdown(self) -> Any:
        """The message as a Markdown file: what a person exports to read it, or keep it, elsewhere."""
        return self.action("export_markdown")

    def export_json(self) -> Any:
        """The message as a JSON file: what a script or an agent reads its record from."""
        return self.action("export_json")

    def export_eml(self) -> Any:
        """The message as an `.eml` file, which every mail program opens. A message taken in from outside an account's server is exported as it was sent; one its server holds is written from what the workspace keeps of it."""
        return self.action("export_eml")

    def reply_to(self) -> Draft:
        """Answers one message of the conversation, to whoever wrote it, quoting it."""
        return self.action("reply_to")

    def reply_all_to(self) -> Draft:
        """Answers one message of the conversation, to everyone it went to but the person answering."""
        return self.action("reply_all_to")

    def forward_message(self) -> Draft:
        """Sends one message of the conversation on, with its sender, when and what it says, and the files it came with."""
        return self.action("forward_message")

    def attachment(self, *, name: str) -> Any:
        """One file the message came with, as a file of its own: what Save puts in Files, and what an agent reads or hands another app."""
        return self.action("attachment", name=name)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"export_markdown", "export_json", "export_eml", "reply_to", "reply_all_to", "forward_message", "attachment"})


class Draft(Resource):
    """Draft — `tl.so.tilo.mail.Draft`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    m2g6zrle: str  # from
    zx2b4ax2: str  # from_address
    hd3o4gkq: str  # to
    cwukhrvn: str  # cc
    ib44zkec: str  # bcc
    tyxmravh: str  # subject
    d5u2c7jm: str  # body
    w6twdkob: str  # in_reply_to
    w26tzvt4: str  # references
    yfyt2cfh: bool  # sent
    lgrwf5yf: str  # send_at
    h2nvr2op: bool  # recalled
    em6rrmae: str  # invitation
    @property
    def _2tq2zegc(self) -> list[Any]:
        """attachments (`2tq2zegc` on the wire)."""
        return self.fields["2tq2zegc"]

    @_2tq2zegc.setter
    def _2tq2zegc(self, value: list[Any]) -> None:
        self.edit("2tq2zegc", value=value)
    ozukvva2: str  # set_aside

    def attach(self, *, file: Any) -> None:
        """Takes a file along with the message: one the person picks, or one an agent hands over. A message takes up to sixteen files of up to 32 MB each, as mail servers take them."""
        return self.action("attach", file=file)

    def attach_many(self, items: list[AttachArgs]) -> list[dict[str, Any]]:
        """Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("attach", items)

    def detach(self, *, name: str) -> None:
        """Leaves a file the message was to take along behind."""
        return self.action("detach", name=name)

    def detach_many(self, items: list[DetachArgs]) -> list[dict[str, Any]]:
        """Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("detach", items)

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

    def send_as(self, *, identity: str) -> None:
        """Sends the message as another of its account's names or addresses, `Name <address>`: what a person writing as a role rather than as themselves chooses. Empty sends as the account's own."""
        return self.action("send_as", identity=identity)

    def send_as_many(self, items: list[SendAsArgs]) -> list[dict[str, Any]]:
        """Runs `send_as` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send_as", items)

    def send(self) -> None:
        """Sends the message from its account. It leaves the system, so it goes once, and waits in the outbox until the server took it; it is held back for the person's hold first, so they can take it back. The effect is an argument the action records, so the wait survives a window closing and an agent takes it back the same way."""
        return self.action("send")

    def undo_send(self) -> None:
        """Takes back a message that was sent and has not gone yet: it never leaves, and the message is a draft again, as it was. Once it went it cannot be taken back."""
        return self.action("undo_send")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"attach", "attach_many", "detach", "detach_many", "cancel_later", "send_later", "send_in_the_morning", "send_from", "send_from_many", "send_as", "send_as_many", "send", "undo_send"})


class Mail(Resource):
    """Mail — `tl.so.tilo.mail.Mail`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"
    blig7xcq: list[Entry[Account]]  # accounts
    gkuo27e6: list[Entry[Draft]]  # drafts
    z3zcztpz: list[Entry[Label]]  # labels
    bag7bf25: list[Entry[Thread]]  # recovered_mail
    @property
    def _4ow5ydy5(self) -> int:
        """hold (`4ow5ydy5` on the wire)."""
        return self.fields["4ow5ydy5"]

    @_4ow5ydy5.setter
    def _4ow5ydy5(self, value: int) -> None:
        self.edit("4ow5ydy5", value=value)

    def set_hold(self, *, seconds: int) -> None:
        """Sets how many seconds a sent message waits before it goes, so it can be taken back: no wait at all at zero."""
        return self.action("set_hold", seconds=seconds)

    def set_hold_many(self, items: list[SetHoldArgs]) -> list[dict[str, Any]]:
        """Runs `set_hold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_hold", items)

    def new_draft(self) -> Draft:
        """Starts a message from the first account."""
        return self.action("new_draft")

    def draft_message(self,
        *,
        to: str,
        subject: str | None = None,
        body: str | None = None,
        cc: str | None = None,
        in_reply_to: str | None = None,
        from_: str | None = None,
    ) -> Draft:
        """Writes a message for the person to read and send: to whom, its subject and what it says, answering `in_reply_to` (a Message-ID) when it names one. Every call makes a draft of its own beside the others; nothing is written over and nothing is sent. Where an agent drafts a message, the person finds it in Drafts and sends it."""
        args: dict[str, Any] = {}
        args["to"] = to
        if subject is not None:
            args["subject"] = subject
        if body is not None:
            args["body"] = body
        if cc is not None:
            args["cc"] = cc
        if in_reply_to is not None:
            args["in_reply_to"] = in_reply_to
        if from_ is not None:
            args["from"] = from_
        return self.action("draft_message", **args)

    def prepare_calendar_invitation(self,
        *,
        to: str,
        subject: str,
        body: str,
        calendar_ics: str,
    ) -> Draft:
        """Starts a message inviting `to` to an event, its invitation (`calendar_ics`, iCalendar) carried beside what it says: what Calendar hands Mail for each guest it invites. Nothing is sent; the person reads it, and sends it."""
        return self.action("prepare_calendar_invitation", to=to, subject=subject, body=body, calendar_ics=calendar_ics)

    def deliver_message(self,
        *,
        from_: str,
        subject: str | None = None,
        body: str | None = None,
        folder: str | None = None,
        read: bool | None = None,
        to: str | None = None,
        cc: str | None = None,
        date: str | None = None,
        conversation: str | None = None,
        message_id: str | None = None,
        original: str | None = None,
        parts: list[str] | None = None,
        attachments: list[Any] | None = None,
    ) -> Message:
        """Takes in a message that reached the workspace from outside its accounts' servers: one moved in from another mail program (`tilo migrate`), or handed over through the API. It is kept with the mail held outside the accounts, in the conversation it belongs to (`conversation`, the Message-ID the conversation began with) and the folder it was in, read or not. `from` is the sender as a header writes it (`Ada Lovelace <ada@example.com>`), and `original` the content reference of the message as it was sent, less its large parts: `parts` says where each of those stands in it and which pieces of content it is in, one JSON text each (`{'at': 120, 'size': 4096, 'pieces': ['tilo.content:…']}`). `attachments` are the files it carries, each content the workspace holds. A message already held under its Message-ID is answered as it is, so delivering it again keeps it once."""
        args: dict[str, Any] = {}
        args["from"] = from_
        if subject is not None:
            args["subject"] = subject
        if body is not None:
            args["body"] = body
        if folder is not None:
            args["folder"] = folder
        if read is not None:
            args["read"] = read
        if to is not None:
            args["to"] = to
        if cc is not None:
            args["cc"] = cc
        if date is not None:
            args["date"] = date
        if conversation is not None:
            args["conversation"] = conversation
        if message_id is not None:
            args["message_id"] = message_id
        if original is not None:
            args["original"] = original
        if parts is not None:
            args["parts"] = parts
        if attachments is not None:
            args["attachments"] = attachments
        return self.action("deliver_message", **args)

    def open_message_file(self, *, file: Any) -> Message:
        """Opens a message kept as a file a mail program writes (`.eml`, a single message of an mbox, or Apple Mail's `.emlx`): its headers are read, and it is kept with the mail held outside the accounts, read, in the inbox. The same file opened again is the message already kept."""
        return self.action("open_message_file", file=file)

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
    __actions__ = frozenset({"set_hold", "set_hold_many", "new_draft", "draft_message", "prepare_calendar_invitation", "deliver_message", "open_message_file", "rename_label", "rename_label_many", "delete_label", "delete_label_many", "discard", "discard_many"})


class TiloMail(Resource):
    """Mail — `tl.so.tilo.mail`."""
    __surface__ = "so.tilo.mail@33ae2193476d972e"

