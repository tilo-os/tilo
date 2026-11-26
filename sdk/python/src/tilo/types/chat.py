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
    "Destination",
    "FeedbackReport",
    "FrameTimings",
    "Hidden",
    "Interaction",
    "Outcome",
    "Part",
    "ReadMark",
    "RecordingRef",
    "ResourceRef",
    "ReviewedFeedback",
    "ReviewedSelection",
    "ReviewedText",
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
    "Answered2",
    "Attached",
    "Cause",
    "Choice",
    "ClipAttr",
    "ClipBlock",
    "Earlier",
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
    "Starting",
    "Step",
    "StepUpdate",
    "Stopped",
    "Transition",
    "View",
    "WindowShare",
    "Withdrawn",
    "Work",
    "WorkUpdate",
    "Decision",
    "ModerationOutcome",
    "Report",
    "Reverted",
    "Ruling",
    "Emoji",
    "Example",
    "Skill",
    "Workspace",
    "Messages",
    "Drafts",
    "Moderation",
    "WorkspaceEmoji",
    "Skills",
    "BeginChatArgs",
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
    "ImportMessageArgs",
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
    "AskAgentArgs",
    "StopAgentArgs",
    "PostViewArgs",
    "AnswerWithViewArgs",
    "RewriteViewArgs",
    "CarryCountsArgs",
    "StreamOutputArgs",
    "StartDraftArgs",
    "ImportPreviousArgs",
    "ReportMessageArgs",
    "ReportPersonArgs",
    "DecideReportsArgs",
    "OverturnDecisionArgs",
    "AddEmojiArgs",
    "RemoveEmojiArgs",
    "RemoveAnyEmojiArgs",
    "WriteSkillArgs",
    "CorrectSkillArgs",
    "KeepExampleArgs",
    "DropExampleArgs",
    "ForgetSkillArgs",
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

Outcome = Literal["done", "refused", "failed"]

class Part(TypedDict):
    text: str
    hidden: Hidden | None

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

class ResourceRef(TypedDict):
    kind: str
    id: str
    label: str
    span: Span | None

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
    provider: str
    model: str
    label: str
    cost_micros: int
    tokens: int

class Answered2(TypedDict):
    answer: str
    device: str
    at: int

Attached = dict[str, Any]
"""One of resource, text, image, video, file: an object whose case the app tags it with."""

class Cause(TypedDict):
    kind: str
    label: str
    key: str
    person: str
    look_grant: str | None

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

class Earlier(TypedDict):
    version: int
    source: str
    reads: list[str]
    at: int

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
    cause: Cause | None
    stopped: Stopped | None
    view: View | None
    attempts: int

class NewMessage(TypedDict):
    id: str | None
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
    answered: Answered2 | None
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

class Starting(TypedDict):
    kind: str
    label: str
    key: str

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

class Stopped(TypedDict):
    attempt: int
    at: int

class Transition(TypedDict):
    state: AgentWorkState
    since: int

class View(TypedDict):
    source: str
    reads: list[str]
    version: int
    at: int
    earlier: list[Earlier]

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
    answered: Answered | None

class WorkUpdate(TypedDict):
    state: str
    app: str | None
    did: str | None
    about: str | None
    trace: list[StepUpdate]
    detail: str | None
    answered: Answered | None

class Decision(TypedDict):
    id: str
    person: str
    reports: list[str]
    outcome: ModerationOutcome
    until_ms: int | None
    reasoning: str
    model: str
    reverted: list[Reverted]
    by: str
    at_ms: int
    overturned_by: str | None
    overturn_note: str | None
    overturned_at_ms: int | None

ModerationOutcome = Literal["no-action", "restrict", "ban"]

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

class Reverted(TypedDict):
    app: str
    record: str
    run: str
    changes: int

class Ruling(TypedDict):
    outcome: ModerationOutcome
    until_ms: int | None
    reasoning: str
    model: str
    reverted: list[Reverted]

class Emoji(TypedDict):
    image: Any
    added_by: str
    added_at: int

class Example(TypedDict):
    id: str
    accepted: bool
    text: str
    note: str
    at: int

class Skill(TypedDict):
    name: str
    work: str
    how: str
    form: str
    model: str | None
    examples: list[Example]
    at: int
    by: str | None

