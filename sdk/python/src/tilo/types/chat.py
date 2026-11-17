"""Types for Chat (`so.tilo.chat`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `chat.workspace` at `chat/<resource>`, read with
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
    "Category",
    "ChannelKind",
    "ChannelPlace",
    "ContextField",
    "Conversation",
    "CropRef",
    "Decision",
    "Destination",
    "FeedbackReport",
    "FrameTimings",
    "Hidden",
    "Interaction",
    "Moderation",
    "Outcome",
    "Outcome2",
    "Part",
    "Person",
    "Presence",
    "ReadMark",
    "RecordingRef",
    "Report",
    "ResourceRef",
    "Reverted",
    "ReviewedFeedback",
    "ReviewedSelection",
    "ReviewedText",
    "Ruling",
    "ScreenshotRef",
    "Span",
    "SpanUnit",
    "Submission",
    "Summary",
    "TextKind",
    "Triage",
    "TriageUpdate",
    "WorkspaceFiles",
    "AgentFinish",
    "AgentStepOutcome",
    "AgentWorkState",
    "Anchor",
    "Answered",
    "Attached",
    "Choice",
    "ClipAttr",
    "ClipBlock",
    "MarkSpan",
    "MarkValue",
    "Mention",
    "Message",
    "NewMessage",
    "Origin",
    "MessagesOutcome",
    "ProviderStream",
    "Question",
    "Quote",
    "ResourceAt",
    "Revision",
    "ShareAccess",
    "ShownAs",
    "ShownPersona",
    "Step",
    "StepUpdate",
    "Transition",
    "WindowShare",
    "Withdrawn",
    "Work",
    "WorkUpdate",
    "Fact",
    "Emoji",
    "Agent",
    "AgentState",
    "Bound",
    "Spent",
    "Workspace",
    "Messages",
    "Drafts",
    "Learned",
    "WorkspaceEmoji",
    "ChatAgent",
    "BeginChatArgs",
    "IntroduceArgs",
    "CreateCategoryArgs",
    "CreateChannelArgs",
    "CreateChannelWithIdArgs",
    "DeleteCategoryArgs",
    "MoveChannelArgs",
    "MoveCategoryArgs",
    "ArrangeChannelsArgs",
    "SetChannelCategoryArgs",
    "RenameChannelArgs",
    "SetChannelTopicArgs",
    "ArchiveChannelArgs",
    "RenameCategoryArgs",
    "DeleteChannelArgs",
    "ReportMessageArgs",
    "ReportPersonArgs",
    "DecideReportsArgs",
    "OverturnDecisionArgs",
    "MarkReadArgs",
    "KeepSummaryArgs",
    "SubmitFeedbackArgs",
    "TriageArgs",
    "PostArgs",
    "PostFileArgs",
    "ShareResourceArgs",
    "ShareWindowArgs",
    "StopSharingArgs",
    "SetShareAccessArgs",
    "ShareOpenArgs",
    "DeliverMessageArgs",
    "EditMessageArgs",
    "DeleteMessageArgs",
    "RemoveMessageArgs",
    "PinArgs",
    "RetryArgs",
    "RecordWorkArgs",
    "AskArgs",
    "AnswerQuestionArgs",
    "WithdrawQuestionArgs",
    "SettleQuestionArgs",
    "CarryCountsArgs",
    "StreamOutputArgs",
    "StartDraftArgs",
    "LearnArgs",
    "CorrectArgs",
    "ForgetArgs",
    "AddEmojiArgs",
    "RemoveEmojiArgs",
    "RemoveAnyEmojiArgs",
    "CreateAgentArgs",
    "SetAgentSpendLimitArgs",
    "RecordAgentSpendArgs",
    "SetAgentOnHostArgs",
    "SetAgentColourArgs",
    "RenameAgentArgs",
    "SetAgentModelArgs",
    "SetAgentInstructionArgs",
    "SetAgentBoundArgs",
    "RetireAgentArgs",
]

class Category(TypedDict):
    name: str

ChannelKind = Literal["text", "voice"]

class ChannelPlace(TypedDict):
    channel: str
    category: str | None

class ContextField(TypedDict):
    key: str
    value: str

class Conversation(TypedDict):
    category: str | None
    name: str
    topic: str
    kind: ChannelKind
    archived: bool
    members: list[str]

class CropRef(TypedDict):
    x: int
    y: int
    width: int
    height: int

class Decision(TypedDict):
    id: str
    person: str
    reports: list[str]
    outcome: Outcome2
    until_ms: int | None
    reasoning: str
    model: str
    reverted: list[Reverted]
    by: str
    at_ms: int
    overturned_by: str | None
    overturn_note: str | None
    overturned_at_ms: int | None

class Destination(TypedDict):
    workspace: str
    provider: str
    channel: str

class FeedbackReport(TypedDict):
    submission: Submission | None
    author: str
    ts: int
    triage: list[Entry[Triage]]
    reactions: Any

class FrameTimings(TypedDict):
    frames: list[int]
    budget: int

Hidden = Literal["name", "email", "phone", "address", "account", "credential", "personal"]

class Interaction(TypedDict):
    at: int
    app: str
    action: str
    resource: str
    label: str | None
    outcome: Outcome

class Moderation(TypedDict):
    reports: list[Entry[Report]]
    decisions: list[Entry[Decision]]

Outcome = Literal["done", "refused", "failed"]

Outcome2 = Literal["no-action", "restrict", "ban"]

class Part(TypedDict):
    text: str
    hidden: Hidden | None

class Person(TypedDict):
    id: str
    name: str
    initials: str
    color: int
    status: Presence

Presence = Literal["online", "idle", "dnd", "offline"]

class ReadMark(TypedDict):
    part: int
    seen: int

class RecordingRef(TypedDict):
    hash: str
    app: str
    app_name: str
    kind: str
    from_ms: int
    to_ms: int
    steps: int
    bytes: int

class Report(TypedDict):
    id: str
    reporter: str
    person: str
    app: str | None
    record: str | None
    item: str | None
    reason: str
    at_ms: int
    decision: str | None

class ResourceRef(TypedDict):
    kind: str
    id: str
    label: str
    span: Span | None

class Reverted(TypedDict):
    app: str
    record: str
    run: str
    changes: int

class ReviewedFeedback(TypedDict):
    text: str
    context: list[ContextField]
    screenshot: ScreenshotRef | None
    selections: list[ReviewedSelection]
    interactions: list[Interaction]
    timings: FrameTimings | None
    recording: RecordingRef | None

class ReviewedSelection(TypedDict):
    app: str
    app_name: str
    build: str | None
    path: str
    role: str
    kind: str | None
    record: str | None
    label: ReviewedText | None
    texts: list[ReviewedText]
    note: ReviewedText | None
    crop: CropRef | None

class ReviewedText(TypedDict):
    kind: TextKind
    parts: list[Part]

class Ruling(TypedDict):
    outcome: Outcome2
    until_ms: int | None
    reasoning: str
    model: str
    reverted: list[Reverted]

class ScreenshotRef(TypedDict):
    hash: str
    width: int
    height: int

class Span(TypedDict):
    start: int
    end: int
    unit: SpanUnit

SpanUnit = Literal["Chars", "Bytes"]

class Submission(TypedDict):
    id: str
    destination: Destination
    feedback: ReviewedFeedback

class Summary(TypedDict):
    text: str
    through: str
    through_ts: int

TextKind = Literal["interface", "content"]

class Triage(TypedDict):
    author: str
    ts: int
    update: TriageUpdate | None

TriageUpdate = dict[str, Any]
"""One of investigating, resolved, reopen, duplicate, task: an object whose case the app tags it with."""

class WorkspaceFiles(TypedDict):
    largest: int
    member_hourly: int

AgentFinish = Literal["complete", "truncated", "refused", "cancelled", "failed"]

AgentStepOutcome = dict[str, Any]
"""One of asked, applied, no_change, refused: an object whose case the app tags it with."""

AgentWorkState = dict[str, Any]
"""One of taken, waiting, doing, provider_finished, done, refused: an object whose case the app tags it with."""

Anchor = dict[str, Any]
"""One of Item, Items, Text: an object whose case the app tags it with."""

class Answered(TypedDict):
    answer: str
    device: str
    at: int
    detail: str

Attached = dict[str, Any]
"""One of resource, text, image, video, file: an object whose case the app tags it with."""

class Choice(TypedDict):
    id: str
    label: str

ClipAttr = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class ClipBlock(TypedDict):
    kind: str
    text: str
    marks: list[MarkSpan]
    level: int | None
    ordered: bool | None
    align: str | None
    attrs: dict[str, ClipAttr]

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class Mention(TypedDict):
    user: str
    start: int
    len: int
    is_agent: bool

class Message(TypedDict):
    channel: str
    author: str
    ts: int
    text: str
    body: list[ClipBlock]
    edited: bool
    history: list[Revision]
    parent: str | None
    quoted: Quote | None
    reactions: Any
    mentions: list[Mention]
    attachments: list[Attached]
    pinned: bool
    work: Work | None
    appearance: ShownAs | None
    acting: Any | None
    author_name: str | None
    origin: Origin | None
    share: WindowShare | None
    question: Question | None
    attempts: int

class NewMessage(TypedDict):
    channel: str
    text: str
    parent: str | None
    quoted: Quote | None
    body: list[ClipBlock] | None
    attachments: list[Attached]
    mentions: list[Mention] | None
    written: int | None
    origin: Origin | None

class Origin(TypedDict):
    author: str
    handle: str | None
    source: str
    url: str | None
    reference: str | None
    created_ms: int | None
    updated_ms: int | None

class MessagesOutcome(TypedDict):
    outcome: str
    at: int
    detail: str

class ProviderStream(TypedDict):
    run: str
    sequence: int
    reasoning: str
    progress: str
    finish: AgentFinish | None
    detail: str

class Question(TypedDict):
    kind: str
    person: str
    answers: list[Choice]
    detail: str
    answered: Answered | None
    outcome: MessagesOutcome | None
    withdrawn: Withdrawn | None

class Quote(TypedDict):
    text: str
    start: int
    end: int

class ResourceAt(TypedDict):
    app: str
    record: str | None
    reference: ResourceRef

class Revision(TypedDict):
    ts: int
    text: str

ShareAccess = Literal["view", "edit"]

class ShownAs(TypedDict):
    name: str
    avatar: str | None
    persona: ShownPersona | None

class ShownPersona(TypedDict):
    service: str
    account: str
    linked: str | None

class Step(TypedDict):
    id: str
    sequence: int
    resource: str
    label: str
    outcome: AgentStepOutcome | None

class StepUpdate(TypedDict):
    id: str
    sequence: int
    resource: str
    label: str
    outcome: str
    why: str | None

class Transition(TypedDict):
    state: AgentWorkState
    since: int

class WindowShare(TypedDict):
    access: ShareAccess
    open: bool
    ended: bool

class Withdrawn(TypedDict):
    at: int
    why: str

class Work(TypedDict):
    stream: ProviderStream | None
    state: Transition | None
    trace: list[Entry[Step]]
    failure_detail: str | None

class WorkUpdate(TypedDict):
    state: str
    app: str | None
    did: str | None
    about: str | None
    trace: list[StepUpdate]
    detail: str | None

class Fact(TypedDict):
    text: str
    source: str
    at: int
    by: str | None

class Emoji(TypedDict):
    image: Any
    added_by: str
    added_at: int

class Agent(TypedDict):
    id: str
    name: str
    colour: int | None
    keeper: str
    on_host: bool
    model: str | None
    mcp_client: str | None
    bound: list[Bound]
    spend_limit_micros: int | None
    spent: Spent | None
    state: AgentState
    created_at: int
    instruction: str | None

AgentState = Literal["active", "retired"]

class Bound(TypedDict):
    node: str
    kinds: list[str]
    answer: str

class Spent(TypedDict):
    day: str
    cost_micros: int

BeginChatArgs = TypedDict("BeginChatArgs", {
    "name": "str",
    "initials": "str",
    "color": "int",
})
"""The arguments of `begin-chat`."""

IntroduceArgs = TypedDict("IntroduceArgs", {
    "name": "str",
    "initials": "str",
    "color": "int",
})
"""The arguments of `introduce`."""

CreateCategoryArgs = TypedDict("CreateCategoryArgs", {
    "name": "str",
})
"""The arguments of `create-category`."""

CreateChannelArgs = TypedDict("CreateChannelArgs", {
    "name": "str",
    "category": NotRequired["str | None"],
    "kind": NotRequired["ChannelKind | None"],
    "topic": NotRequired["str | None"],
})
"""The arguments of `create-channel`."""

CreateChannelWithIdArgs = TypedDict("CreateChannelWithIdArgs", {
    "id": "str",
    "name": "str",
    "category": NotRequired["str | None"],
    "kind": NotRequired["ChannelKind | None"],
    "topic": NotRequired["str | None"],
    "members": NotRequired["list[Any] | None"],
})
"""The arguments of `create-channel-with-id`."""

DeleteCategoryArgs = TypedDict("DeleteCategoryArgs", {
    "category": "str",
})
"""The arguments of `delete-category`."""

MoveChannelArgs = TypedDict("MoveChannelArgs", {
    "channel": "str",
    "up": "bool",
})
"""The arguments of `move-channel`."""

MoveCategoryArgs = TypedDict("MoveCategoryArgs", {
    "category": "str",
    "up": "bool",
})
"""The arguments of `move-category`."""

ArrangeChannelsArgs = TypedDict("ArrangeChannelsArgs", {
    "categories": "list[str]",
    "channels": "list[ChannelPlace]",
})
"""The arguments of `arrange-channels`."""

SetChannelCategoryArgs = TypedDict("SetChannelCategoryArgs", {
    "channel": "str",
    "category": NotRequired["str | None"],
})
"""The arguments of `set-channel-category`."""

RenameChannelArgs = TypedDict("RenameChannelArgs", {
    "channel": "str",
    "name": "str",
})
"""The arguments of `rename-channel`."""

SetChannelTopicArgs = TypedDict("SetChannelTopicArgs", {
    "channel": "str",
    "topic": "str",
})
"""The arguments of `set-channel-topic`."""

ArchiveChannelArgs = TypedDict("ArchiveChannelArgs", {
    "channel": "str",
    "archived": "bool",
})
"""The arguments of `archive-channel`."""

RenameCategoryArgs = TypedDict("RenameCategoryArgs", {
    "category": "str",
    "name": "str",
})
"""The arguments of `rename-category`."""

DeleteChannelArgs = TypedDict("DeleteChannelArgs", {
    "channel": "str",
})
"""The arguments of `delete-channel`."""

ReportMessageArgs = TypedDict("ReportMessageArgs", {
    "message": "str",
    "author": "Any",
    "reason": "str",
})
"""The arguments of `report-message`."""

ReportPersonArgs = TypedDict("ReportPersonArgs", {
    "person": "str",
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

MarkReadArgs = TypedDict("MarkReadArgs", {
    "channel": "str",
    "part": "int",
    "seen": "int",
})
"""The arguments of `mark-read`."""

KeepSummaryArgs = TypedDict("KeepSummaryArgs", {
    "channel": "str",
    "through": "str",
    "through_ts": "int",
    "text": "str",
})
"""The arguments of `keep-summary`."""

SubmitFeedbackArgs = TypedDict("SubmitFeedbackArgs", {
    "channel": "str",
    "submission": "Any",
})
"""The arguments of `submit-feedback`."""

TriageArgs = TypedDict("TriageArgs", {
    "report": "str",
    "update": "Any",
})
"""The arguments of `triage`."""

PostArgs = TypedDict("PostArgs", {
    "channel": "str",
    "text": "str",
    "parent": NotRequired["str | None"],
    "quoted": NotRequired["Quote | None"],
    "body": NotRequired["list[ClipBlock] | None"],
    "attachments": NotRequired["list[Attached] | None"],
    "mentions": NotRequired["list[Mention] | None"],
    "written": NotRequired["int | None"],
    "origin": NotRequired["Origin | None"],
})
"""The arguments of `post`."""

PostFileArgs = TypedDict("PostFileArgs", {
    "channel": "str",
    "content": "str",
    "name": "str",
    "text": NotRequired["str | None"],
    "attachment": NotRequired["Attached | None"],
    "mentions": NotRequired["list[Mention] | None"],
})
"""The arguments of `post-file`."""

ShareResourceArgs = TypedDict("ShareResourceArgs", {
    "channel": "str",
    "attached": "ResourceAt",
    "text": NotRequired["str | None"],
    "mentions": NotRequired["list[Mention] | None"],
})
"""The arguments of `share-resource`."""

ShareWindowArgs = TypedDict("ShareWindowArgs", {
    "channel": "str",
    "attached": "ResourceAt",
    "access": NotRequired["ShareAccess | None"],
    "text": NotRequired["str | None"],
    "mentions": NotRequired["list[Mention] | None"],
})
"""The arguments of `share-window`."""

StopSharingArgs = TypedDict("StopSharingArgs", {
    "message": "str",
})
"""The arguments of `stop-sharing`."""

SetShareAccessArgs = TypedDict("SetShareAccessArgs", {
    "message": "str",
    "access": "ShareAccess",
})
"""The arguments of `set-share-access`."""

ShareOpenArgs = TypedDict("ShareOpenArgs", {
    "message": "str",
    "open": "bool",
})
"""The arguments of `share-open`."""

DeliverMessageArgs = TypedDict("DeliverMessageArgs", {
    "channel": "str",
    "author": "str",
    "body": "str",
    "ts": NotRequired["int | None"],
})
"""The arguments of `deliver-message`."""

EditMessageArgs = TypedDict("EditMessageArgs", {
    "message": "str",
    "text": "str",
    "body": NotRequired["list[ClipBlock] | None"],
    "mentions": NotRequired["list[Mention] | None"],
})
"""The arguments of `edit-message`."""

DeleteMessageArgs = TypedDict("DeleteMessageArgs", {
    "message": "str",
})
"""The arguments of `delete-message`."""

RemoveMessageArgs = TypedDict("RemoveMessageArgs", {
    "message": "str",
})
"""The arguments of `remove-message`."""

PinArgs = TypedDict("PinArgs", {
    "message": "str",
    "pinned": "bool",
})
"""The arguments of `pin`."""

RetryArgs = TypedDict("RetryArgs", {
    "message": "str",
    "reply": "str",
})
"""The arguments of `retry`."""

RecordWorkArgs = TypedDict("RecordWorkArgs", {
    "message": "str",
    "work": "WorkUpdate",
    "say": NotRequired["str | None"],
})
"""The arguments of `record-work`."""

AskArgs = TypedDict("AskArgs", {
    "channel": "str",
    "text": "str",
    "answers": NotRequired["list[Choice] | None"],
    "kind": NotRequired["str | None"],
    "detail": NotRequired["Any | None"],
    "parent": NotRequired["str | None"],
})
"""The arguments of `ask`."""

AnswerQuestionArgs = TypedDict("AnswerQuestionArgs", {
    "message": "str",
    "answer": "str",
    "device": NotRequired["str | None"],
    "detail": NotRequired["Any | None"],
})
"""The arguments of `answer-question`."""

WithdrawQuestionArgs = TypedDict("WithdrawQuestionArgs", {
    "message": "str",
    "why": NotRequired["str | None"],
})
"""The arguments of `withdraw-question`."""

SettleQuestionArgs = TypedDict("SettleQuestionArgs", {
    "message": "str",
    "outcome": "str",
    "detail": NotRequired["str | None"],
})
"""The arguments of `settle-question`."""

CarryCountsArgs = TypedDict("CarryCountsArgs", {
    "earlier": "dict[str, dict[str, int]]",
})
"""The arguments of `carry-counts`."""

StreamOutputArgs = TypedDict("StreamOutputArgs", {
    "channel": "str",
    "reply": "str",
    "name": NotRequired["str | None"],
    "event": "Any",
})
"""The arguments of `stream-output`."""

StartDraftArgs = TypedDict("StartDraftArgs", {
    "key": "str",
})
"""The arguments of `start-draft`."""

LearnArgs = TypedDict("LearnArgs", {
    "text": "str",
    "source": "str",
})
"""The arguments of `learn`."""

CorrectArgs = TypedDict("CorrectArgs", {
    "fact": "str",
    "text": "str",
    "source": NotRequired["str | None"],
})
"""The arguments of `correct`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "fact": "str",
})
"""The arguments of `forget`."""

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

