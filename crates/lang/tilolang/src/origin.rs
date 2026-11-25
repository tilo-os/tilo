//! Outside origins: the services whose resources a type can hold (convergence.md, "Origins" and
//! "Outside services").
//!
//! A type names the kind of an outside service's resource it holds with `type Message (origin:
//! imap.message) { ... }`. The system keeps such resources in step with the service: it gets and
//! searches them through the service's capabilities and writes what the service says into the fields
//! the kind defines.
//!
//! A kind's fields are written by the service alone, except those people change (a message's
//! `unread`, `flagged` and `mailbox`): a change of one applies at once and becomes final when the
//! service accepts it. Code writing any other field of the kind is refused (E113), except in a
//! scenario, whose seed data stands in for the service.
//!
//! Some kinds are the origin's to derive: the time, the shell, who is here, the workspaces on the
//! device. The origin writes them again from what it holds at every start, so nothing of an app's
//! makes them.

#[derive(Clone, Copy, Debug)]
pub struct OriginField {
    pub name: &'static str,
    pub ty: &'static str,
    /// Whether people change it (the service is told), or only the service writes it.
    pub people: bool,
    pub doc: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct OriginKind {
    /// The dotted name a type declares: `imap.message`.
    pub name: &'static str,
    pub doc: &'static str,
    pub fields: &'static [OriginField],
    /// The origin derives these resources from what it holds: it makes them again from nothing each
    /// time it starts, so an app's action never makes one.
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

/// The capabilities an action may hand an outside effect to with `outside(capability, ...)`: `post`, the
/// development log, and each origin's own.
pub const CAPABILITIES: &[(&str, &str)] = &[
    ("post", "Posts a payload; the development sink logs it."),
    (
        "smtp.send",
        "Sends a message from a mail account of the workspace's settings: (account, the name and \
         address it goes from, to, cc, bcc, subject, body, the Message-ID it answers, the \
         References it continues, its calendar invitation). What it says goes out as the words \
         alone and as the formatted alternative of them.",
    ),
    (
        "imap.check",
        "Asks a mail account for its mail now: (the account's key). What its server holds comes \
         in as it does when the server is heard from, and the account says how it stands.",
    ),
    (
        "imap.older",
        "Asks a mail account's server for the messages older than the oldest one the workspace \
         holds: (the account's key, the mailbox's key). They come in as the server's changes do.",
    ),
    (
        "imap.search",
        "Searches a mail account's server for the text: (the account's key, the text). What it \
         finds comes in as the account's mail, so what the workspace does not hold is read here \
         from then on.",
    ),
    (
        "imap.save_draft",
        "Keeps a draft in a mail account's Drafts folder on its server: (the account's key, to, \
         cc, subject, body, the Message-ID it answers, the References it continues, the calendar \
         invitation or nothing), so another mail client of the account sees it.",
    ),
    (
        "imap.purge",
        "Deletes messages of a mail account from its server for good, its trash emptied of them: \
         (the account's key, the keys of its messages). A message the account no longer holds is \
         passed over.",
    ),
    (
        "caldav.check",
        "Asks a calendar account for its calendars now: (the account's key). What its server \
         holds comes in as it does when the server is heard from.",
    ),
    (
        "caldav.answer",
        "Answers an invitation: (the account's key, the event's key, the address invited, \
         `accepted`, `declined` or `tentative`). The person's own answer goes to its server, and \
         their status on the attendee says it.",
    ),
    (
        "caldav.import",
        "Puts an invitation's iCalendar text into a calendar, once: (the account's key, the \
         calendar's key, the text).",
    ),
    (
        "youtube.search",
        "Searches YouTube's catalogue for the query, as typed: (the query, the kinds it shows, \
         how long its videos run, how recent they are). The channels, playlists and videos it \
         answers are written where the search's field says.",
    ),
    (
        "youtube.channel",
        "Asks YouTube for a channel's videos: (the channel's key).",
    ),
    (
        "youtube.playlist",
        "Asks YouTube for what a playlist holds: (the playlist's key). Its videos are written \
         where the playlist's page reads them, in the order its maker put them.",
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
        "Asks YouTube where a video's bytes play from, at a quality: (the video's key, the \
         tallest picture the person wants or `auto`). The address is written into the video's \
         `stream`, which the player opens.",
    ),
    (
        "youtube.comments",
        "Asks YouTube for a video's comments, most liked first: (the video's key). They are \
         written where the client reads them, keyed by the comment's own id.",
    ),
    (
        "youtube.segments",
        "Asks SponsorBlock for the segments it marks in a video: (the video's key). They are \
         written where the client reads them, keyed by where each stands.",
    ),
    (
        "youtube.branding",
        "Asks DeArrow for the community's title and picture of a video: (the video's key). They \
         are written into the video.",
    ),
    (
        "youtube.votes",
        "Asks Return YouTube Dislike how many people disliked a video: (the video's key). The \
         count is written into the video.",
    ),
    (
        "youtube.captions",
        "Asks YouTube for a caption track's words: (the video's key, the track's language). The \
         words are written into the video's `captions`, which the player draws.",
    ),
    (
        "music.search",
        "Searches YouTube Music's catalogue for the query, as typed: (the query). Its songs, \
         albums, artists and playlists are written where the search's field says.",
    ),
    (
        "music.home",
        "Asks YouTube Music for its home page: (nothing). Its sections are written as \
         suggestions.",
    ),
    (
        "music.artist",
        "Asks YouTube Music for an artist's page: (the artist's key).",
    ),
    (
        "music.album",
        "Asks YouTube Music for an album's songs: (the album's key).",
    ),
    (
        "music.playlist",
        "Asks YouTube Music for a catalogue playlist's songs: (the playlist's key).",
    ),
    (
        "music.stream",
        "Asks YouTube Music where a song's bytes play from: (the song's key). The address is \
         written into the song's `stream`, which the player opens.",
    ),
    (
        "music.lyrics",
        "Asks for a song's words: (its title and its artist). What was found is written into \
         the song's `lyrics`, and `lyrics_state` says whether they are timed.",
    ),
    (
        "music.pin",
        "Keeps a song on this device for playing with no network: (the address its bytes play \
         from, and its name). The copy's address is written into the song's `offline`.",
    ),
    (
        "music.spotify",
        "Asks Spotify's own Web API: (what to do, and what it is about). It begins and finishes \
         the person's sign-in, reads their library and the devices Spotify plays on, and moves \
         playback between them; the catalogue pages it finds are written as any catalogue's.",
    ),
];

pub const KINDS: &[OriginKind] = &[
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
                "The resource, by its id in its app, as the layout's `place` takes it.",
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
        name: "session.layout",
        doc: "The person's own layout: what is open where on their shell (shell.md §2), as \
              the session keeps it in their own record. A shell's views read it; every change \
              is one of the arranging actions on `shell:layout`, so nothing writes it \
              directly.",
        derived: true,
        fields: &[
            field(
                "person",
                "Text",
                "The person at this device, by their name.",
            ),
            field(
                "people",
                "Collection<session.layout_person>",
                "The others here in the workspace in front.",
            ),
            field(
                "workspaces",
                "Collection<session.layout_workspace>",
                "The workspaces the shell shows.",
            ),
            field(
                "rows",
                "Collection<session.layout_row>",
                "The shell's rows, each a strip across a workspace.",
            ),
            field(
                "strips",
                "Collection<session.layout_strip>",
                "The strips, each a column of places.",
            ),
            field(
                "places",
                "Collection<session.layout_place>",
                "Every place, showing a resource through a view; each names the strip it is in.",
            ),
            field(
                "queries",
                "Collection<session.layout_query>",
                "The queries the canvas stands while an app answers them.",
            ),
            field(
                "refs",
                "Collection<session.layout_ref>",
                "The resources those queries found, each naming the query it is of.",
            ),
            field(
                "recents",
                "Collection<session.layout_recent>",
                "What the person opened lately in the workspace in front: each resource once, \
                 theirs first, newest first, without what they cleared or cannot read.",
            ),
        ],
    },
    OriginKind {
        name: "session.layout_workspace",
        doc: "A workspace the person's shell shows.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the layout names it."),
            field("name", "Text", "Its name."),
            field(
                "current",
                "Bool",
                "Whether it is the workspace in front, which the shell shows.",
            ),
        ],
    },
    OriginKind {
        name: "session.layout_person",
        doc: "Someone else here in the workspace in front: a person or an agent.",
        derived: true,
        fields: &[
            field("key", "Text", "Who they are, by their live presence's id."),
            field("name", "Text", "Their name here."),
        ],
    },
    OriginKind {
        name: "session.layout_row",
        doc: "A row of the shell: a strip across a workspace's width, holding columns.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the layout names it."),
            field("workspace", "Text", "The workspace it is a row of."),
            field("name", "Text", "Its name."),
            field(
                "current",
                "Bool",
                "Whether it is the row in front in its workspace.",
            ),
            field(
                "shell",
                "Text",
                "The shell that made it; empty reads as this shell's.",
            ),
        ],
    },
    OriginKind {
        name: "session.layout_strip",
        doc: "A strip: a column of places, with a width, in a row of a workspace.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the layout names it."),
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
                "canvas_hugs",
                "Bool",
                "Whether its height is the most it takes: what it shows hugs what it holds.",
            ),
            field(
                "shell",
                "Text",
                "The shell that made it; empty reads as this shell's.",
            ),
        ],
    },
    OriginKind {
        name: "session.layout_place",
        doc: "A place: one window, showing a resource through a view.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the layout names it."),
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
            field(
                "resource",
                "Text",
                "The resource it shows, when it shows one.",
            ),
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
        name: "session.layout_query",
        doc: "A query typed on the canvas: what the person looked for, in which app, and what \
              came of it (shell.md §8, \"Finding what to place\"). Placed like any resource, it \
              shows the search as it runs, the rows it found and what a press on a row places, \
              and why it found nothing or failed. Its `matches` name resources the same way a \
              place's record does (`session.layout_ref`).",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as the layout names it."),
            field("text", "Text", "What the person typed."),
            field("in_app", "Text", "The app asked, by its canonical id."),
            field(
                "kind",
                "Text",
                "The kind of resource the query names, as its app publishes it.",
            ),
            field(
                "state",
                "Text",
                "How the search stands: `searching`, `one`, `several`, `none` or `failed`.",
            ),
            field(
                "reason",
                "Text",
                "Why it failed, in the app's own words; empty while it does better.",
            ),
        ],
    },
    OriginKind {
        name: "session.layout_recent",
        doc: "A resource the person opened lately: read off the workspace's history of what \
              its windows showed, by whom and when. Clearing recents (`shell:layout` \
              `clear-recents`) hides what came before for the person alone; the history keeps it.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id: the resource's address."),
            field(
                "resource",
                "Text",
                "The resource, by its address, as the layout's `open` takes it.",
            ),
            field("title", "Text", "What it is called."),
            field(
                "origin_app",
                "Text",
                "The app that keeps it, by its canonical id.",
            ),
            field(
                "app_title",
                "Text",
                "That app's name, in the person's language.",
            ),
            field(
                "kind",
                "Text",
                "Its type, as its app publishes it (`tube.video`).",
            ),
            field("by", "Text", "Who opened it last, by name."),
            field("at", "Time", "When it was opened."),
            field(
                "mine",
                "Bool",
                "Whether the person looking opened it last themselves.",
            ),
        ],
    },
    OriginKind {
        name: "session.layout_ref",
        doc: "A resource a query found: the same reference a place holds, so a row of matches \
              names what a place shows by the same three words.",
        derived: true,
        fields: &[
            field("key", "Text", "Its id, as `<query>/<number>`."),
            field("query", "Text", "The query it is of, by its key."),
            field(
                "address",
                "Text",
                "The resource, by its id in its app, as the layout's `place` takes it.",
            ),
            field("title", "Text", "What it is called."),
            field(
                "kind",
                "Text",
                "Its kind, as its app publishes it (`tube.channel`).",
            ),
        ],
    },
    OriginKind {
        name: "caldav.calendar",
        doc: "A calendar: one the workspace's own, or one an account's server holds, which the \
              system keeps in step with it. Its entries are `caldav.event` records that name it.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The calendar's id: the address its server keeps it at, or one the app gives a \
                 calendar of the workspace's own.",
            ),
            field(
                "account",
                "Text",
                "The key of the account whose server holds it; empty for a calendar of the \
                 workspace's own, which no server is ever asked about.",
            ),
            people("name", "Text", "Its name, as the person reads it."),
            people(
                "color",
                "Text",
                "The colour it is shown in, as its server's `calendar-color` says it; empty \
                 for none.",
            ),
            field(
                "writable",
                "Bool",
                "Whether its server lets its person write it, and the account is not read-only.",
            ),
            field(
                "sync_token",
                "Text",
                "Where its last pull ended, as its server names it: the next pull asks from                  there. Empty for one not pulled yet.",
            ),
        ],
    },
    OriginKind {
        name: "caldav.attendee",
        doc: "Somebody an entry names: who organises it, or who it invites.",
        derived: false,
        fields: &[
            people("name", "Text", "Their name, as the entry says it."),
            people("email", "Text", "Their address."),
            people(
                "status",
                "Text",
                "Whether they come: `needs-action`, `accepted`, `declined` or `tentative`.",
            ),
            people(
                "role",
                "Text",
                "What they are to the entry: `required`, `optional` or `chair`.",
            ),
        ],
    },
    OriginKind {
        name: "caldav.account",
        doc: "One of the workspace's calendar accounts, and how its server answers: whether it \
              is reached, what it said when it was not, and when its calendars last came.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The account's id, as the workspace settings name it.",
            ),
            field("address", "Text", "The name it signs in with."),
            people("name", "Text", "What it is called."),
            field("server", "Text", "The server address the person typed."),
            field(
                "read_only",
                "Bool",
                "Whether nothing is ever written to its server.",
            ),
            field(
                "trouble",
                "Text",
                "What the server said when it could not be reached, in its words; empty while \
                 it stands well.",
            ),
            field("online", "Bool", "Whether the server answers."),
            field(
                "checking",
                "Bool",
                "Whether the server is being asked now, so the window says what is under way.",
            ),
            field(
                "fetched",
                "Time",
                "When the account's calendars were last taken in from its server.",
            ),
        ],
    },
    OriginKind {
        name: "caldav.event",
        doc: "One entry of a calendar, as its server holds it. The fields people change are \
              written to the server; the rest are the server's.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The entry's id: its `UID`, the name its server and every client know it by.",
            ),
            field(
                "href",
                "Text",
                "Where its server keeps it; empty for an entry no server holds.",
            ),
            field(
                "etag",
                "Text",
                "The version its server last gave it: a write goes over that one.",
            ),
            people("title", "Text", "What it is called."),
            people(
                "start",
                "Text",
                "When it starts, as iCalendar writes the wall time: `2026-11-24T09:00:00`.",
            ),
            people(
                "end",
                "Text",
                "When it ends, as iCalendar writes the wall time.",
            ),
            people("all_day", "Bool", "Whether it lasts whole days."),
            people(
                "timezone",
                "Text",
                "The zone its wall time is read in: an IANA name, `UTC` for a time written in \
                 it, or `floating` for a time written nowhere in particular.",
            ),
            people(
                "day",
                "Date",
                "The day it starts on, its wall time's own: what a calendar sorts and counts it \
                 by.",
            ),
            people(
                "end_day",
                "Date",
                "The day it ends on, its wall time's own; the last day it is on, where it lasts \
                 whole days.",
            ),
            people("location", "Text", "Where it is."),
            people("notes", "Text", "What it says: its `DESCRIPTION`, as text."),
            people(
                "attendees",
                "Collection<caldav.attendee>",
                "Who organises it and who it invites.",
            ),
            people(
                "categories",
                "Set<Text>",
                "What it is filed under, as its server's `CATEGORIES`.",
            ),
            people(
                "recurrence",
                "Text",
                "How it repeats, as its `RRULE`'s frequency and days \
                 (`FREQ=WEEKLY;BYDAY=MO,TU`); empty for one occurrence.",
            ),
            people(
                "recurrence_every",
                "Int",
                "One for a repeat every time it comes, two for every other, and so on.",
            ),
            people(
                "recurrence_until",
                "Option<Date>",
                "The last day it repeats on; none for no end.",
            ),
            people(
                "recurrence_count",
                "Option<Int>",
                "How many times it repeats; none for no count.",
            ),
            people(
                "exdates",
                "Set<Text>",
                "The days it is left out on, `YYYY-MM-DD` each.",
            ),
            people(
                "color",
                "Text",
                "The colour it is shown in, as its `COLOR`; empty for none.",
            ),
            people(
                "organizer",
                "Text",
                "The address of whoever organises it; empty for nobody.",
            ),
            field(
                "project",
                "Text",
                "What the app files it under; the server holds nothing of it.",
            ),
            people(
                "cancelled",
                "Bool",
                "Whether this occurrence of a repeat is called off.",
            ),
            people(
                "remind",
                "Time",
                "When its reminder rings, an instant; 1970 for none. Its `VALARM`.",
            ),
            people(
                "calendar",
                "Option<caldav.calendar>",
                "The calendar it is in. A change to it moves the entry: written to the calendar \
                 it names, and taken out of the one it was in.",
            ),
            people(
                "recurrence_of",
                "Option<caldav.event>",
                "The repeating entry this is one occurrence of, changed or cancelled; none for \
                 an entry of its own.",
            ),
            people(
                "original_start",
                "Text",
                "The wall time this occurrence would have started at, where it is one of a \
                 repeat's.",
            ),
        ],
    },
    OriginKind {
        name: "imap.account",
        doc: "What the IMAP server of one of the workspace's mail accounts holds. The account itself \
              (its address, servers and signature) is the settings', and its credentials stay in \
              ownership: `key` names it.",
        derived: false,
        fields: &[
            field("key", "Text", "The settings account it mirrors, by its id."),
            field("address", "Text", "Its address."),
            field("name", "Text", "The name it sends as."),
            field(
                "trouble",
                "Text",
                "Why the account is not in step now: its server cannot be reached, or what the \
                 server said could not be saved; empty while it is in step.",
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
            field(
                "more",
                "Bool",
                "Whether its server holds messages older than the ones the workspace holds.",
            ),
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
            field(
                "html",
                "Text",
                "What it says as its sender formatted it: its HTML part, each picture it shows in \
                 place at the address of its content. Empty for a message of plain text.",
            ),
            field("message_id", "Text", "Its Message-ID header."),
            field(
                "references",
                "Text",
                "The Message-IDs it answers, oldest first, space separated.",
            ),
            field(
                "attachments",
                "List<File>",
                "The files it came with, each kept in the workspace's content; one whose bytes \
                 did not come with it is named with no content.",
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
            field(
                "dislikes",
                "Int",
                "How many people disliked it, as Return YouTube Dislike estimates it.",
            ),
            field("thumbnail", "Text", "The address of its still."),
            field(
                "dearrow_title",
                "Text",
                "The community's title for it, empty when it has none.",
            ),
            field(
                "dearrow_thumbnail",
                "Text",
                "The address of the picture the community frames it with, empty for none.",
            ),
            field(
                "stream",
                "Text",
                "The address its bytes play from, empty until the service is asked.",
            ),
            field("live", "Bool", "Whether it is a live stream."),
        ],
    },
    OriginKind {
        name: "youtube.comment",
        doc: "A comment someone left on a video, as the service says it is.",
        derived: false,
        fields: &[
            field("key", "Text", "The comment's id."),
            field("video_key", "Text", "The id of the video it was left on."),
            field("author", "Text", "Who wrote it."),
            field(
                "avatar",
                "Text",
                "The address of the writer's picture, empty for none.",
            ),
            field("text", "Text", "What it says."),
            field("likes", "Int", "How many people liked it."),
            field("published", "Time", "When it was written."),
            field("pinned", "Bool", "Whether the video's maker pinned it."),
            field(
                "hearted",
                "Bool",
                "Whether the video's maker marked it with a heart.",
            ),
            field("replies", "Int", "How many replies it has on YouTube."),
        ],
    },
    OriginKind {
        name: "youtube.segment",
        doc: "One SponsorBlock segment of a video, as the community placed it.",
        derived: false,
        fields: &[
            field("key", "Text", "Where the segment stands in the video."),
            field("video_key", "Text", "The id of the video it belongs to."),
            field("category", "Text", "Its kind, as SponsorBlock names it."),
            field("start", "Int", "Where it starts, in whole seconds."),
            field("end", "Int", "Where it ends, in whole seconds."),
            field("votes", "Int", "The community votes that place it."),
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
    OriginKind {
        name: "music.track",
        doc: "A song of a music catalogue, as the service says it is.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The song's id: what its stream and its queue entry name it by.",
            ),
            field("title", "Text", "Its title."),
            field("artist", "Text", "Who performs it."),
            field("artist_key", "Text", "The id of the artist."),
            field("album", "Text", "The album it is on."),
            field("album_key", "Text", "The id of the album."),
            field("seconds", "Int", "How long it runs."),
            field(
                "plays",
                "Int",
                "How many times the service says it was played.",
            ),
            field("cover", "Text", "The address of its cover, empty for none."),
            field(
                "found_for",
                "Text",
                "The search that found it, empty for none.",
            ),
            field(
                "stream",
                "Text",
                "Where its bytes play from, once the service says.",
            ),
            people(
                "offline",
                "Text",
                "The address of the copy kept on this device, empty for none.",
            ),
            field("lyrics", "Text", "Its words, as LRC."),
            field(
                "lyrics_state",
                "Text",
                "`synced`, `plain`, or empty when none were found.",
            ),
        ],
    },
    OriginKind {
        name: "music.section",
        doc: "One titled section of the catalogue's home page.",
        derived: false,
        fields: &[
            field("key", "Text", "The section's id."),
            field("title", "Text", "The words it is shown by."),
            field("order", "Int", "Where it stands on the page."),
        ],
    },
    OriginKind {
        name: "music.album",
        doc: "An album of a music catalogue.",
        derived: false,
        fields: &[
            field("key", "Text", "The album's id."),
            field("title", "Text", "Its title."),
            field("artist", "Text", "Who made it."),
            field("artist_key", "Text", "The id of the artist."),
            field("year", "Int", "The year it came out."),
            field(
                "release_kind",
                "Text",
                "`album`, `single`, `ep` or `release`.",
            ),
            field("cover", "Text", "The address of its cover, empty for none."),
            field(
                "found_for",
                "Text",
                "The search that found it, empty for none.",
            ),
        ],
    },
    OriginKind {
        name: "music.artist",
        doc: "An artist of a music catalogue.",
        derived: false,
        fields: &[
            field("key", "Text", "The artist's id."),
            field("name", "Text", "Their name."),
            field("subscribers", "Int", "How many people listen each month."),
            field(
                "avatar",
                "Text",
                "The address of their picture, empty for none.",
            ),
            field(
                "found_for",
                "Text",
                "The search that found them, empty for none.",
            ),
            field(
                "genres",
                "List<Text>",
                "The kinds of music the catalogue calls theirs, empty where it names none.",
            ),
        ],
    },
    OriginKind {
        name: "music.playlist",
        doc: "A playlist the catalogue publishes.",
        derived: false,
        fields: &[
            field("key", "Text", "The playlist's id."),
            field("title", "Text", "Its title."),
            field("subtitle", "Text", "Who made it, or what it is."),
            field("track_count", "Int", "How many songs it lists."),
            field("cover", "Text", "The address of its cover, empty for none."),
            field(
                "found_for",
                "Text",
                "The search that found it, empty for none.",
            ),
        ],
    },
    OriginKind {
        name: "music.device",
        doc: "A place Spotify plays on: a phone, a computer, a speaker or the person's browser.",
        derived: false,
        fields: &[
            field("key", "Text", "The device's id, as Spotify names it."),
            field("name", "Text", "What people read of it."),
            field(
                "kind",
                "Text",
                "`computer`, `smartphone`, `speaker`, `tv`, `cast` or another Spotify names.",
            ),
            field("active", "Bool", "Whether what plays plays here."),
            field(
                "volume",
                "Int",
                "How loud it is, `0..=100`, when Spotify says.",
            ),
        ],
    },
    OriginKind {
        name: "music.suggestion",
        doc: "One row of the catalogue's home page.",
        derived: false,
        fields: &[
            field(
                "key",
                "Text",
                "The row's id, its section and what it names.",
            ),
            field("section", "Text", "The section it stands in."),
            field("kind", "Text", "`track`, `album`, `artist` or `playlist`."),
            field("target", "Text", "The key of the thing it names."),
            field("title", "Text", "What the row says."),
            field("subtitle", "Text", "What stands under it."),
            field(
                "cover",
                "Text",
                "The address of its picture, empty for none.",
            ),
        ],
    },
    OriginKind {
        name: "music.spotify_queue",
        doc: "A song in what plays next on Spotify, in its own order.",
        derived: false,
        fields: &[
            field("key", "Text", "The song's Spotify address."),
            field("title", "Text", "What it is called."),
            field(
                "artist",
                "Text",
                "Who sings it, or the show it is an episode of.",
            ),
            field("album", "Text", "The album or show it is from."),
            field(
                "cover",
                "Text",
                "The address of its picture, empty for none.",
            ),
            field("seconds", "Int", "How long it runs."),
        ],
    },
    OriginKind {
        name: "discord.account",
        doc: "One of the workspace's connected Discord accounts: the server it bridges and what \
              the bridge knows of both sides. Chat holds it; the server's channels and direct \
              messages are Chat's conversations, and the account holds what joins the two sides.",
        derived: false,
        fields: &[
            field("key", "Text", "The account's id."),
            people("name", "Text", "What the account is called on Discord."),
            people(
                "guild_id",
                "Text",
                "The server bridged, by its Discord id; empty until one is connected.",
            ),
            people("server", "Text", "The server's name, as Discord shows it."),
            people(
                "only_channel",
                "Text",
                "The one channel bridged, by its Discord id; empty for the whole server.",
            ),
            people(
                "mirror_channels",
                "Bool",
                "Whether channels made here are made in Discord too.",
            ),
            people(
                "map_roles",
                "Bool",
                "Whether the workspace's roles are kept as the bridge's roles in Discord.",
            ),
            people(
                "token_item",
                "Text",
                "The secret item holding the bot token on this device; the token never enters the \
                 resource.",
            ),
            people(
                "bot_user",
                "Text",
                "The bot user's Discord id: its own messages and reactions are echoes.",
            ),
            people(
                "sign_in_page",
                "Text",
                "Discord's consent page for signing in through this device, without its state; \
                 empty when the device offers none.",
            ),
            people(
                "voice_problem",
                "Text",
                "Why the bridge cannot carry a voice channel's call now; empty while it can.",
            ),
            people(
                "emoji_problem",
                "Text",
                "Why the workspace's emoji do not reach the server now; empty while they do.",
            ),
            people("setup_open", "Bool", "Whether the setup dialog is open."),
            people(
                "setup_problem",
                "Text",
                "Why finding the bot's servers did not work, in words the person reads.",
            ),
        ],
    },
];

#[must_use]
pub fn kind(name: &str) -> Option<&'static OriginKind> {
    KINDS.iter().find(|k| k.name == name)
}

#[must_use]
pub fn kind_names() -> String {
    KINDS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
}

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

/// Whether the kind named `name` is one its origin derives, written again at every start.
#[must_use]
pub fn is_derived(name: &str) -> bool {
    kind(name).is_some_and(|kind| kind.derived)
}
