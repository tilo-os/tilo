"""Types for Chat (`so.tilo.chat`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `chat/<resource>`, read with
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
    "Emoji",
    "Decision",
    "Outcome",
    "Report",
    "Reverted",
    "Ruling",
    "Notice",
    "Conversations",
    "Category",
    "VoiceChannel",
    "TextChannel",
    "OutsideAuthor",
    "OutsideEmoji",
    "Message",
    "ChatReport",
    "Thread",
    "Direct",
    "Draft",
    "Attached",
    "RelayedReaction",
    "Part",
    "Skill",
    "Example",
    "Chat",
    "WorkspaceEmoji",
    "Moderation",
    "DeleteChannelArgs",
    "DeleteCategoryArgs",
    "MoveChannelArgs",
    "MoveCategoryArgs",
    "BridgeArgs",
    "UnbridgeArgs",
    "CategoryDeleteChannelArgs",
    "RenameCategoryArgs",
    "VoiceChannelBridgeArgs",
    "VoiceChannelUnbridgeArgs",
    "RenameVoiceChannelArgs",
    "TextChannelBridgeArgs",
    "TextChannelUnbridgeArgs",
    "SummarizeArgs",
    "RenameChannelArgs",
    "SetTopicArgs",
    "ArchiveChannelArgs",
    "DeleteArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "RelayReactionArgs",
    "PinArgs",
    "ReactArgs",
    "DecideArgs",
    "OverturnArgs",
    "ThreadSummarizeArgs",
    "ThreadDeleteArgs",
    "DirectBridgeArgs",
    "DirectUnbridgeArgs",
    "DirectSummarizeArgs",
    "DirectDeleteArgs",
    "DraftEditBlockArgs",
    "DraftInsertBlockArgs",
    "DraftDeleteBlockArgs",
    "AttachArgs",
    "DetachArgs",
    "CorrectSkillArgs",
    "ForgetSkillArgs",
    "KeepExampleArgs",
    "DropExampleArgs",
    "AddEmojiArgs",
    "RemoveEmojiArgs",
    "RemoveAnyEmojiArgs",
    "ReportMessageArgs",
    "ReportPersonArgs",
    "DecideReportsArgs",
    "OverturnDecisionArgs",
]

class Emoji(TypedDict):
    """One emoji of the workspace."""
    image: Any  # Its picture
    added_by: str  # Who added it: they may remove it again
    added_at: int  # When, in milliseconds since the Unix epoch

class Decision(TypedDict):
    """One decision, with the reasoning that led to it."""
    id: str  # Id
    person: str  # Person
    reports: list[str]  # The reports it settles
    outcome: Outcome  # Outcome
    until_ms: int | None  # When a restriction lapses
    reasoning: str  # The model's reasoning, or the Moderator's, in their words
    model: str  # The model that judged, empty for a person's own decision
    reverted: list[Reverted]  # What it took back
    by: str  # Who decided: the agent or the Moderator
    at_ms: int  # At ms
    overturned_by: str | None  # Overturned by
    overturn_note: str | None  # Overturn note
    overturned_at_ms: int | None  # Overturned at ms

Outcome = Literal["no-action", "restrict", "ban"]
"""What a decision does to the person."""

class Report(TypedDict):
    """One report of a person, optionally pointing at what they did."""
    id: str  # Id
    reporter: str  # Who reported; the moderation agent for its own heuristics
    person: str  # Whom the report is about
    app: str | None  # The app, resource and item reported, when the report is about content
    record: str | None  # Record
    item: str | None  # Item
    reason: str  # Reason
    at_ms: int  # At ms
    decision: str | None  # The decision that settled it

class Reverted(TypedDict):
    """A resource whose changes by the person a decision took back, and the run that did it, so overturning can take the take-back back."""
    app: str  # App
    record: str  # Record
    run: str  # The history run the moderation agent's undo ran in
    changes: int  # How many of the person's changes it took back

class Ruling(TypedDict):
    """What was decided about a person, and why."""
    outcome: Outcome
    until_ms: int | None
    reasoning: str
    model: str
    reverted: list[Reverted]

DeleteChannelArgs = TypedDict("DeleteChannelArgs", {
    "ch": "TextChannel | VoiceChannel",
})
"""The arguments of `delete_channel`."""

DeleteCategoryArgs = TypedDict("DeleteCategoryArgs", {
    "c": "Category",
})
"""The arguments of `delete_category`."""

MoveChannelArgs = TypedDict("MoveChannelArgs", {
    "ch": "TextChannel | VoiceChannel",
    "category": NotRequired["Category | None"],
    "at": NotRequired["int | None"],
})
"""The arguments of `move_channel`."""

MoveCategoryArgs = TypedDict("MoveCategoryArgs", {
    "c": "Category",
    "at": NotRequired["int | None"],
})
"""The arguments of `move_category`."""

BridgeArgs = TypedDict("BridgeArgs", {
    "service": "str",
    "outside_id": NotRequired["str | None"],
})
"""The arguments of `bridge`."""

UnbridgeArgs = TypedDict("UnbridgeArgs", {
})
"""The arguments of `unbridge`."""

CategoryDeleteChannelArgs = TypedDict("CategoryDeleteChannelArgs", {
    "ch": "TextChannel | VoiceChannel",
})
"""The arguments of `delete_channel`."""

RenameCategoryArgs = TypedDict("RenameCategoryArgs", {
    "name": "str",
})
"""The arguments of `rename_category`."""

VoiceChannelBridgeArgs = TypedDict("VoiceChannelBridgeArgs", {
    "service": "str",
    "outside_id": NotRequired["str | None"],
})
"""The arguments of `bridge`."""

VoiceChannelUnbridgeArgs = TypedDict("VoiceChannelUnbridgeArgs", {
})
"""The arguments of `unbridge`."""

RenameVoiceChannelArgs = TypedDict("RenameVoiceChannelArgs", {
    "name": "str",
})
"""The arguments of `rename_voice_channel`."""

TextChannelBridgeArgs = TypedDict("TextChannelBridgeArgs", {
    "service": "str",
    "outside_id": NotRequired["str | None"],
})
"""The arguments of `bridge`."""

TextChannelUnbridgeArgs = TypedDict("TextChannelUnbridgeArgs", {
})
"""The arguments of `unbridge`."""

SummarizeArgs = TypedDict("SummarizeArgs", {
    "summary": "str",
})
"""The arguments of `summarize`."""

RenameChannelArgs = TypedDict("RenameChannelArgs", {
    "name": "str",
})
"""The arguments of `rename_channel`."""

SetTopicArgs = TypedDict("SetTopicArgs", {
    "topic": "str",
})
"""The arguments of `set_topic`."""

ArchiveChannelArgs = TypedDict("ArchiveChannelArgs", {
    "archived": NotRequired["bool | None"],
})
"""The arguments of `archive_channel`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "m": "Message",
})
"""The arguments of `delete`."""

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