BeginChatArgs = TypedDict("BeginChatArgs", {
})
"""The arguments of `begin-chat`."""

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

MarkReadArgs = TypedDict("MarkReadArgs", {
    "channel": "str",
    "part": "int",
    "seen": "int",
})
"""The arguments of `mark-read`."""

KeepSummaryArgs = TypedDict("KeepSummaryArgs", {
    "through": "str",
    "through_ts": "int",
    "text": "str",
    "conversation": "str",
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
    "id": NotRequired["str | None"],
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

ImportMessageArgs = TypedDict("ImportMessageArgs", {
    "channel": "str",
    "text": "str",
    "origin": "Origin",
    "parent": NotRequired["str | None"],
    "attachments": NotRequired["list[Attached] | None"],
})
"""The arguments of `import-message`."""

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

AskAgentArgs = TypedDict("AskAgentArgs", {
    "text": "str",
    "channel": "str",
    "cause": "Starting",
    "members": NotRequired["list[Any] | None"],
    "message": NotRequired["str | None"],
    "look_grant": NotRequired["str | None"],
})
"""The arguments of `ask-agent`."""

StopAgentArgs = TypedDict("StopAgentArgs", {
    "message": "str",
})
"""The arguments of `stop-agent`."""

PostViewArgs = TypedDict("PostViewArgs", {
    "channel": "str",
    "text": "str",
    "source": "str",
    "reads": "list[str]",
    "parent": NotRequired["str | None"],
})
"""The arguments of `post-view`."""

AnswerWithViewArgs = TypedDict("AnswerWithViewArgs", {
    "message": "str",
    "source": "str",
    "reads": "list[str]",
    "say": NotRequired["str | None"],
})
"""The arguments of `answer-with-view`."""

RewriteViewArgs = TypedDict("RewriteViewArgs", {
    "message": "str",
    "source": "str",
    "reads": "list[str]",
})
"""The arguments of `rewrite-view`."""

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

ImportPreviousArgs = TypedDict("ImportPreviousArgs", {
    "receipt": "str",
    "key": "str",
    "markdown": "str",
})
"""The arguments of `import-previous`."""

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

WriteSkillArgs = TypedDict("WriteSkillArgs", {
    "name": "str",
    "work": "str",
    "how": "str",
    "form": NotRequired["str | None"],
    "model": NotRequired["str | None"],
})
"""The arguments of `write-skill`."""

CorrectSkillArgs = TypedDict("CorrectSkillArgs", {
    "skill": "str",
    "name": NotRequired["str | None"],
    "work": NotRequired["str | None"],
    "how": NotRequired["str | None"],
    "form": NotRequired["str | None"],
    "model": NotRequired["str | None"],
})
"""The arguments of `correct-skill`."""

KeepExampleArgs = TypedDict("KeepExampleArgs", {
    "skill": "str",
    "text": "str",
    "accepted": "bool",
    "note": NotRequired["str | None"],
})
"""The arguments of `keep-example`."""

DropExampleArgs = TypedDict("DropExampleArgs", {
    "skill": "str",
    "example": "str",
})
"""The arguments of `drop-example`."""

ForgetSkillArgs = TypedDict("ForgetSkillArgs", {
    "skill": "str",
})
"""The arguments of `forget-skill`."""

class Workspace(Resource):
    """Chat — `chat.workspace`."""
    __surface__ = "so.tilo.chat@073d1a40a84e8db1"
    @property
    def categories(self) -> list[Entry[Category]]:
        """Read only."""
        return self.fields["categories"]
    @property
    def channels(self) -> list[Entry[Conversation]]:
        """Read only."""
        return self.fields["channels"]
    read: dict[str, dict[str, ReadMark]]
    feedback: list[Entry[FeedbackReport]]
    @property
    def files(self) -> WorkspaceFiles:
        """Read only."""
        return self.fields["files"]
    @property
    def summaries(self) -> dict[str, dict[str, Summary]]:
        """Read only."""
        return self.fields["summaries"]

    def begin_chat(self) -> None:
        """Runs the app's action."""
        return self.action("begin-chat")

    def begin_chat_many(self, items: list[BeginChatArgs]) -> list[dict[str, Any]]:
        """Runs `begin-chat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin-chat", items)

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
        through: str,
        through_ts: int,
        text: str,
        conversation: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("keep-summary", through=through, through_ts=through_ts, text=text, conversation=conversation)

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
    __actions__ = frozenset({"begin_chat", "begin_chat_many", "create_category", "create_category_many", "create_channel", "create_channel_many", "create_channel_with_id", "create_channel_with_id_many", "delete_category", "delete_category_many", "move_channel", "move_channel_many", "move_category", "move_category_many", "arrange_channels", "arrange_channels_many", "set_channel_category", "set_channel_category_many", "rename_channel", "rename_channel_many", "set_channel_topic", "set_channel_topic_many", "archive_channel", "archive_channel_many", "rename_category", "rename_category_many", "delete_channel", "delete_channel_many", "mark_read", "mark_read_many", "keep_summary", "keep_summary_many", "submit_feedback", "submit_feedback_many", "triage", "triage_many", "export_content", "start_call", "select_input"})


class Messages(Resource):
    """Messages — `chat.messages`."""
    __surface__ = "so.tilo.chat@073d1a40a84e8db1"
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
        id: str | None = None,
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
        if id is not None:
            args["id"] = id
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

    def import_message(self,
        *,
        channel: str,
        text: str,
        origin: Origin,
        parent: str | None = None,
        attachments: list[Attached] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["text"] = text
        args["origin"] = origin
        if parent is not None:
            args["parent"] = parent
        if attachments is not None:
            args["attachments"] = attachments
        return self.action("import-message", **args)

    def import_message_many(self, items: list[ImportMessageArgs]) -> list[dict[str, Any]]:
        """Runs `import-message` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-message", items)

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
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["answer"] = answer
        if device is not None:
            args["device"] = device
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

    def ask_agent(self,
        *,
        text: str,
        channel: str,
        cause: Starting,
        members: list[Any] | None = None,
        message: str | None = None,
        look_grant: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["text"] = text
        args["channel"] = channel
        args["cause"] = cause
        if members is not None:
            args["members"] = members
        if message is not None:
            args["message"] = message
        if look_grant is not None:
            args["look_grant"] = look_grant
        return self.action("ask-agent", **args)

    def ask_agent_many(self, items: list[AskAgentArgs]) -> list[dict[str, Any]]:
        """Runs `ask-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ask-agent", items)

    def stop_agent(self, *, message: str) -> None:
        """Runs the app's action."""
        return self.action("stop-agent", message=message)

    def stop_agent_many(self, items: list[StopAgentArgs]) -> list[dict[str, Any]]:
        """Runs `stop-agent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop-agent", items)

    def post_view(self,
        *,
        channel: str,
        text: str,
        source: str,
        reads: list[str],
        parent: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["text"] = text
        args["source"] = source
        args["reads"] = reads
        if parent is not None:
            args["parent"] = parent
        return self.action("post-view", **args)

    def post_view_many(self, items: list[PostViewArgs]) -> list[dict[str, Any]]:
        """Runs `post-view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("post-view", items)

    def answer_with_view(self,
        *,
        message: str,
        source: str,
        reads: list[str],
        say: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["source"] = source
        args["reads"] = reads
        if say is not None:
            args["say"] = say
        return self.action("answer-with-view", **args)

    def answer_with_view_many(self, items: list[AnswerWithViewArgs]) -> list[dict[str, Any]]:
        """Runs `answer-with-view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("answer-with-view", items)

    def rewrite_view(self,
        *,
        message: str,
        source: str,
        reads: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("rewrite-view", message=message, source=source, reads=reads)

    def rewrite_view_many(self, items: list[RewriteViewArgs]) -> list[dict[str, Any]]:
        """Runs `rewrite-view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rewrite-view", items)

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
    __actions__ = frozenset({"post", "post_many", "post_file", "post_file_many", "share_resource", "share_resource_many", "share_window", "share_window_many", "stop_sharing", "stop_sharing_many", "set_share_access", "set_share_access_many", "share_open", "share_open_many", "deliver_message", "deliver_message_many", "import_message", "import_message_many", "edit_message", "edit_message_many", "delete_message", "delete_message_many", "remove_message", "remove_message_many", "pin", "pin_many", "retry", "retry_many", "record_work", "record_work_many", "ask", "ask_many", "answer_question", "answer_question_many", "withdraw_question", "withdraw_question_many", "settle_question", "settle_question_many", "ask_agent", "ask_agent_many", "stop_agent", "stop_agent_many", "post_view", "post_view_many", "answer_with_view", "answer_with_view_many", "rewrite_view", "rewrite_view_many", "carry_counts", "carry_counts_many", "stream_output", "stream_output_many"})


class Drafts(Resource):
    """Chat drafts — `chat.drafts`."""
    __surface__ = "so.tilo.chat@073d1a40a84e8db1"
    drafts: dict[str, str]
    @property
    def imported(self) -> list[str]:
        """Read only."""
        return self.fields["imported"]

    def start_draft(self, *, key: str) -> None:
        """Runs the app's action."""
        return self.action("start-draft", key=key)

    def start_draft_many(self, items: list[StartDraftArgs]) -> list[dict[str, Any]]:
        """Runs `start-draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start-draft", items)

    def import_previous(self,
        *,
        receipt: str,
        key: str,
        markdown: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("import-previous", receipt=receipt, key=key, markdown=markdown)

    def import_previous_many(self, items: list[ImportPreviousArgs]) -> list[dict[str, Any]]:
        """Runs `import-previous` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-previous", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"start_draft", "start_draft_many", "import_previous", "import_previous_many"})


class Moderation(Resource):
    """Moderation — `tilo.moderation`."""
    __surface__ = "so.tilo.chat@073d1a40a84e8db1"
    @property
    def reports(self) -> list[Entry[Report]]:
        """Read only."""
        return self.fields["reports"]
    @property
    def decisions(self) -> list[Entry[Decision]]:
        """Read only."""
        return self.fields["decisions"]

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
        person: Any,
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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"report_message", "report_message_many", "report_person", "report_person_many", "decide_reports", "decide_reports_many", "overturn_decision", "overturn_decision_many"})


class WorkspaceEmoji(Resource):
    """Workspace emoji — `tilo.workspace.emoji`."""
    __surface__ = "so.tilo.chat@073d1a40a84e8db1"
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


class Skills(Resource):
    """Workspace Skills — `chat.skills`."""
    __surface__ = "so.tilo.chat@073d1a40a84e8db1"
    @property
    def skills(self) -> list[Entry[Skill]]:
        """Read only."""
        return self.fields["skills"]

    def write_skill(self,
        *,
        name: str,
        work: str,
        how: str,
        form: str | None = None,
        model: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["work"] = work
        args["how"] = how
        if form is not None:
            args["form"] = form
        if model is not None:
            args["model"] = model
        return self.action("write-skill", **args)

    def write_skill_many(self, items: list[WriteSkillArgs]) -> list[dict[str, Any]]:
        """Runs `write-skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("write-skill", items)

    def correct_skill(self,
        *,
        skill: str,
        name: str | None = None,
        work: str | None = None,
        how: str | None = None,
        form: str | None = None,
        model: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["skill"] = skill
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
        return self.action("correct-skill", **args)

    def correct_skill_many(self, items: list[CorrectSkillArgs]) -> list[dict[str, Any]]:
        """Runs `correct-skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("correct-skill", items)

    def keep_example(self,
        *,
        skill: str,
        text: str,
        accepted: bool,
        note: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["skill"] = skill
        args["text"] = text
        args["accepted"] = accepted
        if note is not None:
            args["note"] = note
        return self.action("keep-example", **args)

    def keep_example_many(self, items: list[KeepExampleArgs]) -> list[dict[str, Any]]:
        """Runs `keep-example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep-example", items)

    def drop_example(self,
        *,
        skill: str,
        example: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("drop-example", skill=skill, example=example)

    def drop_example_many(self, items: list[DropExampleArgs]) -> list[dict[str, Any]]:
        """Runs `drop-example` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drop-example", items)

    def forget_skill(self, *, skill: str) -> None:
        """Runs the app's action."""
        return self.action("forget-skill", skill=skill)

    def forget_skill_many(self, items: list[ForgetSkillArgs]) -> list[dict[str, Any]]:
        """Runs `forget-skill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget-skill", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"write_skill", "write_skill_many", "correct_skill", "correct_skill_many", "keep_example", "keep_example_many", "drop_example", "drop_example_many", "forget_skill", "forget_skill_many"})