CreateAgentArgs = TypedDict("CreateAgentArgs", {
    "name": "str",
    "keeper": NotRequired["str | None"],
    "id": NotRequired["str | None"],
    "model": NotRequired["str | None"],
    "colour": NotRequired["int | None"],
    "mcp_client": NotRequired["str | None"],
})
"""The arguments of `create-agent`."""

SetAgentSpendLimitArgs = TypedDict("SetAgentSpendLimitArgs", {
    "agent": "str",
    "limit_micros": NotRequired["int | None"],
})
"""The arguments of `set-agent-spend-limit`."""

RecordAgentSpendArgs = TypedDict("RecordAgentSpendArgs", {
    "agent": "str",
    "day": "str",
    "cost_micros": "int",
})
"""The arguments of `record-agent-spend`."""

SetAgentOnHostArgs = TypedDict("SetAgentOnHostArgs", {
    "agent": "str",
    "on_host": "bool",
})
"""The arguments of `set-agent-on-host`."""

SetAgentColourArgs = TypedDict("SetAgentColourArgs", {
    "agent": "str",
    "colour": "int",
})
"""The arguments of `set-agent-colour`."""

RenameAgentArgs = TypedDict("RenameAgentArgs", {
    "agent": "str",
    "name": "str",
})
"""The arguments of `rename-agent`."""