RelayReactionArgs = TypedDict("RelayReactionArgs", {
    "emoji": "str",
    "persona": "str",
    "by": "str",
    "given": NotRequired["bool | None"],
})
"""The arguments of `relay_reaction`."""

PinArgs = TypedDict("PinArgs", {
    "pinned": NotRequired["bool | None"],
})
"""The arguments of `pin`."""

ReactArgs = TypedDict("ReactArgs", {
    "emoji": "str",
})
"""The arguments of `react`."""

DecideArgs = TypedDict("DecideArgs", {
    "decision": "str",
})
"""The arguments of `decide`."""

OverturnArgs = TypedDict("OverturnArgs", {
    "why": "str",
})
"""The arguments of `overturn`."""

ThreadSummarizeArgs = TypedDict("ThreadSummarizeArgs", {
    "summary": "str",
})
"""The arguments of `summarize`."""

ThreadDeleteArgs = TypedDict("ThreadDeleteArgs", {
    "m": "Message",
})
"""The arguments of `delete`."""

DirectBridgeArgs = TypedDict("DirectBridgeArgs", {
    "service": "str",
    "outside_id": NotRequired["str | None"],
})
"""The arguments of `bridge`."""

DirectUnbridgeArgs = TypedDict("DirectUnbridgeArgs", {
})
"""The arguments of `unbridge`."""

DirectSummarizeArgs = TypedDict("DirectSummarizeArgs", {
    "summary": "str",
})
"""The arguments of `summarize`."""

DirectDeleteArgs = TypedDict("DirectDeleteArgs", {
    "m": "Message",
})
"""The arguments of `delete`."""

