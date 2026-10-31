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
/// `post`, the development log, and each origin's own.
pub const CAPABILITIES: &[(&str, &str)] = &[
    ("post", "Posts a payload; the development sink logs it."),
    (
        "smtp.send",
        "Sends a message from a mail account of the workspace's settings: (account, to, cc, \
         subject, body, the Message-ID it answers, the References it continues).",
    ),
    (
        "session.switch_workspace",
        "Brings a workspace of this device's desk to the front: the `key` of its \
         `session.workspace`.",
    ),
    (
        "session.act",
        "Asks the session for one of the desktop's own actions that take nothing (the \
         `desktop:chrome` resource's: `toggle-tilo-menu`, `toggle-quick-settings`, \
         `show-strips`, `toggle-overview`, `new-workspace`, `join-workspace`, \
         `workspace-settings`), by its id.",
    ),
    (
        "session.run",
        "Asks the session for one of the desktop's own actions that take arguments (the \
         `desktop:chrome` resource's), by its id, as `(action, arguments)`: each argument the \
         action's name, as the action's own field type writes it in JSON.",
    ),
    (
        "session.notify",
        "Asks the session for one of the notification centre's actions (the notifications' \
         resource's: `invoke`, `dismiss`, `clear`, `undo-clear`), by its id, as `(action, \
         arguments)`.",
    ),
];

/// Every kind of every origin the platform has.
pub const KINDS: &[OriginKind] = &[
    OriginKind {
        name: "session.desk",
        doc: "This device's desk as the session holds it, for the person at it: the time in their \
              style, who they are, their notifications, the workspaces on this device and the \
              tasks running for them. A shell's bar reads it; the session writes it.",
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
                "unread",
                "Int",
                "How many of the person's notifications are unread.",
            ),
            field(
                "quiet",
                "Bool",
                "Whether new notifications are held back (Do Not Disturb, presenting).",
            ),
            field(
                "status_panel",
                "Bool",
                "Whether the device has quick settings; without them its status is its \
                 notifications.",
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
                "title",
                "Text",
                "What the bar names the desk: the workspace in front, and the strip after it.",
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
                "The notifications in the centre, newest first.",
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
        ],
    },
    OriginKind {
        name: "session.window",
        doc: "A window of the desk: a tab of a strip in the workspace in front.",
        fields: &[
            field("key", "Text", "The tab's id, as moving or closing it names it."),
            field("title", "Text", "Its title."),
            field("origin_app", "Text", "The app it shows, by its name here."),
            field("icon", "Text", "The app's glyph, as the icon set names it."),
            field("strip", "Text", "The strip it is in, by its name."),
            field(
                "workspace",
                "Text",
                "The workspace it is in, by its name.",
            ),
            field(
                "recency",
                "Int",
                "How recently it was in front: lower is more recent.",
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
        fields: &[
            field("key", "Text", "The agent's id."),
            field("name", "Text", "Its name here."),
            field("state", "Text", "`running`, `waiting` (for the person) or `done`."),
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
        ],
    },
    OriginKind {
        name: "session.room",
        doc: "A workspace the person is in, as the workspace overview names it.",
        fields: &[
            field("key", "Text", "The workspace's id, as switching to it names it."),
            field("name", "Text", "Its name."),
            field("current", "Bool", "Whether it is the one in front."),
            field(
                "members",
                "Text",
                "Who is in it, by their names, in the person's language.",
            ),
            field("present", "Int", "How many people are in it now."),
            field("activity", "Text", "What happened in it last, in a few words."),
        ],
    },
    OriginKind {
        name: "session.notice",
        doc: "A notification in the centre, as its sender or its task named it.",
        fields: &[
            field("key", "Text", "The notification's id, as acting on it names it."),
            field("title", "Text", "What happened, in a short sentence."),
            field("body", "Text", "What it says, under the title."),
            field("origin_app", "Text", "The app that sent it, or the agent's task."),
            field(
                "icon",
                "Text",
                "Its glyph, as the icon set names it.",
            ),
            field("kind", "Text", "Its kind: `info`, `success`, `warning` or `error`."),
            field("read", "Bool", "Whether the person has seen it."),
            field("group", "Text", "What it is grouped under: its app or its task."),
            field(
                "wants_reply",
                "Bool",
                "Whether it asks the person something directly: it is not shown while quiet.",
            ),
        ],
    },
    OriginKind {
        name: "session.notice_action",
        doc: "One action a notification offers, as its sender named it: a message's Reply, an \
              import's Open.",
        fields: &[
            field("notice", "Text", "The notice it is on, by its id."),
            field("key", "Text", "The key the sender answers by."),
            field("label", "Text", "What it says, in the sender's words."),
        ],
    },
    OriginKind {
        name: "session.workspace",
        doc: "A workspace on this device's desk.",
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
        fields: &[
            field("key", "Text", "Who they are, by their live presence's id."),
            field("name", "Text", "Their name here."),
        ],
    },
    OriginKind {
        name: "session.task",
        doc: "A task running for the person: an agent's or a script's.",
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
