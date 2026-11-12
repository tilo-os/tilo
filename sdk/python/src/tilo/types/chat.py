"""Types for Chat (`so.tilo.chat`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `chat.workspace` at `chat/<resource>`, read with
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
    "TextKind",
    "Triage",
    "TriageUpdate",
    "WorkspaceFiles",
    "AgentFinish",
    "AgentStepOutcome",
    "AgentWorkState",
    "Anchor",
    "Attached",
    "ClipAttr",
    "ClipBlock",
    "MarkSpan",
    "MarkValue",
    "Mention",
    "Message",
    "ProviderStream",
    "ResourceAt",
    "Revision",
    "ShareAccess",
    "ShownAs",
    "ShownPersona",
    "Step",
    "StepUpdate",
    "Transition",
    "WindowShare",
    "Work",
    "WorkUpdate",
    "Emoji",
    "Agent",
    "AgentState",
    "Bound",
    "Spent",
    "Workspace",
    "Messages",
    "Drafts",
    "WorkspaceEmoji",
    "ChatAgent",
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
    participants: list[str]

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
    room: str
    provider: str
    channel: str

class FeedbackReport(TypedDict):
    submission: Submission | None
    author: str
    ts: int
    triage: list[Triage]
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
    reports: list[Report]
    decisions: list[Decision]

Outcome = Literal["done", "refused", "failed"]

Outcome2 = Literal["no_action", "restrict", "ban"]

class Part(TypedDict):
    text: str
    hidden: Hidden | None

class Person(TypedDict):
    id: str
    name: str
    initials: str
    color: int
    status: Presence
    agent: bool

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

SpanUnit = Literal["chars", "bytes"]

class Submission(TypedDict):
    id: str
    destination: Destination
    feedback: ReviewedFeedback

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
"""One of item, items, text: an object whose case the app tags it with."""

Attached = dict[str, Any]
"""One of resource, text, image, video, file: an object whose case the app tags it with."""

ClipAttr = dict[str, Any]
"""One of bool, int, str: an object whose case the app tags it with."""

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
"""One of bool, int, str: an object whose case the app tags it with."""

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
    reactions: Any
    mentions: list[Mention]
    attachments: list[Attached]
    pinned: bool
    work: Work | None
    appearance: ShownAs | None
    share: WindowShare | None
    attempts: int

class ProviderStream(TypedDict):
    run: str
    sequence: int
    reasoning: str
    progress: str
    finish: AgentFinish | None
    detail: str

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

class Work(TypedDict):
    stream: ProviderStream | None
    state: Transition | None
    trace: list[Step]

class WorkUpdate(TypedDict):
    state: str
    app: str | None
    did: str | None
    about: str | None
    trace: list[StepUpdate]

class Emoji(TypedDict):
    image: Any
    added_by: str
    added_at: int

class Agent(TypedDict):
    id: str
    name: str
    colour: int | None
    keeper: str
    model: str | None
    bound: list[Bound]
    spend_limit_micros: int | None
    spent: Spent | None
    state: AgentState
    created_at: int

AgentState = Literal["active", "retired"]

class Bound(TypedDict):
    node: str
    kinds: list[str]
    answer: str

class Spent(TypedDict):
    day: str
    cost_micros: int

class Workspace(Resource):
    """Chat — `chat.workspace`."""
    __surface__ = "so.tilo.chat@4d5b813b3ee16427"
    @property
    def categories(self) -> list[Category]:
        """Read only."""
        return self.fields["categories"]
    @property
    def channels(self) -> list[Conversation]:
        """Read only."""
        return self.fields["channels"]
    people: list[Person]
    agent_names: dict[str, str]
    read: dict[str, dict[str, ReadMark]]
    feedback: list[FeedbackReport]
    moderation: Moderation
    @property
    def files(self) -> WorkspaceFiles:
        """Read only."""
        return self.fields["files"]

    def begin_chat(self,
        *,
        name: str,
        initials: str,
        color: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("begin-chat", name=name, initials=initials, color=color)

    def introduce(self,
        *,
        name: str,
        initials: str,
        color: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("introduce", name=name, initials=initials, color=color)

    def name_agent(self,
        *,
        participant: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("name-agent", participant=participant, name=name)

    def create_category(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("create-category", name=name)

    def create_channel(self,
        *,
        name: str,
        category: str | None = None,
        kind: ChannelKind | None = None,
        topic: str | None = None,
    ) -> Run | None:
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

    def create_channel_with_id(self,
        *,
        id: str,
        name: str,
        category: str | None = None,
        kind: ChannelKind | None = None,
        topic: str | None = None,
        participants: list[str] | None = None,
    ) -> Run | None:
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
        if participants is not None:
            args["participants"] = participants
        return self.action("create-channel-with-id", **args)

    def delete_category(self, *, category: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-category", category=category)

    def move_channel(self,
        *,
        channel: str,
        up: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("move-channel", channel=channel, up=up)

    def move_category(self,
        *,
        category: str,
        up: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("move-category", category=category, up=up)

    def arrange_channels(self,
        *,
        categories: list[str],
        channels: list[ChannelPlace],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("arrange-channels", categories=categories, channels=channels)

    def set_channel_category(self,
        *,
        channel: str,
        category: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        if category is not None:
            args["category"] = category
        return self.action("set-channel-category", **args)

    def rename_channel(self,
        *,
        channel: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-channel", channel=channel, name=name)

    def set_channel_topic(self,
        *,
        channel: str,
        topic: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-channel-topic", channel=channel, topic=topic)

    def archive_channel(self,
        *,
        channel: str,
        archived: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("archive-channel", channel=channel, archived=archived)

    def rename_category(self,
        *,
        category: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-category", category=category, name=name)

    def delete_channel(self, *, channel: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-channel", channel=channel)

    def report_message(self,
        *,
        message: str,
        author: str,
        reason: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("report-message", message=message, author=author, reason=reason)

    def report_person(self,
        *,
        person: str,
        reason: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("report-person", person=person, reason=reason)

    def decide_reports(self,
        *,
        id: str,
        person: str,
        reports: list[str],
        ruling: Ruling,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("decide-reports", id=id, person=person, reports=reports, ruling=ruling)

    def overturn_decision(self,
        *,
        decision: str,
        note: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("overturn-decision", decision=decision, note=note)

    def mark_read(self,
        *,
        channel: str,
        part: int,
        seen: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("mark-read", channel=channel, part=part, seen=seen)

    def submit_feedback(self,
        *,
        channel: str,
        submission: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("submit-feedback", channel=channel, submission=submission)

    def triage(self,
        *,
        report: str,
        update: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("triage", report=report, update=update)

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


class Messages(Resource):
    """Messages — `chat.messages`."""
    __surface__ = "so.tilo.chat@4d5b813b3ee16427"
    @property
    def messages(self) -> list[Message]:
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
        body: list[Any] | None = None,
        attachments: list[Any] | None = None,
        mentions: list[Any] | None = None,
        written: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["text"] = text
        if parent is not None:
            args["parent"] = parent
        if body is not None:
            args["body"] = body
        if attachments is not None:
            args["attachments"] = attachments
        if mentions is not None:
            args["mentions"] = mentions
        if written is not None:
            args["written"] = written
        return self.action("post", **args)

    def post_file(self,
        *,
        channel: str,
        content: str,
        name: str,
        text: str | None = None,
        attachment: Attached | None = None,
        mentions: list[Mention] | None = None,
    ) -> Run | None:
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

    def share_resource(self,
        *,
        channel: str,
        attached: ResourceAt,
        text: str | None = None,
        mentions: list[Mention] | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["attached"] = attached
        if text is not None:
            args["text"] = text
        if mentions is not None:
            args["mentions"] = mentions
        return self.action("share-resource", **args)

    def share_window(self,
        *,
        channel: str,
        attached: ResourceAt,
        access: ShareAccess | None = None,
        text: str | None = None,
        mentions: list[Mention] | None = None,
    ) -> Run | None:
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

    def stop_sharing(self, *, message: str) -> Run | None:
        """Runs the app's action."""
        return self.action("stop-sharing", message=message)

    def set_share_access(self,
        *,
        message: str,
        access: ShareAccess,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-share-access", message=message, access=access)

    def share_open(self,
        *,
        message: str,
        open: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("share-open", message=message, open=open)

    def deliver_message(self,
        *,
        channel: str,
        author: str,
        body: str,
        ts: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["author"] = author
        args["body"] = body
        if ts is not None:
            args["ts"] = ts
        return self.action("deliver-message", **args)

    def edit_message(self,
        *,
        message: str,
        text: str,
        body: list[ClipBlock] | None = None,
        mentions: list[Mention] | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["text"] = text
        if body is not None:
            args["body"] = body
        if mentions is not None:
            args["mentions"] = mentions
        return self.action("edit-message", **args)

    def delete_message(self, *, message: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-message", message=message)

    def remove_message(self, *, message: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-message", message=message)

    def pin(self,
        *,
        message: str,
        pinned: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("pin", message=message, pinned=pinned)

    def retry(self,
        *,
        message: str,
        reply: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("retry", message=message, reply=reply)

    def record_work(self,
        *,
        message: str,
        work: WorkUpdate,
        say: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["message"] = message
        args["work"] = work
        if say is not None:
            args["say"] = say
        return self.action("record-work", **args)

    def carry_counts(self, *, earlier: dict[str, dict[str, int]]) -> Run | None:
        """Runs the app's action."""
        return self.action("carry-counts", earlier=earlier)

    def stream_output(self,
        *,
        channel: str,
        reply: str,
        event: Any,
        name: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["channel"] = channel
        args["reply"] = reply
        if name is not None:
            args["name"] = name
        args["event"] = event
        return self.action("stream-output", **args)


class Drafts(Resource):
    """Chat drafts — `chat.drafts`."""
    __surface__ = "so.tilo.chat@4d5b813b3ee16427"
    drafts: dict[str, str]

    def start_draft(self, *, key: str) -> Run | None:
        """Runs the app's action."""
        return self.action("start-draft", key=key)


class WorkspaceEmoji(Resource):
    """Workspace emoji — `tilo.workspace.emoji`."""
    __surface__ = "so.tilo.chat@4d5b813b3ee16427"
    emoji: dict[str, Emoji]

    def add_emoji(self,
        *,
        name: str,
        image: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-emoji", name=name, image=image)

    def remove_emoji(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-emoji", name=name)

    def remove_any_emoji(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-any-emoji", name=name)


class ChatAgent(Resource):
    """Agents — `chat.agent`."""
    __surface__ = "so.tilo.chat@4d5b813b3ee16427"
    agents: list[Agent]

    def create_agent(self,
        *,
        name: str,
        keeper: str | None = None,
        id: str | None = None,
        model: str | None = None,
        colour: int | None = None,
    ) -> Run | None:
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
        return self.action("create-agent", **args)

    def set_agent_spend_limit(self,
        *,
        agent: str,
        limit_micros: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if limit_micros is not None:
            args["limit_micros"] = limit_micros
        return self.action("set-agent-spend-limit", **args)

    def record_agent_spend(self,
        *,
        agent: str,
        day: str,
        cost_micros: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record-agent-spend", agent=agent, day=day, cost_micros=cost_micros)

    def set_agent_colour(self,
        *,
        agent: str,
        colour: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-agent-colour", agent=agent, colour=colour)

    def rename_agent(self,
        *,
        agent: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-agent", agent=agent, name=name)

    def set_agent_model(self,
        *,
        agent: str,
        model: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["agent"] = agent
        if model is not None:
            args["model"] = model
        return self.action("set-agent-model", **args)

    def set_agent_bound(self,
        *,
        agent: str,
        bound: list[Bound],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-agent-bound", agent=agent, bound=bound)

    def retire_agent(self, *, agent: str) -> Run | None:
        """Runs the app's action."""
        return self.action("retire-agent", agent=agent)