DraftEditBlockArgs = TypedDict("DraftEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

DraftInsertBlockArgs = TypedDict("DraftInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

DraftDeleteBlockArgs = TypedDict("DraftDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

AttachArgs = TypedDict("AttachArgs", {
    "files": "list[Any]",
})
"""The arguments of `attach`."""

DetachArgs = TypedDict("DetachArgs", {
    "file": "Any",
})
"""The arguments of `detach`."""

CorrectSkillArgs = TypedDict("CorrectSkillArgs", {
    "name": NotRequired["str | None"],
    "work": NotRequired["str | None"],
    "how": NotRequired["str | None"],
    "form": NotRequired["str | None"],
    "model": NotRequired["str | None"],
})
"""The arguments of `correct_skill`."""

ForgetSkillArgs = TypedDict("ForgetSkillArgs", {
})
"""The arguments of `forget_skill`."""

KeepExampleArgs = TypedDict("KeepExampleArgs", {
    "text": "str",
    "accepted": "bool",
    "note": NotRequired["str | None"],
})
"""The arguments of `keep_example`."""

DropExampleArgs = TypedDict("DropExampleArgs", {
    "example": "Example",
})
"""The arguments of `drop_example`."""

AddEmojiArgs = TypedDict("AddEmojiArgs", {
    "name": "str",
    "image": "str",
})
"""The arguments of `add-emoji`."""

RemoveEmojiArgs = TypedDict("RemoveEmojiArgs", {
    "name": "str",
})
"""The arguments of `remove-emoji`."""

RemoveAnyEmojiArgs = TypedDict("RemoveAnyEmojiArgs", {
    "name": "str",
})
"""The arguments of `remove-any-emoji`."""

ReportMessageArgs = TypedDict("ReportMessageArgs", {
    "message": "str",
    "author": "Any",
    "reason": "str",
})
"""The arguments of `report-message`."""

ReportPersonArgs = TypedDict("ReportPersonArgs", {
    "person": "Any",
    "reason": "str",
})
"""The arguments of `report-person`."""

DecideReportsArgs = TypedDict("DecideReportsArgs", {
    "id": "str",
    "person": "Any",
    "reports": "list[str]",
    "ruling": "Ruling",
})
"""The arguments of `decide-reports`."""

OverturnDecisionArgs = TypedDict("OverturnDecisionArgs", {
    "decision": "str",
    "note": "str",
})
"""The arguments of `overturn-decision`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
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


class Conversations(Resource):
    """Conversations — `tl.so.tilo.chat.Conversations`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    @property
    def _5kzdptyg(self) -> list[Entry[Category | Direct | TextChannel | VoiceChannel]]:
        """channels (`5kzdptyg` on the wire)."""
        return self.fields["5kzdptyg"]

    @_5kzdptyg.setter
    def _5kzdptyg(self, value: list[Entry[Category | Direct | TextChannel | VoiceChannel]]) -> None:
        self.edit("5kzdptyg", value=value)
    n2xgh234: list[Entry[Skill]]  # skills
    @property
    def _64vv225r(self) -> list[Entry[Draft]]:
        """drafts (`64vv225r` on the wire)."""
        return self.fields["64vv225r"]

    @_64vv225r.setter
    def _64vv225r(self, value: list[Entry[Draft]]) -> None:
        self.edit("64vv225r", value=value)
    @property
    def _7spkrvju(self) -> list[Entry[ChatReport]]:
        """reports (`7spkrvju` on the wire)."""
        return self.fields["7spkrvju"]

    @_7spkrvju.setter
    def _7spkrvju(self, value: list[Entry[ChatReport]]) -> None:
        self.edit("7spkrvju", value=value)

    def new_channel(self,
        *,
        name: str | None = None,
        topic: str | None = None,
        category: Category | None = None,
    ) -> TextChannel:
        """Adds a text channel to the sidebar, under `category` when it is given and about `topic` when that is."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if topic is not None:
            args["topic"] = topic
        if category is not None:
            args["category"] = category
        return self.action("new_channel", **args)

    def new_category(self,
        *,
        name: str | None = None,
    ) -> Category:
        """Adds a category to the sidebar."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_category", **args)

    def new_voice_channel(self,
        *,
        name: str | None = None,
        category: Category | None = None,
    ) -> VoiceChannel:
        """Adds a voice channel to the sidebar, under `category` when it is given."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if category is not None:
            args["category"] = category
        return self.action("new_voice_channel", **args)

    def new_skill(self,
        *,
        name: str | None = None,
    ) -> Skill:
        """Starts a skill, called `name`. What it is for and how its work is done are written on its page, or by an agent told to share the guidance with the workspace."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_skill", **args)

    def report_problem(self, *, body: str) -> Message:
        """Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens. The people who read the reports there answer, react and start a thread under one as under any message. The channel is made under one key on every device, so reports filed apart open one."""
        return self.action("report_problem", body=body)

    def message_person(self, *, who: str) -> Direct:
        """Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time. With yourself it is your notes to self. A person is someone of the workspace: one who is not in it is refused, since no one but its members reads what is written here."""
        return self.action("message_person", who=who)

    def conversation_with(self, *, who: str) -> Direct | None:
        """The direct conversation between the invoker and `who`, none while the two of them have none: what an agent, a script or another app reads to find it, opening and making nothing. With yourself it is your notes to self."""
        return self.action("conversation_with", who=who)

    def write_to(self,
        *,
        who: str,
        body: str,
    ) -> Message:
        """Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar. The message is sent as every message is."""
        return self.action("write_to", who=who, body=body)

    def delete_channel(self, *, ch: TextChannel | VoiceChannel) -> None:
        """Deletes a channel and what was said in it, from the sidebar or the category it is in."""
        return self.action("delete_channel", ch=ch)

    def delete_channel_many(self, items: list[DeleteChannelArgs]) -> list[dict[str, Any]]:
        """Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_channel", items)

    def delete_category(self, *, c: Category) -> None:
        """Deletes a category that holds no channel."""
        return self.action("delete_category", c=c)

    def delete_category_many(self, items: list[DeleteCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `delete_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_category", items)

    def move_channel(self,
        *,
        ch: TextChannel | VoiceChannel,
        category: Category | None = None,
        at: int | None = None,
    ) -> None:
        """Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among the channels listed there (the end when it is negative). A move keeps the channel and what was written in it."""
        args: dict[str, Any] = {}
        args["ch"] = ch
        if category is not None:
            args["category"] = category
        if at is not None:
            args["at"] = at
        return self.action("move_channel", **args)

    def move_channel_many(self, items: list[MoveChannelArgs]) -> list[dict[str, Any]]:
        """Runs `move_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_channel", items)

    def move_category(self,
        *,
        c: Category,
        at: int | None = None,
    ) -> None:
        """Moves a category to a place in the sidebar, at `at` among the categories and channels listed there (the end when it is negative). Its own channels move with it."""
        args: dict[str, Any] = {}
        args["c"] = c
        if at is not None:
            args["at"] = at
        return self.action("move_category", **args)

    def move_category_many(self, items: list[MoveCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `move_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_category", items)

    def report_person(self,
        *,
        who: str,
        reason: str,
    ) -> ChatReport:
        """Reports a person to the workspace's moderators, for how they behave here rather than for one message: what the reporter says is the whole of it, and the moderators decide it as any other report."""
        return self.action("report_person", who=who, reason=reason)

    def ask_for_sub_agent(self,
        *,
        agent: Any,
        job: str | None = None,
    ) -> Message | None:
        """Asks the assistant to start a sub-agent for `job`, in the direct conversation it answers in: the record's own `start-sub-agent` is the assistant's to call there, and the sub-agent reports in that conversation, where the person follows it. None where the workspace keeps no conversation with the assistant yet."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if job is not None:
            args["job"] = job
        return self.action("ask_for_sub_agent", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_channel", "new_category", "new_voice_channel", "new_skill", "report_problem", "message_person", "conversation_with", "write_to", "delete_channel", "delete_channel_many", "delete_category", "delete_category_many", "move_channel", "move_channel_many", "move_category", "move_category_many", "report_person", "ask_for_sub_agent"})


class Category(Resource):
    """Category — `tl.so.tilo.chat.Category`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    hbhrds5c: str  # name
    hvqgmwrk: list[Entry[TextChannel | VoiceChannel]]  # channels
    @property
    def _5pfbdqqj(self) -> str:
        """service (`5pfbdqqj` on the wire)."""
        return self.fields["5pfbdqqj"]

    @_5pfbdqqj.setter
    def _5pfbdqqj(self, value: str) -> None:
        self.edit("5pfbdqqj", value=value)
    vvs4ecqz: str  # outside_id

    def bridge(self,
        *,
        service: str,
        outside_id: str | None = None,
    ) -> None:
        """How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in."""
        args: dict[str, Any] = {}
        args["service"] = service
        if outside_id is not None:
            args["outside_id"] = outside_id
        return self.action("bridge", **args)

    def bridge_many(self, items: list[BridgeArgs]) -> list[dict[str, Any]]:
        """Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bridge", items)

    def unbridge(self) -> None:
        """Keeps a conversation here: no service carries it any more."""
        return self.action("unbridge")

    def unbridge_many(self, items: list[UnbridgeArgs]) -> list[dict[str, Any]]:
        """Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unbridge", items)

    def delete_channel(self, *, ch: TextChannel | VoiceChannel) -> None:
        """Deletes a channel and what was said in it, from the sidebar or the category it is in."""
        return self.action("delete_channel", ch=ch)

    def delete_channel_many(self, items: list[CategoryDeleteChannelArgs]) -> list[dict[str, Any]]:
        """Runs `delete_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_channel", items)

    def rename_category(self, *, name: str) -> None:
        """Renames a category. Its channels stay in it."""
        return self.action("rename_category", name=name)

    def rename_category_many(self, items: list[RenameCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `rename_category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_category", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"bridge", "bridge_many", "unbridge", "unbridge_many", "delete_channel", "delete_channel_many", "rename_category", "rename_category_many"})


class VoiceChannel(Resource):
    """VoiceChannel — `tl.so.tilo.chat.VoiceChannel`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    olqjezxu: str  # name
    enzjdbyp: str  # service
    nvzanb5n: str  # outside_id

    def bridge(self,
        *,
        service: str,
        outside_id: str | None = None,
    ) -> None:
        """How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in."""
        args: dict[str, Any] = {}
        args["service"] = service
        if outside_id is not None:
            args["outside_id"] = outside_id
        return self.action("bridge", **args)

    def bridge_many(self, items: list[VoiceChannelBridgeArgs]) -> list[dict[str, Any]]:
        """Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bridge", items)

    def unbridge(self) -> None:
        """Keeps a conversation here: no service carries it any more."""
        return self.action("unbridge")

    def unbridge_many(self, items: list[VoiceChannelUnbridgeArgs]) -> list[dict[str, Any]]:
        """Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unbridge", items)

    def rename_voice_channel(self, *, name: str) -> None:
        """Renames a voice channel. A call in it is untouched."""
        return self.action("rename_voice_channel", name=name)

    def rename_voice_channel_many(self, items: list[RenameVoiceChannelArgs]) -> list[dict[str, Any]]:
        """Runs `rename_voice_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_voice_channel", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"bridge", "bridge_many", "unbridge", "unbridge_many", "rename_voice_channel", "rename_voice_channel_many"})


class TextChannel(Resource):
    """TextChannel — `tl.so.tilo.chat.TextChannel`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    x3haugpo: str  # name
    u6b35mgm: str  # topic
    fld77g72: str  # summary
    ksuxpopi: list[Entry[Message]]  # messages
    ztou5nin: list[Entry[Thread]]  # threads
    kgihivmr: list[Entry[ChatReport]]  # reports
    byf6kyop: bool  # archived
    eqr5qd3v: str  # service
    elvv3ebj: str  # outside_id

    def bridge(self,
        *,
        service: str,
        outside_id: str | None = None,
    ) -> None:
        """How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in."""
        args: dict[str, Any] = {}
        args["service"] = service
        if outside_id is not None:
            args["outside_id"] = outside_id
        return self.action("bridge", **args)

    def bridge_many(self, items: list[TextChannelBridgeArgs]) -> list[dict[str, Any]]:
        """Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bridge", items)

    def unbridge(self) -> None:
        """Keeps a conversation here: no service carries it any more."""
        return self.action("unbridge")

    def unbridge_many(self, items: list[TextChannelUnbridgeArgs]) -> list[dict[str, Any]]:
        """Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unbridge", items)

    def send(self,
        *,
        body: str | None = None,
        reply_to: Message | None = None,
        attachments: list[Any] | None = None,
        turn: str | None = None,
        draft: Draft | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. A message in a direct conversation is news to the other people in it: each is told once, on every device of theirs, who wrote and what, and choosing the notice opens the conversation. A composer names the person's `draft`: what it holds is the message, and the draft is emptied in the same change, so the press is one step to undo. What is sent without a draft is the `body` given, as a program writing a message says it. A reply an agent's `turn` writes begins empty and takes the model's words as it writes them."""
        args: dict[str, Any] = {}
        if body is not None:
            args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        if attachments is not None:
            args["attachments"] = attachments
        if turn is not None:
            args["turn"] = turn
        if draft is not None:
            args["draft"] = draft
        return self.action("send", **args)

    def summarize(self, *, summary: str) -> None:
        """Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers."""
        return self.action("summarize", summary=summary)

    def summarize_many(self, items: list[SummarizeArgs]) -> list[dict[str, Any]]:
        """Runs `summarize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("summarize", items)

    def send_view(self,
        *,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", title=title, definition=definition)

    def relay(self,
        *,
        body: str,
        by: str,
        written_in: str,
        written: int | None = None,
        attachments: list[Any] | None = None,
        reply_to: Message | None = None,
        avatar: str | None = None,
        service: str | None = None,
        outside_emoji: list[OutsideEmoji] | None = None,
        member: str | None = None,
    ) -> Message:
        """Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["body"] = body
        args["by"] = by
        args["written_in"] = written_in
        if written is not None:
            args["written"] = written
        if attachments is not None:
            args["attachments"] = attachments
        if reply_to is not None:
            args["reply_to"] = reply_to
        if avatar is not None:
            args["avatar"] = avatar
        if service is not None:
            args["service"] = service
        if outside_emoji is not None:
            args["outside_emoji"] = outside_emoji
        if member is not None:
            args["member"] = member
        return self.action("relay", **args)

    def rename_channel(self, *, name: str) -> None:
        """Renames a text channel. What was said in it stays."""
        return self.action("rename_channel", name=name)

    def rename_channel_many(self, items: list[RenameChannelArgs]) -> list[dict[str, Any]]:
        """Runs `rename_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_channel", items)

    def set_topic(self, *, topic: str) -> None:
        """Says what a channel is for, in its topic."""
        return self.action("set_topic", topic=topic)

    def set_topic_many(self, items: list[SetTopicArgs]) -> list[dict[str, Any]]:
        """Runs `set_topic` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_topic", items)

    def archive_channel(self,
        *,
        archived: bool | None = None,
    ) -> None:
        """Sets a channel aside, out of the sidebar, keeping everything said in it, or brings it back."""
        args: dict[str, Any] = {}
        if archived is not None:
            args["archived"] = archived
        return self.action("archive_channel", **args)

    def archive_channel_many(self, items: list[ArchiveChannelArgs]) -> list[dict[str, Any]]:
        """Runs `archive_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("archive_channel", items)

    def continue_answer(self, *, m: Message) -> Message:
        """Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out of room, was stopped or broke off is finished this way, not written again from its start."""
        return self.action("continue_answer", m=m)

    def delete(self, *, m: Message) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted, as far as the one deleting reads them."""
        return self.action("delete", m=m)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def report(self,
        *,
        m: Message,
        reason: str | None = None,
    ) -> ChatReport:
        """Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted."""
        args: dict[str, Any] = {}
        args["m"] = m
        if reason is not None:
            args["reason"] = reason
        return self.action("report", **args)

    def start_thread(self,
        *,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"bridge", "bridge_many", "unbridge", "unbridge_many", "send", "summarize", "summarize_many", "send_view", "relay", "rename_channel", "rename_channel_many", "set_topic", "set_topic_many", "archive_channel", "archive_channel_many", "continue_answer", "delete", "delete_many", "report", "start_thread"})


class OutsideAuthor(Resource):
    """OutsideAuthor — `tl.so.tilo.chat.OutsideAuthor`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    @property
    def _5fxndhc2(self) -> str:
        """name (`5fxndhc2` on the wire)."""
        return self.fields["5fxndhc2"]

    @_5fxndhc2.setter
    def _5fxndhc2(self, value: str) -> None:
        self.edit("5fxndhc2", value=value)
    zkszdahn: str  # avatar
    yycajap4: str  # service
    ew25p5fb: str | None  # member


class OutsideEmoji(Resource):
    """OutsideEmoji — `tl.so.tilo.chat.OutsideEmoji`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    ldtylgfu: str  # name
    jjutyqgx: str  # picture


class Message(Resource):
    """Message — `tl.so.tilo.chat.Message`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    xfrqdy3y: str  # body
    @property
    def _4uwxpnw2(self) -> Message | None:
        """reply_to (`4uwxpnw2` on the wire)."""
        return self.fields["4uwxpnw2"]

    @_4uwxpnw2.setter
    def _4uwxpnw2(self, value: Message | None) -> None:
        self.edit("4uwxpnw2", value=value)
    kxekobh6: Thread | None  # thread
    mwily5kw: list[Entry[Part]]  # parts
    @property
    def _4zr7hti6(self) -> bool:
        """pinned (`4zr7hti6` on the wire)."""
        return self.fields["4zr7hti6"]

    @_4zr7hti6.setter
    def _4zr7hti6(self, value: bool) -> None:
        self.edit("4zr7hti6", value=value)
    hj4axvbu: str  # turn
    t2kepujk: str  # reasoning
    ofad24rc: dict[str, list[str]]  # reactions
    dh6346no: str  # sent_view
    wqxsenb3: OutsideAuthor | None  # outside
    bbu6225x: list[Entry[OutsideEmoji]]  # outside_emoji
    tz5yw5dt: str  # by
    @property
    def _4da5gcom(self) -> str:
        """written_in (`4da5gcom` on the wire)."""
        return self.fields["4da5gcom"]

    @_4da5gcom.setter
    def _4da5gcom(self, value: str) -> None:
        self.edit("4da5gcom", value=value)
    mmlcexsa: int  # written
    jcgolgdf: str  # reacted
    @property
    def _6luxdz2x(self) -> list[Entry[RelayedReaction]]:
        """relayed_reactions (`6luxdz2x` on the wire)."""
        return self.fields["6luxdz2x"]

    @_6luxdz2x.setter
    def _6luxdz2x(self, value: list[Entry[RelayedReaction]]) -> None:
        self.edit("6luxdz2x", value=value)
    n7d27huf: list[Entry[Attached]]  # attachments

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

    def relay_reaction(self,
        *,
        emoji: str,
        persona: str,
        by: str,
        given: bool | None = None,
    ) -> None:
        """Relays a reaction someone gave a message where it was written, as that place names them: the counterpart of `relay` for a reaction. A bridge carrying another service's conversation here makes these, so a reaction made there shows under the person's name rather than the bridge's; `given` false takes it back. The reaction's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["emoji"] = emoji
        args["persona"] = persona
        args["by"] = by
        if given is not None:
            args["given"] = given
        return self.action("relay_reaction", **args)

    def relay_reaction_many(self, items: list[RelayReactionArgs]) -> list[dict[str, Any]]:
        """Runs `relay_reaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("relay_reaction", items)

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.
    def pin(self,
        *,
        pinned: bool | None = None,
    ) -> None:
        """Pins a message to its channel, or unpins it."""
        args: dict[str, Any] = {}
        if pinned is not None:
            args["pinned"] = pinned
        return self.action("pin", **args)

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def react(self, *, emoji: str) -> None:
        """Adds your reaction with an emoji to a message, or takes it back when you had reacted with it."""
        return self.action("react", emoji=emoji)

    def react_many(self, items: list[ReactArgs]) -> list[dict[str, Any]]:
        """Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("react", items)

    def copy_text(self) -> None:
        """Copies what a message says, as plain words, to the clipboard."""
        return self.action("copy_text")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "relay_reaction", "relay_reaction_many", "pin", "pin_many", "react", "react_many", "copy_text"})


