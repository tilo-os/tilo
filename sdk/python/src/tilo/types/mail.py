"""Types for Mail (`so.tilo.mail`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `mail/<resource>`, read with
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
    "Notice",
    "Account",
    "Mailbox",
    "Thread",
    "Label",
    "Message",
    "Picture",
    "TrustedSender",
    "Draft",
    "Mail",
    "TiloMail",
    "SetSignatureArgs",
    "SnoozeArgs",
    "SnoozeAnHourArgs",
    "SnoozeUntilTomorrowArgs",
    "UnsnoozeArgs",
    "LabelArgs",
    "MuteArgs",
    "AnnounceArgs",
    "ShowPicturesArgs",
    "AttachArgs",
    "DetachArgs",
    "SendFromArgs",
    "SendAsArgs",
    "LeaveArgs",
    "TrustSenderArgs",
    "DistrustSenderArgs",
    "SetHoldArgs",
    "RenameLabelArgs",
    "DeleteLabelArgs",
    "DiscardArgs",
]

SetSignatureArgs = TypedDict("SetSignatureArgs", {
    "signature": "str",
})
"""The arguments of `set_signature`."""

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

AnnounceArgs = TypedDict("AnnounceArgs", {
})
"""The arguments of `announce`."""

ShowPicturesArgs = TypedDict("ShowPicturesArgs", {
})
"""The arguments of `show_pictures`."""

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

LeaveArgs = TypedDict("LeaveArgs", {
})
"""The arguments of `leave`."""

TrustSenderArgs = TypedDict("TrustSenderArgs", {
    "address": "str",
})
"""The arguments of `trust_sender`."""

DistrustSenderArgs = TypedDict("DistrustSenderArgs", {
    "address": "str",
})
"""The arguments of `distrust_sender`."""

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

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
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


class Account(Resource):
    """Account — `tl.so.tilo.mail.Account`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
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
    z3itgb5e: str  # trouble_kind
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

    def search_server(self, *, text: str) -> None:
        """Asks the account's server to search its mail for `text`: what finds mail the workspace does not hold, and mail in folders it does not sync. What it finds comes in as the account's conversations, as a fetch's mail does, so it is read and searched here from then on."""
        return self.action("search_server", text=text)

    def new_message(self) -> Draft:
        """Starts a message from the account."""
        return self.action("new_message")

    def empty_trash(self) -> None:
        """Deletes every conversation in the account's trash from the mailbox and its server for good, each as `purge` does. A view offers this behind a confirmation, as nothing undoes it."""
        return self.action("empty_trash")

    def mark_all_read(self) -> None:
        """Marks every conversation of the account read."""
        return self.action("mark_all_read")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_signature", "set_signature_many", "check_now", "search_server", "new_message", "empty_trash", "mark_all_read"})


class Mailbox(Resource):
    """Mailbox — `tl.so.tilo.mail.Mailbox`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
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
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
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
    ypxqazxd: str | None  # arrived

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

    def to_inbox(self) -> None:
        """Moves the conversation back to the inbox, and unmutes it: what a person brings back is what they want to see again."""
        return self.action("to_inbox")

    def move_to(self, *, to: Mailbox) -> None:
        """Moves the conversation to another mailbox of its account."""
        return self.action("move_to", to=to)

    def delete(self) -> None:
        """Deletes the conversation: its messages go to the trash."""
        return self.action("delete")

    def junk(self) -> None:
        """Marks the conversation as junk: its messages leave the inbox for the account's junk mailbox, and what comes of it again stays there."""
        return self.action("junk")

    def purge(self) -> None:
        """Deletes the conversation's trashed messages for good, from its server too, and the conversation with them when no message of it is left. This is the one deletion there is no trash to take back from: what is deleted for good is gone from the server, and this app's record of it leaves once the server says it is."""
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

    def announce(self) -> None:
        """Tells everyone who reads the conversation that mail came to it: who wrote, what it is called, and which account it came to while the workspace reads several of them, opening the conversation. It is run once for each arrival, on the device that hosts the workspace, so each person is told once however many of their devices are open."""
        return self.action("announce")

    def announce_many(self, items: list[AnnounceArgs]) -> list[dict[str, Any]]:
        """Runs `announce` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("announce", items)

    def answer(self, *, body: str) -> Draft:
        """Answers the conversation with what was typed: the message goes to whoever wrote its newest, as a reply to it, quoted."""
        return self.action("answer", body=body)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reply", "reply_all", "forward", "archive", "to_inbox", "move_to", "delete", "junk", "purge", "flag", "snooze", "snooze_many", "snooze_an_hour", "snooze_an_hour_many", "snooze_until_tomorrow", "snooze_until_tomorrow_many", "unsnooze", "unsnooze_many", "label", "label_many", "mute", "mute_many", "mark_read", "announce", "announce_many", "answer"})


class Label(Resource):
    """Label — `tl.so.tilo.mail.Label`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
    ammoua3h: str  # name


