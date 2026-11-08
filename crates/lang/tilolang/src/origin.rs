//! Outside origins: the services whose resources a type can hold (convergence.md, "Origins"
//! and "Outside services").
//!
//! A type names the kind of an outside service's resource it holds with
//! `type Message (origin: imap.message) { ... }`. The system keeps such resources in step with
//! the service: it gets and searches them through the service's capabilities, which the
//! platform implements, and writes what the service says into the fields the kind defines.
//! The type declares the fields of the kind it shows, with the kind's types, and may add
//! fields of its own, which the service never touches.
//!
//! A kind's fields are written by the service alone, except those people change (a message's
//! `unread`, `flagged` and `mailbox`): a change of one applies at once and becomes final when
//! the service accepts it. Code writing any other field of the kind is refused (E113), except
//! in a scenario, whose seed data stands in for the service.
//!
//! Some kinds are the origin's to derive rather than to save: the time, the desk, who is here,
//! the workspaces on the device. The origin makes them again from nothing at every start, so
//! they are never put in a collaborative record: [their resources live in the store's unsaved
//! layer](OriginKind::derived), where a change reaches the views at once and leaves no change
//! behind to be committed, synced or replayed.

/// A field of an origin's kind.
#[derive(Clone, Copy, Debug)]
pub struct OriginField {
    pub name: &'static str,
    /// Its type as the checker shows types: `Text`, `Bool`, `Int`, `Time`, `Set<Text>`, or
    /// `Collection<kind>` for the resources of another kind of the same origin.
    pub ty: &'static str,
    /// Whether people change it (the service is told), or only the service writes it.
    pub people: bool,
    pub doc: &'static str,
}

/// A kind of resource an origin holds.
#[derive(Clone, Copy, Debug)]
pub struct OriginKind {
    /// The dotted name a type declares: `imap.message`.
    pub name: &'static str,
    pub doc: &'static str,
    pub fields: &'static [OriginField],
    /// The origin derives these resources from what it holds: it makes them again from
    /// nothing each time it starts, so they are never saved to a collaborative record. They
    /// live in the store's unsaved layer, where a change reaches the views without being
    /// committed: written, remembered, and gone at the next start, when the origin writes them
    /// again.
    pub derived: bool,
}

const fn field(name: &'static str, ty: &'static str, doc: &'static str) -> OriginField {
    OriginField {
        name,
        ty,
        people: false,
        doc,
    }
}

const fn people(name: &'static str, ty: &'static str, doc: &'static str) -> OriginField {
    OriginField {
        name,
        ty,
        people: true,
        doc,
    }
}

/// The capabilities an action may hand an outside effect to with `outside(capability, ...)`:
/// `post`, the development log, and each origin's own. The session takes none: a shell calls
/// its actions as an app's (`use so.tilo.session as session`).
pub const CAPABILITIES: &[(&str, &str)] = &[
    ("post", "Posts a payload; the development sink logs it."),
    (
        "smtp.send",
        "Sends a message from a mail account of the workspace's settings: (account, to, cc, \
         subject, body, the Message-ID it answers, the References it continues).",
    ),
    (
        "youtube.search",
        "Searches YouTube's catalogue for the query, as typed: (the query). The channels, \
         playlists and videos it answers are written where the search's field says.",
    ),
    (
        "youtube.channel",
        "Asks YouTube for a channel's videos: (the channel's key).",
    ),
    (
        "youtube.subscriptions",
        "Asks YouTube for the newest videos of the channels the person follows: (the keys of \
         the channels followed, in the feed's order), so a feed of a person who follows none \
         asks nothing.",
    ),
    (
        "youtube.trending",
        "Asks YouTube for the videos it lists as trending: (nothing).",
    ),
    (
        "youtube.stream",
        "Asks YouTube where a video's bytes play from: (the video's key). The address is written \
         into the video's `stream`, which the player opens.",
    ),
];

