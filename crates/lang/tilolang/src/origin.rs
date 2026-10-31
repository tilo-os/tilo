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
        "session.arrange",
        "Arranges the person's own desk: one of the arranging actions the session publishes on \
         `shell:arrangement` (`open-into`, `move-place`, `resize-strip`, `close-place`, \
         `pin-place`), by its id, with its arguments as one line of JSON: `move-place \
         {\"place\":3,\"row\":7}`.",
    ),
];

/// Every kind of every origin the platform has.
pub const KINDS: &[OriginKind] = &[
    OriginKind {
        name: "session.desk",
        doc: "This device's desk as the session holds it, for the person at it: the time in their \
              style, who they are, their notifications, the workspaces on this device and the \
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