class Message(Resource):
    """Message — `tl.so.tilo.mail.Message`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
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
    c263r6w3: list[Entry[Picture]]  # pictures

    def show_pictures(self) -> None:
        """Brings in the pictures the sender's page names: each read from where it stands and kept in the workspace's content, so the message shows them as it was written. What a workspace's rules or its network refuse is left out, and the message reads without it."""
        return self.action("show_pictures")

    def show_pictures_many(self, items: list[ShowPicturesArgs]) -> list[dict[str, Any]]:
        """Runs `show_pictures` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_pictures", items)

    def links(self) -> list[str]:
        """The links a message says, each once, in the order it says them: what its formatted part links to, or the web and mail addresses its words carry. A person reads them, copies one, or an agent asks what a message would have one open."""
        return self.action("links")

    def open_link(self, *, url: str) -> None:
        """Opens one of a message's links in the app that opens links of its scheme, as pressing a link does: the Browser for a web address. A message opens only a link it says."""
        return self.action("open_link", url=url)

    def copy_link(self, *, url: str) -> None:
        """Copies one of a message's links, the address alone, as a person hands it to someone."""
        return self.action("copy_link", url=url)

    def export_markdown(self) -> Any:
        """The message as a Markdown file: what a person exports to read it, or keep it, elsewhere. The system's one Export… lists it with the message's other formats."""
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

    def open_attachment(self, *, name: str) -> None:
        """Opens a file a message came with in the app that opens its kind, as Files opens one: the file reaches that app as the same content both read, never as a path neither has."""
        return self.action("open_attachment", name=name)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"show_pictures", "show_pictures_many", "links", "open_link", "copy_link", "export_markdown", "export_json", "export_eml", "reply_to", "reply_all_to", "forward_message", "attachment", "open_attachment"})


class Picture(Resource):
    """Picture — `tl.so.tilo.mail.Picture`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
    gb5hqtag: str  # source
    @property
    def _7hhcy4ay(self) -> Any:
        """picture (`7hhcy4ay` on the wire)."""
        return self.fields["7hhcy4ay"]

    @_7hhcy4ay.setter
    def _7hhcy4ay(self, value: Any) -> None:
        self.edit("7hhcy4ay", value=value)


class TrustedSender(Resource):
    """TrustedSender — `tl.so.tilo.mail.TrustedSender`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
    @property
    def _5eoon66w(self) -> str:
        """address (`5eoon66w` on the wire)."""
        return self.fields["5eoon66w"]

    @_5eoon66w.setter
    def _5eoon66w(self, value: str) -> None:
        self.edit("5eoon66w", value=value)