class ChatReport(Resource):
    """Report — `tl.so.tilo.chat.Report`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    jqcabz2h: Message | None  # message
    e3zqz33a: str | None  # person
    adip7bw4: str  # said
    bmfbgqt6: str  # reason
    @property
    def _3oner4zg(self) -> str:
        """decision (`3oner4zg` on the wire)."""
        return self.fields["3oner4zg"]

    @_3oner4zg.setter
    def _3oner4zg(self, value: str) -> None:
        self.edit("3oner4zg", value=value)
    maczz6ef: str  # overturned

    def decide(self, *, decision: str) -> None:
        """Settles a report: what a moderator decided about it. A report whose decision was overturned is decided again this way."""
        return self.action("decide", decision=decision)

    def decide_many(self, items: list[DecideArgs]) -> list[dict[str, Any]]:
        """Runs `decide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("decide", items)

    def overturn(self, *, why: str) -> None:
        """Overturns what was decided about a report, saying why: the report is open again, for a moderator to decide anew, and the decision overturned stays in its history."""
        return self.action("overturn", why=why)

    def overturn_many(self, items: list[OverturnArgs]) -> list[dict[str, Any]]:
        """Runs `overturn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("overturn", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"decide", "decide_many", "overturn", "overturn_many"})


class Thread(Resource):
    """Thread — `tl.so.tilo.chat.Thread`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    xuvfifa5: str  # name
    cv664irm: str  # summary
    ijy454g6: list[Entry[Message]]  # messages
    @property
    def _2ki7h2z5(self) -> bool:
        """archived (`2ki7h2z5` on the wire)."""
        return self.fields["2ki7h2z5"]

    @_2ki7h2z5.setter
    def _2ki7h2z5(self, value: bool) -> None:
        self.edit("2ki7h2z5", value=value)
    ep7fvntj: list[Entry[ChatReport]]  # reports

    def send(self,
        *,
        body: str | None = None,
        reply_to: Message | None = None,
        attachments: list[Any] | None = None,
        turn: str | None = None,
        draft: Draft | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. A message in a direct conversation is news to the other people in it: each is told once, on every device of theirs, who wrote and what, and choosing the notice opens the conversation. A composer names the person's `draft`: what it holds is the message, and the draft is emptied in the same change, so the press is one step to undo. What is sent without a draft is the `body` given, as a program writing a message says it. A reply an agent's `turn` writes begins empty and takes the model's words as it writes them."""
        args: dict[str, Any] = {}
        if body is not None:
            args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        if attachments is not None:
            args["attachments"] = attachments
        if turn is not None:
            args["turn"] = turn
        if draft is not None:
            args["draft"] = draft
        return self.action("send", **args)

    def summarize(self, *, summary: str) -> None:
        """Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers."""
        return self.action("summarize", summary=summary)

    def summarize_many(self, items: list[ThreadSummarizeArgs]) -> list[dict[str, Any]]:
        """Runs `summarize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("summarize", items)

    def send_view(self,
        *,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", title=title, definition=definition)

    def relay(self,
        *,
        body: str,
        by: str,
        written_in: str,
        written: int | None = None,
        attachments: list[Any] | None = None,
        reply_to: Message | None = None,
        avatar: str | None = None,
        service: str | None = None,
        outside_emoji: list[OutsideEmoji] | None = None,
        member: str | None = None,
    ) -> Message:
        """Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["body"] = body
        args["by"] = by
        args["written_in"] = written_in
        if written is not None:
            args["written"] = written
        if attachments is not None:
            args["attachments"] = attachments
        if reply_to is not None:
            args["reply_to"] = reply_to
        if avatar is not None:
            args["avatar"] = avatar
        if service is not None:
            args["service"] = service
        if outside_emoji is not None:
            args["outside_emoji"] = outside_emoji
        if member is not None:
            args["member"] = member
        return self.action("relay", **args)

    def continue_answer(self, *, m: Message) -> Message:
        """Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out of room, was stopped or broke off is finished this way, not written again from its start."""
        return self.action("continue_answer", m=m)

    def delete(self, *, m: Message) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted, as far as the one deleting reads them."""
        return self.action("delete", m=m)

    def delete_many(self, items: list[ThreadDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def report(self,
        *,
        m: Message,
        reason: str | None = None,
    ) -> ChatReport:
        """Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted."""
        args: dict[str, Any] = {}
        args["m"] = m
        if reason is not None:
            args["reason"] = reason
        return self.action("report", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"send", "summarize", "summarize_many", "send_view", "relay", "continue_answer", "delete", "delete_many", "report"})


class Direct(Resource):
    """Direct — `tl.so.tilo.chat.Direct`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    @property
    def _5stv6ro7(self) -> str:
        """name (`5stv6ro7` on the wire)."""
        return self.fields["5stv6ro7"]

    @_5stv6ro7.setter
    def _5stv6ro7(self, value: str) -> None:
        self.edit("5stv6ro7", value=value)
    wph25aa5: list[str]  # people
    ycadyqh5: str  # person
    s7ol3kl4: str  # summary
    y65whtrn: list[Entry[Message]]  # messages
    @property
    def _3p73kpuw(self) -> list[Entry[Thread]]:
        """threads (`3p73kpuw` on the wire)."""
        return self.fields["3p73kpuw"]

    @_3p73kpuw.setter
    def _3p73kpuw(self, value: list[Entry[Thread]]) -> None:
        self.edit("3p73kpuw", value=value)
    doiukw5i: list[Entry[ChatReport]]  # reports
    a7u4jddr: str  # service
    f4cfgfpu: str  # outside_id

    def bridge(self,
        *,
        service: str,
        outside_id: str | None = None,
    ) -> None:
        """How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work. Carries a conversation to an outside service: its name as people read it ('Discord') and the id it has there. The bridge's own actions keep what joins the two sides; this is what every view reads to show the place a conversation is also said in."""
        args: dict[str, Any] = {}
        args["service"] = service
        if outside_id is not None:
            args["outside_id"] = outside_id
        return self.action("bridge", **args)

    def bridge_many(self, items: list[DirectBridgeArgs]) -> list[dict[str, Any]]:
        """Runs `bridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bridge", items)

    def unbridge(self) -> None:
        """Keeps a conversation here: no service carries it any more."""
        return self.action("unbridge")

    def unbridge_many(self, items: list[DirectUnbridgeArgs]) -> list[dict[str, Any]]:
        """Runs `unbridge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unbridge", items)

    def unread_count(self) -> int:
        """How many messages of a direct conversation the person asking has not read: the number its row shows beside the other person, and what an agent or a script reads of it."""
        return self.action("unread_count")

    def send(self,
        *,
        body: str | None = None,
        reply_to: Message | None = None,
        attachments: list[Any] | None = None,
        turn: str | None = None,
        draft: Draft | None = None,
    ) -> Message:
        """Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one. History and the catch-up say what a person did in its own words, counting the messages a step of them holds. A message in a direct conversation is news to the other people in it: each is told once, on every device of theirs, who wrote and what, and choosing the notice opens the conversation. A composer names the person's `draft`: what it holds is the message, and the draft is emptied in the same change, so the press is one step to undo. What is sent without a draft is the `body` given, as a program writing a message says it. A reply an agent's `turn` writes begins empty and takes the model's words as it writes them."""
        args: dict[str, Any] = {}
        if body is not None:
            args["body"] = body
        if reply_to is not None:
            args["reply_to"] = reply_to
        if attachments is not None:
            args["attachments"] = attachments
        if turn is not None:
            args["turn"] = turn
        if draft is not None:
            args["draft"] = draft
        return self.action("send", **args)

    def summarize(self, *, summary: str) -> None:
        """Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers."""
        return self.action("summarize", summary=summary)

    def summarize_many(self, items: list[DirectSummarizeArgs]) -> list[dict[str, Any]]:
        """Runs `summarize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("summarize", items)

    def send_view(self,
        *,
        title: str,
        definition: str,
    ) -> Message:
        """Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views."""
        return self.action("send_view", title=title, definition=definition)

    def relay(self,
        *,
        body: str,
        by: str,
        written_in: str,
        written: int | None = None,
        attachments: list[Any] | None = None,
        reply_to: Message | None = None,
        avatar: str | None = None,
        service: str | None = None,
        outside_emoji: list[OutsideEmoji] | None = None,
        member: str | None = None,
    ) -> Message:
        """Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when. A bridge relays another service's conversation this way; the message's history names the bridge, and the message says whom it relays."""
        args: dict[str, Any] = {}
        args["body"] = body
        args["by"] = by
        args["written_in"] = written_in
        if written is not None:
            args["written"] = written
        if attachments is not None:
            args["attachments"] = attachments
        if reply_to is not None:
            args["reply_to"] = reply_to
        if avatar is not None:
            args["avatar"] = avatar
        if service is not None:
            args["service"] = service
        if outside_emoji is not None:
            args["outside_emoji"] = outside_emoji
        if member is not None:
            args["member"] = member
        return self.action("relay", **args)

    def continue_answer(self, *, m: Message) -> Message:
        """Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out of room, was stopped or broke off is finished this way, not written again from its start."""
        return self.action("continue_answer", m=m)

    def delete(self, *, m: Message) -> None:
        """Deletes a message from the channel or thread it is in. Its author, or anyone the channel's permissions allow, may: the platform undoes it rather than asking first, and the removal is a version of the message, so who removed it stays answerable. Its reports stay, saying it was deleted, as far as the one deleting reads them."""
        return self.action("delete", m=m)

    def delete_many(self, items: list[DirectDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def report(self,
        *,
        m: Message,
        reason: str | None = None,
    ) -> ChatReport:
        """Reports a message of a conversation to the workspace's moderators, saying why when the reporter does. Who reported it is the report's history's, so moderators see who asked; the conversation keeps it, with who wrote the message and what it said, after the message is deleted."""
        args: dict[str, Any] = {}
        args["m"] = m
        if reason is not None:
            args["reason"] = reason
        return self.action("report", **args)

    def start_thread(self,
        *,
        m: Message,
        name: str | None = None,
    ) -> Thread:
        """Starts a thread from a message of a channel or a direct conversation, named after what the message says."""
        args: dict[str, Any] = {}
        args["m"] = m
        if name is not None:
            args["name"] = name
        return self.action("start_thread", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"bridge", "bridge_many", "unbridge", "unbridge_many", "unread_count", "send", "summarize", "summarize_many", "send_view", "relay", "continue_answer", "delete", "delete_many", "report", "start_thread"})


class Draft(Resource):
    """Draft — `tl.so.tilo.chat.Draft`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    @property
    def _5shwuvrs(self) -> Direct | TextChannel | Thread:
        """conversation (`5shwuvrs` on the wire)."""
        return self.fields["5shwuvrs"]

    @_5shwuvrs.setter
    def _5shwuvrs(self, value: Direct | TextChannel | Thread) -> None:
        self.edit("5shwuvrs", value=value)
    rxmavdj2: str  # body
    o5vmbr2v: list[Any]  # files

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[DraftEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[DraftInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[DraftDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def attach(self, *, files: list[Any]) -> None:
        """Adds the files a person chose to the message they are writing: the system's chooser runs this with what was chosen, and the draft holds them until it is sent. The chooser offers every kind of file, since a message carries any."""
        return self.action("attach", files=files)

    def attach_many(self, items: list[AttachArgs]) -> list[dict[str, Any]]:
        """Runs `attach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("attach", items)

    def detach(self, *, file: Any) -> None:
        """Takes a file off the message a person is writing, before it is sent."""
        return self.action("detach", file=file)

    def detach_many(self, items: list[DetachArgs]) -> list[dict[str, Any]]:
        """Runs `detach` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("detach", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "attach", "attach_many", "detach", "detach_many"})