SetAgentModelArgs = TypedDict("SetAgentModelArgs", {
    "agent": "str",
    "model": NotRequired["str | None"],
})
"""The arguments of `set-agent-model`."""

SetAgentInstructionArgs = TypedDict("SetAgentInstructionArgs", {
    "agent": "str",
    "instruction": NotRequired["str | None"],
})
"""The arguments of `set-agent-instruction`."""

SetAgentBoundArgs = TypedDict("SetAgentBoundArgs", {
    "agent": "str",
    "bound": "list[Bound]",
})
"""The arguments of `set-agent-bound`."""

RetireAgentArgs = TypedDict("RetireAgentArgs", {
    "agent": "str",
})
"""The arguments of `retire-agent`."""

class Workspace(Resource):
    """Chat — `chat.workspace`."""
    __surface__ = "so.tilo.chat@22a1d9a3cb0ac3f0"
    @property
    def categories(self) -> list[Entry[Category]]:
        """Read only."""
        return self.fields["categories"]
    @property
    def channels(self) -> list[Entry[Conversation]]:
        """Read only."""
        return self.fields["channels"]
    people: list[Entry[Person]]
    read: dict[str, dict[str, ReadMark]]
    feedback: list[Entry[FeedbackReport]]
    moderation: Moderation
    @property
    def files(self) -> WorkspaceFiles:
        """Read only."""
        return self.fields["files"]
    @property
    def summaries(self) -> dict[str, dict[str, Summary]]:
        """Read only."""
        return self.fields["summaries"]

    def begin_chat(self,
        *,
        name: str,
        initials: str,
        color: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("begin-chat", name=name, initials=initials, color=color)

    def begin_chat_many(self, items: list[BeginChatArgs]) -> list[dict[str, Any]]:
        """Runs `begin-chat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin-chat", items)

    def introduce(self,
        *,
        name: str,
        initials: str,
        color: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("introduce", name=name, initials=initials, color=color)

    def introduce_many(self, items: list[IntroduceArgs]) -> list[dict[str, Any]]:
        """Runs `introduce` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("introduce", items)

    def create_category(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("create-category", name=name)

    def create_category_many(self, items: list[CreateCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `create-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-category", items)

    def create_channel(self,
        *,
        name: str,
        category: str | None = None,
        kind: ChannelKind | None = None,
        topic: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if category is not None:
            args["category"] = category
        if kind is not None:
            args["kind"] = kind
        if topic is not None:
            args["topic"] = topic
        return self.action("create-channel", **args)

    def create_channel_many(self, items: list[CreateChannelArgs]) -> list[dict[str, Any]]:
        """Runs `create-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-channel", items)

    def create_channel_with_id(self,
        *,
        id: str,
        name: str,
        category: str | None = None,
        kind: ChannelKind | None = None,
        topic: str | None = None,
        members: list[Any] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["name"] = name
        if category is not None:
            args["category"] = category
        if kind is not None:
            args["kind"] = kind
        if topic is not None:
            args["topic"] = topic
        if members is not None:
            args["members"] = members
        return self.action("create-channel-with-id", **args)

    def create_channel_with_id_many(self, items: list[CreateChannelWithIdArgs]) -> list[dict[str, Any]]:
        """Runs `create-channel-with-id` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-channel-with-id", items)

    def delete_category(self, *, category: str) -> None:
        """Runs the app's action."""
        return self.action("delete-category", category=category)

    def delete_category_many(self, items: list[DeleteCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `delete-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-category", items)

    def move_channel(self,
        *,
        channel: str,
        up: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("move-channel", channel=channel, up=up)

    def move_channel_many(self, items: list[MoveChannelArgs]) -> list[dict[str, Any]]:
        """Runs `move-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-channel", items)

    def move_category(self,
        *,
        category: str,
        up: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("move-category", category=category, up=up)

    def move_category_many(self, items: list[MoveCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `move-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-category", items)

    def arrange_channels(self,
        *,
        categories: list[str],
        channels: list[ChannelPlace],
    ) -> None:
        """Runs the app's action."""
        return self.action("arrange-channels", categories=categories, channels=channels)

    def arrange_channels_many(self, items: list[ArrangeChannelsArgs]) -> list[dict[str, Any]]:
        """Runs `arrange-channels` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("arrange-channels", items)

    def set_channel_category(self,
        *,
        channel: str,
        category: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        if category is not None:
            args["category"] = category
        return self.action("set-channel-category", **args)

    def set_channel_category_many(self, items: list[SetChannelCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `set-channel-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-channel-category", items)

    def rename_channel(self,
        *,
        channel: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-channel", channel=channel, name=name)

    def rename_channel_many(self, items: list[RenameChannelArgs]) -> list[dict[str, Any]]:
        """Runs `rename-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-channel", items)

    def set_channel_topic(self,
        *,
        channel: str,
        topic: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-channel-topic", channel=channel, topic=topic)

    def set_channel_topic_many(self, items: list[SetChannelTopicArgs]) -> list[dict[str, Any]]:
        """Runs `set-channel-topic` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-channel-topic", items)

    def archive_channel(self,
        *,
        channel: str,
        archived: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("archive-channel", channel=channel, archived=archived)

    def archive_channel_many(self, items: list[ArchiveChannelArgs]) -> list[dict[str, Any]]:
        """Runs `archive-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("archive-channel", items)

    def rename_category(self,
        *,
        category: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-category", category=category, name=name)

    def rename_category_many(self, items: list[RenameCategoryArgs]) -> list[dict[str, Any]]:
        """Runs `rename-category` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-category", items)

    def delete_channel(self, *, channel: str) -> None:
        """Runs the app's action."""
        return self.action("delete-channel", channel=channel)

    def delete_channel_many(self, items: list[DeleteChannelArgs]) -> list[dict[str, Any]]:
        """Runs `delete-channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-channel", items)

    def report_message(self,
        *,
        message: str,
        author: Any,
        reason: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("report-message", message=message, author=author, reason=reason)

    def report_message_many(self, items: list[ReportMessageArgs]) -> list[dict[str, Any]]:
        """Runs `report-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("report-message", items)

    def report_person(self,
        *,
        person: str,
        reason: str,
    ) -> None:
        """Runs the app's action."""
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
        """Runs the app's action."""
        return self.action("decide-reports", id=id, person=person, reports=reports, ruling=ruling)

    def decide_reports_many(self, items: list[DecideReportsArgs]) -> list[dict[str, Any]]:
        """Runs `decide-reports` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("decide-reports", items)

    def overturn_decision(self,
        *,
        decision: str,
        note: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("overturn-decision", decision=decision, note=note)

    def overturn_decision_many(self, items: list[OverturnDecisionArgs]) -> list[dict[str, Any]]:
        """Runs `overturn-decision` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("overturn-decision", items)

    def mark_read(self,
        *,
        channel: str,
        part: int,
        seen: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("mark-read", channel=channel, part=part, seen=seen)

    def mark_read_many(self, items: list[MarkReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark-read", items)

    def keep_summary(self,
        *,
        channel: str,
        through: str,
        through_ts: int,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("keep-summary", channel=channel, through=through, through_ts=through_ts, text=text)

    def keep_summary_many(self, items: list[KeepSummaryArgs]) -> list[dict[str, Any]]:
        """Runs `keep-summary` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep-summary", items)

    def submit_feedback(self,
        *,
        channel: str,
        submission: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("submit-feedback", channel=channel, submission=submission)

    def submit_feedback_many(self, items: list[SubmitFeedbackArgs]) -> list[dict[str, Any]]:
        """Runs `submit-feedback` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("submit-feedback", items)

    def triage(self,
        *,
        report: str,
        update: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("triage", report=report, update=update)

    def triage_many(self, items: list[TriageArgs]) -> list[dict[str, Any]]:
        """Runs `triage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("triage", items)

    def export_content(self,
        *,
        channel: str,
        format: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        if format is not None:
            args["format"] = format
        return self.action("export-content", **args)

    def start_call(self) -> None:
        """Runs the app's action."""
        return self.action("start-call")

    def select_input(self,
        *,
        channel: str,
        input: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("select-input", channel=channel, input=input)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"begin_chat", "begin_chat_many", "introduce", "introduce_many", "create_category", "create_category_many", "create_channel", "create_channel_many", "create_channel_with_id", "create_channel_with_id_many", "delete_category", "delete_category_many", "move_channel", "move_channel_many", "move_category", "move_category_many", "arrange_channels", "arrange_channels_many", "set_channel_category", "set_channel_category_many", "rename_channel", "rename_channel_many", "set_channel_topic", "set_channel_topic_many", "archive_channel", "archive_channel_many", "rename_category", "rename_category_many", "delete_channel", "delete_channel_many", "report_message", "report_message_many", "report_person", "report_person_many", "decide_reports", "decide_reports_many", "overturn_decision", "overturn_decision_many", "mark_read", "mark_read_many", "keep_summary", "keep_summary_many", "submit_feedback", "submit_feedback_many", "triage", "triage_many", "export_content", "start_call", "select_input"})


class Messages(Resource):
    """Messages — `chat.messages`."""
    __surface__ = "so.tilo.chat@22a1d9a3cb0ac3f0"
    @property
    def messages(self) -> list[Entry[Message]]:
        """Read only."""
        return self.fields["messages"]
    @property
    def earlier(self) -> dict[str, dict[str, int]]:
        """Read only."""
        return self.fields["earlier"]

    def post(self,
        *,
        channel: str,
        text: str,
        parent: str | None = None,
        quoted: Quote | None = None,
        body: list[ClipBlock] | None = None,
        attachments: list[Attached] | None = None,
        mentions: list[Mention] | None = None,
        written: int | None = None,
        origin: Origin | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["text"] = text
        if parent is not None:
            args["parent"] = parent
        if quoted is not None:
            args["quoted"] = quoted
        if body is not None:
            args["body"] = body
        if attachments is not None:
            args["attachments"] = attachments
        if mentions is not None:
            args["mentions"] = mentions
        if written is not None:
            args["written"] = written
        if origin is not None:
            args["origin"] = origin
        return self.action("post", **args)

    def post_many(self, items: list[PostArgs]) -> list[dict[str, Any]]:
        """Runs `post` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("post", items)

    def post_file(self,
        *,
        channel: str,
        content: str,
        name: str,
        text: str | None = None,
        attachment: Attached | None = None,
        mentions: list[Mention] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["content"] = content
        args["name"] = name
        if text is not None:
            args["text"] = text
        if attachment is not None:
            args["attachment"] = attachment
        if mentions is not None:
            args["mentions"] = mentions
        return self.action("post-file", **args)

    def post_file_many(self, items: list[PostFileArgs]) -> list[dict[str, Any]]:
        """Runs `post-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("post-file", items)

    def share_resource(self,
        *,
        channel: str,
        attached: ResourceAt,
        text: str | None = None,
        mentions: list[Mention] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["attached"] = attached
        if text is not None:
            args["text"] = text
        if mentions is not None:
            args["mentions"] = mentions
        return self.action("share-resource", **args)

    def share_resource_many(self, items: list[ShareResourceArgs]) -> list[dict[str, Any]]:
        """Runs `share-resource` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("share-resource", items)

    def share_window(self,
        *,
        channel: str,
        attached: ResourceAt,
        access: ShareAccess | None = None,
        text: str | None = None,
        mentions: list[Mention] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["attached"] = attached
        if access is not None:
            args["access"] = access
        if text is not None:
            args["text"] = text
        if mentions is not None:
            args["mentions"] = mentions
        return self.action("share-window", **args)

    def share_window_many(self, items: list[ShareWindowArgs]) -> list[dict[str, Any]]:
        """Runs `share-window` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("share-window", items)

    def stop_sharing(self, *, message: str) -> None:
        """Runs the app's action."""
        return self.action("stop-sharing", message=message)

    def stop_sharing_many(self, items: list[StopSharingArgs]) -> list[dict[str, Any]]:
        """Runs `stop-sharing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop-sharing", items)

    def set_share_access(self,
        *,
        message: str,
        access: ShareAccess,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-share-access", message=message, access=access)

    def set_share_access_many(self, items: list[SetShareAccessArgs]) -> list[dict[str, Any]]:
        """Runs `set-share-access` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-share-access", items)

    def share_open(self,
        *,
        message: str,
        open: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("share-open", message=message, open=open)

    def share_open_many(self, items: list[ShareOpenArgs]) -> list[dict[str, Any]]:
        """Runs `share-open` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("share-open", items)

    def deliver_message(self,
        *,
        channel: str,
        author: str,
        body: str,
        ts: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["author"] = author
        args["body"] = body
        if ts is not None:
            args["ts"] = ts
        return self.action("deliver-message", **args)

    def deliver_message_many(self, items: list[DeliverMessageArgs]) -> list[dict[str, Any]]:
        """Runs `deliver-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("deliver-message", items)

    def edit_message(self,
        *,
        message: str,
        text: str,
        body: list[ClipBlock] | None = None,
        mentions: list[Mention] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["text"] = text
        if body is not None:
            args["body"] = body
        if mentions is not None:
            args["mentions"] = mentions
        return self.action("edit-message", **args)

    def edit_message_many(self, items: list[EditMessageArgs]) -> list[dict[str, Any]]:
        """Runs `edit-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-message", items)

    def delete_message(self, *, message: str) -> None:
        """Runs the app's action."""
        return self.action("delete-message", message=message)

    def delete_message_many(self, items: list[DeleteMessageArgs]) -> list[dict[str, Any]]:
        """Runs `delete-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-message", items)

    def remove_message(self, *, message: str) -> None:
        """Runs the app's action."""
        return self.action("remove-message", message=message)

    def remove_message_many(self, items: list[RemoveMessageArgs]) -> list[dict[str, Any]]:
        """Runs `remove-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-message", items)

    def pin(self,
        *,
        message: str,
        pinned: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("pin", message=message, pinned=pinned)

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def retry(self,
        *,
        message: str,
        reply: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("retry", message=message, reply=reply)

    def retry_many(self, items: list[RetryArgs]) -> list[dict[str, Any]]:
        """Runs `retry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retry", items)

    def record_work(self,
        *,
        message: str,
        work: WorkUpdate,
        say: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["work"] = work
        if say is not None:
            args["say"] = say
        return self.action("record-work", **args)

    def record_work_many(self, items: list[RecordWorkArgs]) -> list[dict[str, Any]]:
        """Runs `record-work` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-work", items)

    def ask(self,
        *,
        channel: str,
        text: str,
        answers: list[Choice] | None = None,
        kind: str | None = None,
        detail: Any | None = None,
        parent: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["text"] = text
        if answers is not None:
            args["answers"] = answers
        if kind is not None:
            args["kind"] = kind
        if detail is not None:
            args["detail"] = detail
        if parent is not None:
            args["parent"] = parent
        return self.action("ask", **args)

    def ask_many(self, items: list[AskArgs]) -> list[dict[str, Any]]:
        """Runs `ask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ask", items)

    def answer_question(self,
        *,
        message: str,
        answer: str,
        device: str | None = None,
        detail: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["answer"] = answer
        if device is not None:
            args["device"] = device
        if detail is not None:
            args["detail"] = detail
        return self.action("answer-question", **args)

    def answer_question_many(self, items: list[AnswerQuestionArgs]) -> list[dict[str, Any]]:
        """Runs `answer-question` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("answer-question", items)

    def withdraw_question(self,
        *,
        message: str,
        why: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        if why is not None:
            args["why"] = why
        return self.action("withdraw-question", **args)

    def withdraw_question_many(self, items: list[WithdrawQuestionArgs]) -> list[dict[str, Any]]:
        """Runs `withdraw-question` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("withdraw-question", items)

    def settle_question(self,
        *,
        message: str,
        outcome: str,
        detail: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["outcome"] = outcome
        if detail is not None:
            args["detail"] = detail
        return self.action("settle-question", **args)

    def settle_question_many(self, items: list[SettleQuestionArgs]) -> list[dict[str, Any]]:
        """Runs `settle-question` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("settle-question", items)

    def carry_counts(self, *, earlier: dict[str, dict[str, int]]) -> None:
        """Runs the app's action."""
        return self.action("carry-counts", earlier=earlier)

    def carry_counts_many(self, items: list[CarryCountsArgs]) -> list[dict[str, Any]]:
        """Runs `carry-counts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("carry-counts", items)

    def stream_output(self,
        *,
        channel: str,
        reply: str,
        event: Any,
        name: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["reply"] = reply
        if name is not None:
            args["name"] = name
        args["event"] = event
        return self.action("stream-output", **args)

    def stream_output_many(self, items: list[StreamOutputArgs]) -> list[dict[str, Any]]:
        """Runs `stream-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stream-output", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"post", "post_many", "post_file", "post_file_many", "share_resource", "share_resource_many", "share_window", "share_window_many", "stop_sharing", "stop_sharing_many", "set_share_access", "set_share_access_many", "share_open", "share_open_many", "deliver_message", "deliver_message_many", "edit_message", "edit_message_many", "delete_message", "delete_message_many", "remove_message", "remove_message_many", "pin", "pin_many", "retry", "retry_many", "record_work", "record_work_many", "ask", "ask_many", "answer_question", "answer_question_many", "withdraw_question", "withdraw_question_many", "settle_question", "settle_question_many", "carry_counts", "carry_counts_many", "stream_output", "stream_output_many"})


class Drafts(Resource):
    """Chat drafts — `chat.drafts`."""
    __surface__ = "so.tilo.chat@22a1d9a3cb0ac3f0"
    drafts: dict[str, str]

    def start_draft(self, *, key: str) -> None:
        """Runs the app's action."""
        return self.action("start-draft", key=key)

    def start_draft_many(self, items: list[StartDraftArgs]) -> list[dict[str, Any]]:
        """Runs `start-draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start-draft", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"start_draft", "start_draft_many"})


class Learned(Resource):
    """What your agents learned — `chat.learned`."""
    __surface__ = "so.tilo.chat@22a1d9a3cb0ac3f0"
    @property
    def facts(self) -> list[Entry[Fact]]:
        """Read only."""
        return self.fields["facts"]

    def learn(self,
        *,
        text: str,
        source: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("learn", text=text, source=source)

    def learn_many(self, items: list[LearnArgs]) -> list[dict[str, Any]]:
        """Runs `learn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("learn", items)

    def correct(self,
        *,
        fact: str,
        text: str,
        source: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["fact"] = fact
        args["text"] = text
        if source is not None:
            args["source"] = source
        return self.action("correct", **args)

    def correct_many(self, items: list[CorrectArgs]) -> list[dict[str, Any]]:
        """Runs `correct` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("correct", items)

    def forget(self, *, fact: str) -> None:
        """Runs the app's action."""
        return self.action("forget", fact=fact)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"learn", "learn_many", "correct", "correct_many", "forget", "forget_many"})


class WorkspaceEmoji(Resource):
    """Workspace emoji — `tilo.workspace.emoji`."""
    __surface__ = "so.tilo.chat@22a1d9a3cb0ac3f0"
    emoji: dict[str, Emoji]

    def add_emoji(self,
        *,
        name: str,
        image: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-emoji", name=name, image=image)

    def add_emoji_many(self, items: list[AddEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `add-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-emoji", items)

    def remove_emoji(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("remove-emoji", name=name)

    def remove_emoji_many(self, items: list[RemoveEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `remove-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-emoji", items)

    def remove_any_emoji(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("remove-any-emoji", name=name)

    def remove_any_emoji_many(self, items: list[RemoveAnyEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `remove-any-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-any-emoji", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_emoji", "add_emoji_many", "remove_emoji", "remove_emoji_many", "remove_any_emoji", "remove_any_emoji_many"})


class ChatAgent(Resource):
    """Agents — `chat.agent`."""
    __surface__ = "so.tilo.chat@22a1d9a3cb0ac3f0"
    agents: list[Entry[Agent]]

    def create_agent(self,
        *,
        name: str,
        keeper: str | None = None,
        id: str | None = None,
        model: str | None = None,
        colour: int | None = None,
        mcp_client: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if keeper is not None:
            args["keeper"] = keeper
        if id is not None:
            args["id"] = id
        if model is not None:
            args["model"] = model
        if colour is not None:
            args["colour"] = colour
        if mcp_client is not None:
            args["mcp_client"] = mcp_client
        return self.action("create-agent", **args)

    def create_agent_many(self, items: list[CreateAgentArgs]) -> list[dict[str, Any]]:
        """Runs `create-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-agent", items)

    def set_agent_spend_limit(self,
        *,
        agent: str,
        limit_micros: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if limit_micros is not None:
            args["limit_micros"] = limit_micros
        return self.action("set-agent-spend-limit", **args)

    def set_agent_spend_limit_many(self, items: list[SetAgentSpendLimitArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-spend-limit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-spend-limit", items)

    def record_agent_spend(self,
        *,
        agent: str,
        day: str,
        cost_micros: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("record-agent-spend", agent=agent, day=day, cost_micros=cost_micros)

    def record_agent_spend_many(self, items: list[RecordAgentSpendArgs]) -> list[dict[str, Any]]:
        """Runs `record-agent-spend` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-agent-spend", items)

    def set_agent_on_host(self,
        *,
        agent: str,
        on_host: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-agent-on-host", agent=agent, on_host=on_host)

    def set_agent_on_host_many(self, items: list[SetAgentOnHostArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-on-host` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-on-host", items)

    def set_agent_colour(self,
        *,
        agent: str,
        colour: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-agent-colour", agent=agent, colour=colour)

    def set_agent_colour_many(self, items: list[SetAgentColourArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-colour", items)

    def rename_agent(self,
        *,
        agent: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-agent", agent=agent, name=name)

    def rename_agent_many(self, items: list[RenameAgentArgs]) -> list[dict[str, Any]]:
        """Runs `rename-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-agent", items)

    def set_agent_model(self,
        *,
        agent: str,
        model: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if model is not None:
            args["model"] = model
        return self.action("set-agent-model", **args)

    def set_agent_model_many(self, items: list[SetAgentModelArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-model` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-model", items)

    def set_agent_instruction(self,
        *,
        agent: str,
        instruction: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if instruction is not None:
            args["instruction"] = instruction
        return self.action("set-agent-instruction", **args)

    def set_agent_instruction_many(self, items: list[SetAgentInstructionArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-instruction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-instruction", items)

    def set_agent_bound(self,
        *,
        agent: str,
        bound: list[Bound],
    ) -> None:
        """Runs the app's action."""
        return self.action("set-agent-bound", agent=agent, bound=bound)

    def set_agent_bound_many(self, items: list[SetAgentBoundArgs]) -> list[dict[str, Any]]:
        """Runs `set-agent-bound` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-agent-bound", items)

    def retire_agent(self, *, agent: str) -> None:
        """Runs the app's action."""
        return self.action("retire-agent", agent=agent)

    def retire_agent_many(self, items: list[RetireAgentArgs]) -> list[dict[str, Any]]:
        """Runs `retire-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retire-agent", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"create_agent", "create_agent_many", "set_agent_spend_limit", "set_agent_spend_limit_many", "record_agent_spend", "record_agent_spend_many", "set_agent_on_host", "set_agent_on_host_many", "set_agent_colour", "set_agent_colour_many", "rename_agent", "rename_agent_many", "set_agent_model", "set_agent_model_many", "set_agent_instruction", "set_agent_instruction_many", "set_agent_bound", "set_agent_bound_many", "retire_agent", "retire_agent_many"})