class Draft(Resource):
    """Draft — `tl.so.tilo.mail.Draft`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
    m2g6zrle: str  # from
    zx2b4ax2: str  # from_address
    hd3o4gkq: str  # to
    cwukhrvn: str  # cc
    ib44zkec: str  # bcc
    tyxmravh: str  # subject
    d5u2c7jm: str  # body
    w6twdkob: str  # in_reply_to
    w26tzvt4: str  # references
    ualhak62: str  # conversation
    yfyt2cfh: bool  # sent
    lgrwf5yf: str  # send_at
    wgp3brx4: str | None  # leaves_at
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

    def retry(self) -> None:
        """Sends a message the account's server refused again, from where it stands: the version that failed did not leave, so it goes once. A message whose outcome nobody knows is not tried again, since it may have gone."""
        return self.action("retry")

    def send(self) -> None:
        """Sends the message from its account. It leaves the system, so it goes once, and waits in the outbox until the server took it; it is held back for the person's hold first, so they can take it back. The effect is an argument the action records, so the wait survives a window closing and an agent takes it back the same way. Whoever is asked to let it go reads who it goes to and what it says."""
        return self.action("send")

    def undo_send(self) -> None:
        """Takes back a message that was sent and has not gone yet: it never leaves, and the message is a draft again, as it was. Once it went it cannot be taken back."""
        return self.action("undo_send")

    def leave(self) -> None:
        """Lets a message that waited in the outbox leave: its hold or the moment it was sent for came, so it can no longer be taken back, and every member reads it from now on. One taken back before its moment stays a draft."""
        return self.action("leave")

    def leave_many(self, items: list[LeaveArgs]) -> list[dict[str, Any]]:
        """Runs `leave` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("leave", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"attach", "attach_many", "detach", "detach_many", "cancel_later", "send_later", "send_in_the_morning", "send_from", "send_from_many", "send_as", "send_as_many", "retry", "send", "undo_send", "leave", "leave_many"})


class Mail(Resource):
    """Mail — `tl.so.tilo.mail.Mail`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"
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
    zzpnzlp2: list[Entry[TrustedSender]]  # trusted

    def trust_sender(self, *, address: str) -> None:
        """Shows the pictures of every message `address` sends, from now on and in what is already here."""
        return self.action("trust_sender", address=address)

    def trust_sender_many(self, items: list[TrustSenderArgs]) -> list[dict[str, Any]]:
        """Runs `trust_sender` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trust_sender", items)

    def distrust_sender(self, *, address: str) -> None:
        """Asks again before showing what `address` sends."""
        return self.action("distrust_sender", address=address)

    def distrust_sender_many(self, items: list[DistrustSenderArgs]) -> list[dict[str, Any]]:
        """Runs `distrust_sender` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("distrust_sender", items)

    def set_hold(self, *, seconds: int) -> None:
        """Sets how many seconds a sent message waits before it goes, so it can be taken back: no wait at all at zero."""
        return self.action("set_hold", seconds=seconds)

    def set_hold_many(self, items: list[SetHoldArgs]) -> list[dict[str, Any]]:
        """Runs `set_hold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_hold", items)

    def read_more(self, *, query: str) -> None:
        """Reads the page before the rows of the place `query`: what a list does as the person reaches its end, whichever accounts the window reads. The account whose server holds more answers; a place that is no mailbox, or whose mail is all here, has no page before it and asks nothing."""
        return self.action("read_more", query=query)

    def new_draft(self) -> Draft:
        """Starts a message. A message is written before the workspace has an account to send it from: it waits in Drafts, and goes once the person adds one."""
        return self.action("new_draft")

    def open_link(self, *, link: str) -> Draft | None:
        """Opens a `mailto:` link: a message to whom it names, in Drafts, for the person to read and send. A link that names no address opens nothing, as it names nobody to write to. The host publishes this as the system's `open-link` on the mailbox, which is what a shell hands a link to."""
        return self.action("open_link", link=link)

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

    def save_to_drafts(self, *, d: Draft) -> None:
        """Keeps the message in the account's server Drafts folder as it is written now, so another mail client of the account sees it there too. What the server says comes back with the account's next mail, and the server's Drafts are read like its other folders."""
        return self.action("save_to_drafts", d=d)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"trust_sender", "trust_sender_many", "distrust_sender", "distrust_sender_many", "set_hold", "set_hold_many", "read_more", "new_draft", "open_link", "draft_message", "prepare_calendar_invitation", "deliver_message", "open_message_file", "rename_label", "rename_label_many", "delete_label", "delete_label_many", "discard", "discard_many", "save_to_drafts"})


class TiloMail(Resource):
    """Mail — `tl.so.tilo.mail`."""
    __surface__ = "so.tilo.mail@3d8d68c3ca9b0145"