class Attached(Resource):
    """Attached — `tl.so.tilo.chat.Attached`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    qsdc77jc: str  # name
    h5lqkkjt: str  # media
    g7nx7a7g: int  # size
    wu2n7dpm: str  # blob

    def open_attachment(self) -> None:
        """Opens the file a message carries in the app that opens its kind: the file reaches that app as the same content both read, never as a path neither has."""
        return self.action("open_attachment")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_attachment"})


class RelayedReaction(Resource):
    """RelayedReaction — `tl.so.tilo.chat.RelayedReaction`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    r2zcptjt: str  # emoji
    awgtnj33: str  # persona
    hammizbi: str  # by


class Part(Resource):
    """Part — `tl.so.tilo.chat.Part`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    zevnbs3i: str  # kind
    @property
    def _3hkzkqvr(self) -> str:
        """body (`3hkzkqvr` on the wire)."""
        return self.fields["3hkzkqvr"]

    @_3hkzkqvr.setter
    def _3hkzkqvr(self, value: str) -> None:
        self.edit("3hkzkqvr", value=value)


class Skill(Resource):
    """Skill — `tl.so.tilo.chat.Skill`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    gfk6pm4a: str  # name
    gnfldsvz: str  # work
    bn3w5doj: str  # how
    ijpa4i7d: str  # form
    @property
    def _3wlkwi6k(self) -> str:
        """model (`3wlkwi6k` on the wire)."""
        return self.fields["3wlkwi6k"]

    @_3wlkwi6k.setter
    def _3wlkwi6k(self, value: str) -> None:
        self.edit("3wlkwi6k", value=value)
    bxx5rykt: list[Entry[Example]]  # examples

    def correct_skill(self,
        *,
        name: str | None = None,
        work: str | None = None,
        how: str | None = None,
        form: str | None = None,
        model: str | None = None,
    ) -> None:
        """Corrects a skill: only what is given changes, and what is left out stays as it is."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if work is not None:
            args["work"] = work
        if how is not None:
            args["how"] = how
        if form is not None:
            args["form"] = form
        if model is not None:
            args["model"] = model
        return self.action("correct_skill", **args)

    def correct_skill_many(self, items: list[CorrectSkillArgs]) -> list[dict[str, Any]]:
        """Runs `correct_skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("correct_skill", items)

    def forget_skill(self) -> None:
        """Forgets a skill: no later turn reads it."""
        return self.action("forget_skill")

    def forget_skill_many(self, items: list[ForgetSkillArgs]) -> list[dict[str, Any]]:
        """Runs `forget_skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_skill", items)

    def keep_example(self,
        *,
        text: str,
        accepted: bool,
        note: str | None = None,
    ) -> None:
        """Keeps an example of a skill: a result the person accepted (more like this) or rejected (less like this), with what they said about it. The same result said again is that example, moved newest, and a skill keeps its newest twelve."""
        args: dict[str, Any] = {}
        args["text"] = text
        args["accepted"] = accepted
        if note is not None:
            args["note"] = note
        return self.action("keep_example", **args)

    def keep_example_many(self, items: list[KeepExampleArgs]) -> list[dict[str, Any]]:
        """Runs `keep_example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_example", items)

    def drop_example(self, *, example: Example) -> None:
        """Takes an example out of a skill."""
        return self.action("drop_example", example=example)

    def drop_example_many(self, items: list[DropExampleArgs]) -> list[dict[str, Any]]:
        """Runs `drop_example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drop_example", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"correct_skill", "correct_skill_many", "forget_skill", "forget_skill_many", "keep_example", "keep_example_many", "drop_example", "drop_example_many"})


class Example(Resource):
    """Example — `tl.so.tilo.chat.Example`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    fspydtu6: str  # text
    @property
    def _2u2ngevv(self) -> bool:
        """accepted (`2u2ngevv` on the wire)."""
        return self.fields["2u2ngevv"]

    @_2u2ngevv.setter
    def _2u2ngevv(self, value: bool) -> None:
        self.edit("2u2ngevv", value=value)
    agcun6lt: str  # note
    @property
    def _3ccutu3r(self) -> int:
        """at (`3ccutu3r` on the wire)."""
        return self.fields["3ccutu3r"]

    @_3ccutu3r.setter
    def _3ccutu3r(self, value: int) -> None:
        self.edit("3ccutu3r", value=value)


class Chat(Resource):
    """Chat — `tl.so.tilo.chat`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"

class WorkspaceEmoji(Resource):
    """Workspace emoji — `tilo.workspace.emoji`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    emoji: dict[str, Emoji]  # Emoji

    def add_emoji(self,
        *,
        name: str,
        image: str,
    ) -> None:
        """A picture (PNG or WebP, base64, at most 12 000 bytes; 64 pixels a side draws sharply) under a name of 2 to 32 letters, digits and underscores, written `:name:` in reactions and text."""
        return self.action("add-emoji", name=name, image=image)

    def add_emoji_many(self, items: list[AddEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `add-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-emoji", items)

    def remove_emoji(self, *, name: str) -> None:
        """One you added."""
        return self.action("remove-emoji", name=name)

    def remove_emoji_many(self, items: list[RemoveEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `remove-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-emoji", items)

    def remove_any_emoji(self, *, name: str) -> None:
        """Someone else's emoji: the workspace's roles decide who may."""
        return self.action("remove-any-emoji", name=name)

    def remove_any_emoji_many(self, items: list[RemoveAnyEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `remove-any-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-any-emoji", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_emoji", "add_emoji_many", "remove_emoji", "remove_emoji_many", "remove_any_emoji", "remove_any_emoji_many"})