/// Every kind of every origin the platform has.
pub const KINDS: &[OriginKind] = &[
    OriginKind {
        name: "session.desk",
        doc: "This device's desk as the session holds it, for the person at it: the time in their \
              style, who they are, what they were told, the workspaces on this device and the \
              tasks running for them. A shell's bar reads it; the session writes it.",
        derived: true,
        fields: &[
            field(
                "person",
                "Text",
                "The person at this device, by their name.",
            ),
            field(
                "clock",
                "Text",
                "The time of day in the person's style, to the minute.",
            ),
            field(
                "timer",
                "Text",
                "The timer or stopwatch Clocks runs, by its name: its label, or `Timer` or \
                 `Stopwatch` when the app named none; empty when none floats.",
            ),
            field(
                "timer_left",
                "Text",
                "What the timer reads: what it has left, or what the stopwatch has run.",
            ),
            field(
                "timer_paused",
                "Bool",
                "Whether the timer or stopwatch is paused.",
            ),
            field(
                "timer_stopwatch",
                "Bool",
                "Whether the running activity is a stopwatch rather than a timer.",
            ),
            field(
                "ask",
                "Text",
                "The question the session puts to the person about themselves; empty when none \
                 waits.",
            ),
            field("ask_detail", "Text", "What answering the question brings."),
            field(
                "ask_answer",
                "Text",
                "The question's answer, as its button says it.",
            ),
            field(
                "ask_open",
                "Bool",
                "Whether the question stands open on the person's avatar.",
            ),
            field(
                "weekday",
                "Text",
                "Today's weekday, short, in the person's language.",
            ),
            field(
                "date",
                "Text",
                "Today's month and day, in the person's language.",
            ),
            field(
                "quiet",
                "Bool",
                "Whether what the person is told now is held back from showing (Do Not Disturb, \
                 presenting).",
            ),
            field("workspace", "Text", "The name of the workspace in front."),
            field(
                "strip",
                "Text",
                "The name of the strip in front in that workspace; empty without strips.",
            ),
            field(
                "workspaces",
                "Collection<session.workspace>",
                "The workspaces on this device, in the desk's order.",
            ),
            field(
                "tasks",
                "Collection<session.task>",
                "The tasks running for the person now (an agent's, a script's).",
            ),
            field(
                "people",
                "Collection<session.person>",
                "The others here in the workspace in front.",
            ),
            field(
                "here",
                "Text",
                "The surface the desk shows over its windows: `none`, `desk`, `overview`, \
                 `switcher`, `launcher`, `centre` or `composer`.",
            ),
            field(
                "agents",
                "Collection<session.agent>",
                "The person's agents here, and what each is on and did.",
            ),
            field(
                "rooms",
                "Collection<session.room>",
                "The person's other workspaces, with who is in each and what it holds.",
            ),
            field(
                "notices",
                "Collection<session.notice>",
                "What the person was told, newest first.",
            ),
            field(
                "banner",
                "Text",
                "The notice shown as a banner over the desk now, by its id; empty for none.",
            ),
            field(
                "spotlight",
                "Text",
                "What the person last typed in the shell's search, so the shell opens where they \
                 left off.",
            ),
            field(
                "notice_actions",
                "Collection<session.notice_action>",
                "Each action a notice offers, naming the notice it is on, in its sender's order.",
            ),
            field(
                "windows",
                "Collection<session.window>",
                "The windows of every strip of the workspace in front, most recently used first.",
            ),
            field(
                "tray",
                "Collection<session.tray_item>",
                "The running applications' tray items, in the order they registered.",
            ),
            field(
                "runs",
                "Collection<session.run>",
                "The runs of what the person asked the session's agents, oldest first.",
            ),
            field(
                "run_entries",
                "Collection<session.run_entry>",
                "Each run's conversation, entry by entry, naming the run it is of, oldest \
                 first.",
            ),
            field(
                "entry_refs",
                "Collection<session.entry_ref>",
                "What each program of a run's conversation printed and changed, naming the \
                 entry it is of, in the program's order.",
            ),
        ],
    },
    OriginKind {
        name: "session.run",
        doc: "A run of what the person asked one of the session's agents: the conversation the \
              agent and the person have in it.",
        derived: true,
        fields: &[
            field(
                "key",
                "Text",
                "The run's id, as its resource `agent-run:<id>` names it.",
            ),
            field("title", "Text", "What was asked, in a line."),
            field("agent", "Text", "The agent it asked, by its id."),
            field(
                "phase",
                "Text",
                "Where it is: `approved`, `accepted`, `planning`, `executing`, `waiting`, \
                 `done`, `failed` or `stopped`.",
            ),
            field("active", "Bool", "Whether it is still going."),
            field(
                "live",
                "Text",
                "What its agent says now, as the model streams it; empty between turns.",
            ),
        ],
    },
    OriginKind {
        name: "session.run_entry",
        doc: "One entry of a run's conversation: what the person or the agent said, or a \
              program either of them ran, with what came of it.",
        derived: true,
        fields: &[
            field(
                "key",
                "Text",
                "The entry, as `<run>/<number>`; its number never changes.",
            ),
            field("run", "Text", "The run it is of, by its key."),
            field("seq", "Int", "Its number in the run, from 1."),
            field(
                "author",
                "Text",
                "Who said or ran it: the person by their id, or the agent as `agent:<id>`.",
            ),
            field("by_agent", "Bool", "Whether the agent said or ran it."),
            field(
                "step",
                "Bool",
                "Whether it is a program, rather than words.",
            ),
            field("text", "Text", "The words, or the program as it ran."),
            field(
                "outcome",
                "Text",
                "How a program ended: `done`, `rejected`, `preview`, `asked` or `failed`; \
                 empty for words.",
            ),
            field(
                "answer",
                "Text",
                "What a program answered, as tilolang, a line per line.",
            ),
        ],
    },
    OriginKind {
        name: "session.entry_ref",
        doc: "A resource a program of a run's conversation printed (a reference) or changed \
              (an undo row).",
        derived: true,
        fields: &[
            field("key", "Text", "As `<entry>/<number>`."),
            field("entry", "Text", "The entry it is of, by its key."),
            field(
                "changed",
                "Bool",
                "Whether the program changed it, rather than printed it.",
            ),
            field(
                "resource",
                "Text",
                "The resource, by its id in its app, as the arrangement's `place` takes it.",
            ),
            field("app_id", "Text", "The app that keeps it, by its id."),
            field("type_kind", "Text", "Its type, as its app publishes it."),
            field("label", "Text", "What it is called."),
            field(
                "change",
                "Text",
                "What changed it, as its app labels the action; empty for a reference.",
            ),
        ],
    },
    OriginKind {
        name: "session.window",
        doc: "A window of the desk: a tab of a strip in the workspace in front.",
        derived: true,
        fields: &[
            field(
                "key",
                "Text",
                "The tab's id, as moving or closing it names it.",
            ),
            field("title", "Text", "Its title."),
            field("origin_app", "Text", "The app it shows, by its name here."),
            field("icon", "Text", "The app's glyph, as the icon set names it."),
            field("strip", "Text", "The strip it is in, by its name."),
            field("workspace", "Text", "The workspace it is in, by its name."),
            field(
                "recency",
                "Int",
                "Where it stands in the walk a switcher takes: 1 is the window in front, then \
                 the ones it was in front of, then the rest along their strips.",
            ),
            field(
                "seen",
                "Int",
                "How many changes of its the person has not seen.",
            ),
            field("focused", "Bool", "Whether it has focus now."),
            field(
                "preview",
                "Text",
                "Whether it may be pictured: `live`, `withheld` (it holds secrets) or `none`.",
            ),
        ],
    },
    OriginKind {
        name: "session.agent",
        doc: "An agent here for the person, or that acted for them: a collaborator shown as one \
              when the person's setting says so, and a task's own run.",
        derived: true,
        fields: &[
            field("key", "Text", "The agent's id."),
            field("name", "Text", "Its name here."),
            field(
                "state",
                "Text",
                "`running`, `waiting` (for the person) or `done`.",
            ),
            field("doing", "Text", "What it is on, in a few words."),
            field(
                "mark",
                "Text",
                "What it last did to what the person is looking at, in a few words.",
            ),
            field(
                "unseen",
                "Int",
                "How many changes of its the person has not seen.",
            ),
            field(
                "seat",
                "Bool",
                "Whether it is seated at the person's agent door now, a client they admitted, \
                 rather than a run's own agent.",
            ),
        ],
    },
    OriginKind {
        name: "session.room",
        doc: "A workspace the person is in, as the workspace overview names it.",
        derived: true,
        fields: &[
            field(
                "key",
                "Text",
                "The workspace's id, as switching to it names it.",
            ),
            field("name", "Text", "Its name."),
            field("current", "Bool", "Whether it is the one in front."),
            field(
                "members",
                "Text",
                "Who is in it, by their names, in the person's language.",
            ),
            field("present", "Int", "How many people are in it now."),
            field(
                "activity",
                "Text",
                "What happened in it last, in a few words.",
            ),
        ],
    },
    OriginKind {
        name: "session.notice",
        doc: "Something a sender told the person at this device: an app, whatever language it is \
              written in, or the agent task that did. Every app's telling meets here, so the \
              device shows them as one. How they are counted, grouped and shown is the shell's: \
              what a notice is stays the sender's.",
        derived: true,
        fields: &[
            field("key", "Text", "The notice's id, as acting on it names it."),
            field("title", "Text", "What happened, in a short sentence."),
            field("body", "Text", "What it says, under the title."),
            field(
                "origin_app",
                "Text",
                "The app that sent it, or the agent's task.",
            ),
            field(
                "kind",
                "Text",
                "Its kind: `info`, `success`, `warning` or `error`.",
            ),
            field("seen", "Bool", "Whether the person has seen it."),
        ],
    },
    OriginKind {
        name: "session.notice_action",
        doc: "One action a notice offers, as its sender named it: a message's Reply, an \
              import's Open.",
        derived: true,
        fields: &[
            field("notice", "Text", "The notice it is on, by its id."),
            field("key", "Text", "The key the sender answers by."),
            field("label", "Text", "What it says, in the sender's words."),
        ],
    },
    OriginKind {
        name: "session.arrangement",
        doc: "The person's own arrangement: what is open where on their desk (shell.md §2), as \
              the session keeps it in their own record. A shell's views read it; every change \
              is one of the arranging actions on `shell:arrangement`, so nothing writes it \
              directly.",
        derived: true,
        fields: &[
            field(
                "preset",
                "Text",
                "Which preset the person shows: `strips`, `frames` or `spaces`.",
            ),
            field(
                "focused",
                "Text",
                "The strip in front, by its id; empty for none.",
            ),
            field(
                "workspaces",
                "Collection<session.arrangement_workspace>",
                "The workspaces the desk shows.",
            ),
            field(
                "rows",
                "Collection<session.arrangement_row>",
                "The desk's rows, each a strip across a workspace.",
            ),
            field(
                "strips",
                "Collection<session.arrangement_strip>",
                "The strips, each a column of places.",
            ),
            field(
                "places",
                "Collection<session.arrangement_place>",
                "Every place, showing a resource through a view; each names the strip it is in.",
            ),
        ],
    },
    OriginKind {
        name: "session.arrangement_workspace",
        doc: "A workspace the person's desk shows.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the record names it."),
            field("name", "Text", "Its name."),
            field(
                "current",
                "Bool",
                "Whether it is the workspace in front, which the desk shows.",
            ),
        ],
    },
    OriginKind {
        name: "session.arrangement_row",
        doc: "A row of the desk: a strip across a workspace's width, holding columns.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the record names it."),
            field("workspace", "Text", "The workspace it is a row of."),
            field("name", "Text", "Its name."),
            field(
                "shell",
                "Text",
                "The shell that made it; empty reads as this shell's.",
            ),
        ],
    },
    OriginKind {
        name: "session.arrangement_strip",
        doc: "A strip: a column of places, with a width, in a row of a workspace.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the record names it."),
            field("workspace", "Text", "The workspace it is in."),
            field("row", "Text", "The row it stands in."),
            field(
                "width",
                "Int",
                "Its width in per-mille of its row's usable width (333 narrow, 667 normal, \
                 1000 wide).",
            ),
            field("maximized", "Bool", "Whether it is maximized."),
            field(
                "active",
                "Text",
                "The place in front, by its id; empty for none.",
            ),
            field(
                "placed",
                "Bool",
                "Whether it has a place on its workspace's canvas.",
            ),
            field("canvas_x", "Int", "The canvas x of its top-leading corner."),
            field("canvas_y", "Int", "The canvas y of its top-leading corner."),
            field("canvas_width", "Int", "How wide it stands on the canvas."),
            field("canvas_height", "Int", "How tall it stands on the canvas."),
            field(
                "shell",
                "Text",
                "The shell that made it; empty reads as this shell's.",
            ),
        ],
    },
    OriginKind {
        name: "session.arrangement_place",
        doc: "A place: one window, showing a resource through a view.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the record names it."),
            field("strip", "Text", "The strip it is in."),
            field(
                "shows",
                "Text",
                "The app showing the resource, by its canonical id.",
            ),
            field(
                "shown_as",
                "Text",
                "The view it shows the resource through; empty for the app's default.",
            ),
            field("record", "Text", "The record it shows, when it shows one."),
            field("title", "Text", "The title it is shown under."),
            field("pinned", "Bool", "Whether the person pinned it."),
            field(
                "shell",
                "Text",
                "The shell that made it; empty reads as this shell's.",
            ),
        ],
    },
    OriginKind {
        name: "session.workspace",
        doc: "A workspace on this device's desk.",
        derived: true,
        fields: &[
            field(
                "key",
                "Text",
                "The workspace's id, as switching to it names it.",
            ),
            field("name", "Text", "Its name."),
            field("current", "Bool", "Whether it is the one in front."),
        ],
    },
    OriginKind {
        name: "session.person",
        doc: "Someone else here in the workspace in front: a person or an agent.",
        derived: true,
        fields: &[
            field("key", "Text", "Who they are, by their live presence's id."),
            field("name", "Text", "Their name here."),
        ],
    },
    OriginKind {
        name: "session.tray_item",
        doc: "A running application's tray item: what the bar shows, and what a press on it \
              reaches.",
        derived: true,
        fields: &[
            field("key", "Text", "The item's key, which a press on it names."),
            field("title", "Text", "The app's own words for it."),
            field(
                "menu",
                "Bool",
                "Whether choosing it opens its menu rather than its window.",
            ),
        ],
    },
    OriginKind {
        name: "session.task",
        doc: "A task running for the person: an agent's or a script's.",
        derived: true,
        fields: &[
            field("key", "Text", "The task's id."),
            field("title", "Text", "What it is doing, in a few words."),
            field(
                "state",
                "Text",
                "`running`, `waiting` (for the person) or `done`.",
            ),
        ],
    },
    OriginKind {
        name: "imap.account",
        doc: "What the IMAP server of one of the workspace's mail accounts holds. The account itself \
              (its address, servers and signature) is the settings', and its credentials stay in \
              custody: `key` names it.",
        derived: false,
        fields: &[
            field("key", "Text", "The settings account it mirrors, by its id."),
            field("address", "Text", "Its address."),
            field("name", "Text", "The name it sends as."),
            field(
                "trouble",
                "Text",
                "Why the server cannot be reached now; empty while it can.",
            ),
            field(
                "online",
                "Bool",
                "Whether the server was reached on the last try.",
            ),
            field(
                "mailboxes",
                "Collection<imap.mailbox>",
                "Its mailboxes, as the server names them.",
            ),
            field(
                "threads",
                "Collection<imap.thread>",
                "Its conversations, each with its messages.",
            ),
        ],
    },
    OriginKind {
        name: "imap.mailbox",
        doc: "A mailbox of an account.",
        derived: false,
        fields: &[
            field("key", "Text", "What messages name it by in `mailbox`."),
            field("name", "Text", "Its name."),
            field(
                "role",
                "Text",
                "`inbox`, `archive`, `sent`, `drafts`, `trash`, `junk` or empty.",
            ),
            field("unread", "Int", "How many unread messages it holds."),
            field("total", "Int", "How many messages it holds."),
        ],
    },
    OriginKind {
        name: "imap.thread",
        doc: "A conversation: messages that answer each other.",
        derived: false,
        fields: &[
            field("key", "Text", "The conversation's id: its first message's."),
            field("account", "Text", "The key of the account it is in."),
            field("subject", "Text", "The subject it started with."),
            field("latest", "Time", "When its newest message arrived."),
            field("snippet", "Text", "The start of its newest message."),
            field("senders", "Text", "Who wrote in it, newest first."),
            field("count", "Int", "How many messages it holds."),
            people(
                "unread",
                "Bool",
                "Whether a message in it is unread; marking it reads them all.",
            ),
            people("flagged", "Bool", "Whether a message in it is flagged."),
            people(
                "boxes",
                "Set<Text>",
                "The mailboxes its messages are in, by key.",
            ),
            field(
                "messages",
                "Collection<imap.message>",
                "Its messages, oldest first.",
            ),
        ],
    },
    OriginKind {
        name: "imap.message",
        doc: "A message.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The message's id: its Message-ID, or one derived from its content.",
            ),
            field("account", "Text", "The key of the account it is in."),
            field("subject", "Text", "Its subject."),
            field("from", "Text", "The sender's address."),
            field("from_name", "Text", "The sender's name, or their address."),
            field("to", "Text", "Its recipients, as the header lists them."),
            field("cc", "Text", "Its copied recipients."),
            field("received", "Time", "When it arrived."),
            field("snippet", "Text", "The start of what it says."),
            field("body", "Text", "What it says, as text."),
            field("message_id", "Text", "Its Message-ID header."),
            field(
                "references",
                "Text",
                "The Message-IDs it answers, oldest first, space separated.",
            ),
            field(
                "attachments",
                "Text",
                "The names of its attachments, one per line.",
            ),
            people("unread", "Bool", "Whether it is unread."),
            people("flagged", "Bool", "Whether it is flagged."),
            people("mailbox", "Text", "The key of the mailbox it is in."),
        ],
    },
    OriginKind {
        name: "youtube.channel",
        doc: "A channel on YouTube, as the service says it is.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The channel's id: what everything of the channel names it by.",
            ),
            field("name", "Text", "Its name, as the service writes it."),
            field("handle", "Text", "The handle it is known by (`@mkbhd`)."),
            field("subscribers", "Int", "How many people follow it."),
            field(
                "avatar",
                "Text",
                "The address of its picture, empty for none.",
            ),
            field(
                "banner",
                "Text",
                "The address of its banner, empty for none.",
            ),
            field("description", "Text", "What it says about itself."),
            field("verified", "Bool", "Whether the service marks it verified."),
        ],
    },
    OriginKind {
        name: "youtube.video",
        doc: "A video on YouTube, as the service says it is.",
        derived: false,
        fields: &[
            field("key", "Text", "The video's id."),
            field("title", "Text", "Its title."),
            field(
                "channel_key",
                "Text",
                "The id of the channel that published it.",
            ),
            field("channel_name", "Text", "The channel's name."),
            field(
                "channel_avatar",
                "Text",
                "The address of the channel's picture, as the service names it beside the video.",
            ),
            field("views", "Int", "How many times it was watched."),
            field("published", "Time", "When it was published."),
            field("duration", "Int", "How long it runs, in seconds."),
            field("description", "Text", "What its page says about it."),
            field("likes", "Int", "How many people liked it."),
            field("thumbnail", "Text", "The address of its still."),
            field(
                "stream",
                "Text",
                "The address its bytes play from, empty until the service is asked.",
            ),
            field("live", "Bool", "Whether it is a live stream."),
        ],
    },
    OriginKind {
        name: "youtube.playlist",
        doc: "A playlist on YouTube, as the service says it is.",
        derived: false,
        fields: &[
            field("key", "Text", "The playlist's id."),
            field("title", "Text", "Its title."),
            field("channel_key", "Text", "The id of the channel that made it."),
            field("channel_name", "Text", "The channel's name."),
            field("video_count", "Int", "How many videos it lists."),
            field("thumbnail", "Text", "The address of its cover."),
        ],
    },
];

/// The kind named `name`.
#[must_use]
pub fn kind(name: &str) -> Option<&'static OriginKind> {
    KINDS.iter().find(|k| k.name == name)
}

/// The names of every kind, for messages.
#[must_use]
pub fn kind_names() -> String {
    KINDS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
}

/// Whether `capability` is one an outside effect can be handed to.
#[must_use]
pub fn is_capability(capability: &str) -> bool {
    CAPABILITIES.iter().any(|(name, _)| *name == capability)
}

impl OriginKind {
    /// The field of the kind named `name`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&'static OriginField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The origin the kind belongs to: `imap` for `imap.message`.
    #[must_use]
    pub fn origin(&self) -> &'static str {
        self.name.split('.').next().unwrap_or(self.name)
    }
}

/// Whether the kind named `name` is one its origin derives, and so never saved to a
/// collaborative record ([`OriginKind::derived`]).
#[must_use]
pub fn is_derived(name: &str) -> bool {
    kind(name).is_some_and(|kind| kind.derived)
}