class Moderation(Resource):
    """Moderation — `tilo.moderation`."""
    __surface__ = "so.tilo.chat@66fb84ff706b3fcb"
    @property
    def reports(self) -> list[Entry[Report]]:
        """Reports"""
        return self.fields["reports"]
    @property
    def decisions(self) -> list[Entry[Decision]]:
        """Decisions"""
        return self.fields["decisions"]

    def report_message(self,
        *,
        message: str,
        author: Any,
        reason: str,
    ) -> None:
        """Tells the workspace's moderators that this message, and its author, should not be here."""
        return self.action("report-message", message=message, author=author, reason=reason)

    def report_message_many(self, items: list[ReportMessageArgs]) -> list[dict[str, Any]]:
        """Runs `report-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("report-message", items)

    def report_person(self,
        *,
        person: Any,
        reason: str,
    ) -> None:
        """Tells the workspace's moderators that this person is misbehaving."""
        return self.action("report-person", person=person, reason=reason)

    def report_person_many(self, items: list[ReportPersonArgs]) -> list[dict[str, Any]]:
        """Runs `report-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("report-person", items)

    def decide_reports(self,
        *,
        id: str,
        person: Any,
        reports: list[str],
        ruling: Ruling,
    ) -> None:
        """Decide on a person. Resources what was done about a person and why, settling the reports named."""
        return self.action("decide-reports", id=id, person=person, reports=reports, ruling=ruling)

    def decide_reports_many(self, items: list[DecideReportsArgs]) -> list[dict[str, Any]]:
        """Runs `decide-reports` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("decide-reports", items)

    def overturn_decision(self,
        *,
        decision: str,
        note: str,
    ) -> None:
        """Marks a decision as overturned by a Moderator, who lifts its restriction or ban and brings back what it took back."""
        return self.action("overturn-decision", decision=decision, note=note)

    def overturn_decision_many(self, items: list[OverturnDecisionArgs]) -> list[dict[str, Any]]:
        """Runs `overturn-decision` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("overturn-decision", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"report_message", "report_message_many", "report_person", "report_person_many", "decide_reports", "decide_reports_many", "overturn_decision", "overturn_decision_many"})


