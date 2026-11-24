app so.tilo.chat "Chat" (
    icon: .message_circle,
    category: .network,
    tagline: "Workspaces for messages, calls and shared work",
    keywords: ["messenger", "instant messaging", "conversation", "assistant", "personal assistant", "my agent", "main chat", "sub-agents", "supervision"],
    previously: so.tilo.lang.chat,
    splash_budget: 250,
)

use tilo.workspace as workspace
use so.tilo.settings.sections as settings
use so.tilo.device as device

/// The workspace's conversations in the sidebar's order: channels outside any category, the
/// categories each with its own, and the direct messages, in the list as people. Beside them,
/// how kinds of work are done here: the workspace's skills, which its people write and its
/// agents read before they start.
type Conversations {
    channels: Collection<Category | TextChannel | VoiceChannel | Direct>
    skills: Collection<Skill>
    /// What each person is writing in a conversation and has not sent. A draft is read by its
    /// author alone, so a change to it is never news to the conversation: nobody else sees that
    /// someone is half-way through a message.
    drafts: Collection<Draft>
    /// The reports filed about a person rather than one of their messages, for the workspace's
    /// moderators.
    reports: Collection<Report>
}

root conversations: Conversations

device root hearing: Hearing

/// A named group of channels in the sidebar.
type Category {
    name: Text
    /// The server's channels.
    channels: Collection<TextChannel | VoiceChannel>
    /// The outside service this conversation is carried to, as its people read it ("Discord");
    /// empty for one that stays here.
    service: Text = ""
    /// Its id there, when a bridge carries it: the channel, category or conversation there.
    outside_id: Text = ""
}

/// A place to talk live: joining it puts the person in its call, the system's, with whoever else
/// joined. Who is in it is the call's presence, never stored; who may speak or listen is the
/// workspace's.
type VoiceChannel {
    name: Text
    /// The outside service this conversation is carried to, as its people read it ("Discord");
    /// empty for one that stays here.
    service: Text = ""
    /// Its id there, when a bridge carries it: the channel, category or conversation there.
    outside_id: Text = ""
}

/// How this device plays each person it hears in a call: their level, and whether the person
/// silenced them, which keeps the level for when they hear them again. It is the device's own:
/// nobody else's call changes, and another device of the person keeps its own.
type Hearing {
    people: Collection<Heard>
}

/// How this device plays one person in a call.
type Heard {
    who: Actor
    /// In percent of as they arrive, up to twice as loud.
    percent: Int = 100
    /// Not played at all, whatever the level.
    silenced: Bool = false
}

/// Messages in one stream; any message can start a thread. A post is a change of its channel,
/// so it is unseen to everyone but its author: Chat's Unread place counts the conversations
/// holding one.
type TextChannel {
    name: Text
    topic: Text
    /// What has been said here so far, in a few paragraphs, as an agent last wrote it down: what
    /// a turn reads in place of the messages it summarizes. Empty until one is written.
    summary: Text = ""
    messages: Collection<Message>
    /// The threads its messages started, each the one a message's `thread` names.
    /// TODO(language): an owned `Option<Thread>` on the message; a reference needs a home.
    threads: Collection<Thread>
    /// The reports filed about its messages, for the workspace's moderators.
    reports: Collection<Report>
    /// Whether the channel is set aside: it keeps every message and its place in the sidebar is
    /// gone until it is restored. Search still finds it, so restoring it is a press there.
    archived: Bool = false
    /// The outside service this conversation is carried to, as its people read it ("Discord");
    /// empty for one that stays here.
    service: Text = ""
    /// Its id there, when a bridge carries it: the channel, category or conversation there.
    outside_id: Text = ""

    search {
        kind conversation
    }
}

/// Who wrote a message somewhere else, as that place shows them: a bridge's generic outside
/// author, never one service's shape.
type OutsideAuthor {
    /// The name they go by there.
    name: Text
    /// The address of their picture there, empty for none.
    avatar: Text = ""
    /// What the place they wrote from is called, as its people read it ("Discord").
    service: Text = ""
    /// The person here who linked that account to themselves, when one did: the message wears
    /// their face and colour beside the account's name, so the workspace sees who it is.
    member: Option<Actor> = .none
}

/// A window shared live into a conversation, as a message refers to it. The share is the shell's:
/// the session shares the window with the workspace (`device.shares`), every member's shell shows
/// it as the window it is, and this names it so the conversation shows it again where it was
/// posted. What the window was called and whose app it is are kept as it was shared, so a share
/// that ended still says what it was.
type LiveShare {
    /// The shared window, by the key the shell's windows and shares name it with.
    window: Text
    /// The workspace it is shared in, by its key.
    workspace: Text = ""
    /// What the window was called when it was shared.
    title: Text = ""
    /// The app whose window it is, as people know it, and its mark.
    app_name: Text = ""
    icon: Text = ""
    /// Whether the members viewing it may control it, as its sharer set the share.
    can_edit: Bool = false
    /// Whether its sharer stopped sharing it.
    ended: Bool = false
}

/// A picture a message from outside carries in its words: a custom emoji of that service.
type OutsideEmoji {
    name: Text
    /// The address of its picture.
    picture: Text
}

/// A message. Who wrote it, when, and its edits come from the log.
type Message {
    body: RichText
    /// The message this one answers inline, quoted above it.
    reply_to: Option<Message>
    /// The side conversation branched off this message.
    thread: Option<Thread>
    /// What a message says, in a message kept as parts rather than as rich text: its pieces, as
    /// it was written.
    parts: Collection<Part>
    /// Whether it is pinned to its channel, which search finds.
    pinned: Bool = false
    /// The agent's turn that wrote it: the id of the `#answer` invocation it answers in, which
    /// Stop cancels and whose steps, questions and cost are the turn's. Empty for a message a
    /// person wrote.
    turn: Text = ""
    /// What the agent's model reasoned while it wrote this answer, as its provider shows it:
    /// written beside the answer as it comes, and folded under it. Empty for a message a person
    /// wrote, or a model that shows no reasoning.
    reasoning: Text = ""
    /// Who reacted, by emoji.
    reactions: Map<Set<Actor>>
    /// A view the message sends, as its sender had it among their views: a reader adds it to
    /// their own views, and its author is whoever sent the message. Empty for a message that
    /// sends none.
    sent_view: Text
    /// A window its sender shares live: the shell's own share of it, which the conversation shows
    /// where the message stands. None for a message that shares no window.
    live_share: Option<LiveShare> = .none
    /// Who wrote it, when it was written somewhere else: as that place shows them, with the
    /// picture they wear there and the service's name. None for a message written here.
    outside: Option<OutsideAuthor> = .none
    /// The pictures its words carry from where it was written: that service's custom emoji,
    /// each drawn by the address it gives.
    outside_emoji: Collection<OutsideEmoji>
    /// Who wrote it where it was written, as that place named them, for a message carried over
    /// from the native Chat or relayed from another service: its own history names whoever
    /// carried or relayed it.
    by: Text = ""
    /// Where a message carried or relayed here was written.
    written_in: Text = "the native Chat"
    /// When it was written there, in milliseconds since 1970; 0 for a message written here.
    written: Int = 0
    /// The reactions it had in the native Chat, each emoji with how many reacted with it: who
    /// reacted there is not carried, since this app names people only as its own members.
    reacted: Text = ""
    /// What people gave it where it was written, for a message relayed here from another
    /// service: each emoji, who gave it there, and the name that place shows for them. Kept
    /// apart from `reactions`, which holds this app's members, and shown under their name there.
    relayed_reactions: Collection<RelayedReaction>
    /// The files it carries, handed over with it: what a bridge relays from another service, and
    /// what anyone sends with a message.
    attachments: Collection<Attached>

    search {
        kind message
        filter pinned (icon: .pin)
    }
}

/// One person's report of a message to the workspace's moderators, kept by the conversation
/// the message is in so it outlives the message: who filed it is its history's. It names who
/// reported whom, so only its reporter and the workspace's moderators read it, whoever
/// moderates when it is read; everyone else reading the conversation sees no report at all.
type Report (readers: author and moderator) {
    /// The message reported; none once it was deleted.
    message: Option<Message>
    /// Who wrote the reported message.
    person: Option<Actor>
    /// What the message said when it was reported, read again after it was deleted.
    said: Text
    /// Why, in the reporter's words.
    reason: Text
    /// What a moderator, or the workspace's moderation agent, decided about it: empty while it
    /// is open.
    decision: Text = ""
    /// Why a moderator overturned that decision, which opens the report again: empty while the
    /// decision stands. Who decided and who overturned, and when, are the report's history.
    overturned: Text = ""
    /// The decision banned the person from the workspace: overturning it lets them back in.
    banned: Bool = false
}

/// A side conversation branched off one message.
type Thread {
    name: Text
    /// What has been said here so far, in a few paragraphs, as an agent last wrote it down: what
    /// a turn reads in place of the messages it summarizes. Empty until one is written.
    summary: Text = ""
    messages: Collection<Message>
    archived: Bool = false
    /// The reports filed about its messages, for the workspace's moderators.
    reports: Collection<Report>
}

/// A private conversation between people of the workspace, in the same message model as a
/// channel: the person who opened it and the one they opened it with, or one person alone for
/// their notes to self. Only its `people` read it and what it holds; no other member does, the
/// workspace's owner included. Its place in the sidebar is the other person's row
/// (`ui.NavItem(person: ...)`).
type Direct (readers: people) {
    name: Text
    /// Who is in it.
    people: Set<Actor>
    /// The one person an earlier version kept it with, before it said who is in it: what its
    /// migration reads (`carry_direct_people`).
    person: Actor
    /// What has been said here so far, in a few paragraphs, as an agent last wrote it down: what
    /// a turn reads in place of the messages it summarizes. Empty until one is written.
    summary: Text = ""
    messages: Collection<Message>
    threads: Collection<Thread>
    /// The reports filed about its messages, for the workspace's moderators.
    reports: Collection<Report>
    /// The outside service this conversation is carried to, as its people read it ("Discord");
    /// empty for one that stays here.
    service: Text = ""
    /// Its id there, when a bridge carries it: the channel, category or conversation there.
    outside_id: Text = ""

    search {
        kind conversation
        people people
    }
}

/// What a person is writing in a conversation and has not sent. The composer binds its `body`, so
/// what is typed survives the window and reaches the person's other devices; the send that makes
/// the message empties it. A draft is read by its author alone (`readers: author`), so a
/// half-written message is theirs until it goes.
type Draft (readers: author) {
    /// The conversation it is written in: a channel, a thread or a direct conversation.
    conversation: TextChannel | Thread | Direct
    body: RichText
    /// The files it will carry, in the order they were chosen: the system's chooser adds them,
    /// the composer draws them, and the send that makes the message takes them onto it.
    files: List<File>
}

/// A file a message carries, as the place it was copied from keeps it: its name, what it is, how
/// many bytes, and the address of its content.
type Attached {
    name: Text
    media: Text
    size: Int = 0
    /// The address its bytes read at: `tilo-blob://<hash>`.
    blob: Text
}

/// A reaction someone gave a message where it was written, relayed here with the message: its
/// emoji, the account there that gave it, and the name that place shows for them.
type RelayedReaction {
    emoji: Text
    /// Who gave it there, as the relay names them: the service and the account (`discord/3001`).
    persona: Text
    /// The name that place shows for them.
    by: Text
}

/// A piece of a message kept as parts rather than as rich text: its text, or a block's source.
type Part {
    /// Whether the piece is text or a block's source.
    kind: Text merge register
    body: Text
}

/// How one kind of work is done in this workspace: what a person writes down once, and what an
/// agent reads before it starts that work.
/// Carries a conversation to an outside service: its name as people read it ("Discord") and
/// the id it has there. The bridge's own actions keep what joins the two sides; this is what
/// every view reads to show the place a conversation is also said in.
action bridge(
    c: TextChannel | VoiceChannel | Category | Direct,
    service: Text,
    outside_id: Text = "",
) {
    require service.trim() != "" "Say which service carries the conversation"
    match c {
        ch: TextChannel => {
            ch.service = service.trim()
            ch.outside_id = outside_id
        }
        v: VoiceChannel => {
            v.service = service.trim()
            v.outside_id = outside_id
        }
        category: Category => {
            category.service = service.trim()
            category.outside_id = outside_id
        }
        d: Direct => {
            d.service = service.trim()
            d.outside_id = outside_id
        }
    }
}

/// Keeps a conversation here: no service carries it any more.
action unbridge(c: TextChannel | VoiceChannel | Category | Direct) {
    match c {
        ch: TextChannel => {
            ch.service = ""
            ch.outside_id = ""
        }
        v: VoiceChannel => {
            v.service = ""
            v.outside_id = ""
        }
        category: Category => {
            category.service = ""
            category.outside_id = ""
        }
        d: Direct => {
            d.service = ""
            d.outside_id = ""
        }
    }
}

type Skill (untitled: "New skill") {
    /// What it is called, in a few words: what an agent names it by.
    name: Text
    /// The kind of work it is for, in a sentence or two: when it applies.
    work: Text
    /// How that work is done here.
    how: Text
    /// The form the result takes; empty when it has no fixed form.
    form: Text = ""
    /// The model the work is done with, as `provider/model`; empty for the person's choice for
    /// the kind of work.
    model: Text = ""
    /// Results the person accepted and rejected, oldest first: how the work is done, shown by
    /// what came of it.
    examples: Collection<Example>
}

/// A result a person accepted or rejected, kept with its skill as an example of how that work is
/// done: more like this, or less like this.
type Example {
    /// The result, or the part of it that is meant.
    text: Text
    /// Whether the person accepted it (more like this) or rejected it (less like this).
    accepted: Bool
    /// What the person said about it; empty when they said nothing more.
    note: Text = ""
    /// When it was kept where this skill came from, in milliseconds since 1970; 0 for one kept
    /// here, which the skill's history says when.
    at: Int = 0
}

default view sidebar for Conversations

default view channel for TextChannel

default view direct for Direct

default view thread for Thread

default view voice for VoiceChannel

row view channel_summary for TextChannel

row view direct_summary for Direct

row view category_summary for Category

row view voice_summary for VoiceChannel

place view turn for Message

default view skill for Skill

row view skill_row for Skill

/// Adds a text channel to the sidebar, under `category` when it is given and about `topic` when
/// that is.
primary action new_channel(
    all: Conversations,
    name: Text = std.say("new-channel"),
    topic: Text = "",
    category: Option<Category> = .none,
) -> TextChannel (icon: .plus, label: "New channel") {
    let made = TextChannel(name: name, topic: topic)
    if let c = category { c.channels.insert(made) } else { all.channels.insert(made) }
}

/// Adds a category to the sidebar.
action new_category(all: Conversations, name: Text = std.say("New category")) -> Category (
    icon: .folder_plus,
    label: "New category",
) {
    all.channels.insert(Category(name: name))
}

/// Adds a voice channel to the sidebar, under `category` when it is given.
action new_voice_channel(
    all: Conversations,
    name: Text = std.say("Voice"),
    category: Option<Category> = .none,
) -> VoiceChannel (icon: .volume_2, label: "New voice channel") {
    let made = VoiceChannel(name: name)
    if let c = category { c.channels.insert(made) } else { all.channels.insert(made) }
}

/// Starts a skill, called `name`. What it is for and how its work is done are written on its
/// page, or by an agent told to share the guidance with the workspace.
action new_skill(all: Conversations, name: Text = "") -> Skill (icon: .plus, label: "New skill") {
    require name == "" or not all.skills.any(s -> s.name == name) "The workspace already has a skill called {name}"
    all.skills.insert(Skill(name: name))
}

/// Corrects a skill: only what is given changes, and what is left out stays as it is.
action correct_skill(
    s: Skill,
    name: Text = s.name,
    work: Text = s.work,
    how: Text = s.how,
    form: Text = s.form,
    model: Text = s.model,
) (icon: .pencil, label: "Correct skill") {
    require name == ""
        or name == s.name
        or not conversations.skills.any(other -> other != s and other.name == name) "The workspace already has a skill called {name}"
    s.name = name
    s.work = work
    s.how = how
    s.form = form
    s.model = model
}

/// Forgets a skill: no later turn reads it.
action forget_skill(s: Skill) (icon: .trash_2, label: "Forget skill") {
    conversations.skills.remove(s)
}

/// Keeps an example of a skill: a result the person accepted (more like this) or rejected (less
/// like this), with what they said about it. The same result said again is that example, moved
/// newest, and a skill keeps its newest twelve.
action keep_example(s: Skill, text: Text, accepted: Bool, note: Text = "") (
    icon: .bookmark,
    label: "Keep an example",
) {
    require text.trim() != "" "An example says what the result was"
    let text = text.trim()
    let note = note.trim()
    if let known = s.examples.find(e -> e.text == text) {
        known.accepted = accepted
        known.note = note
        if let last = s.examples.last() {
            if last != known { s.examples.move(known, after: last) }
        }
    } else {
        s.examples.insert(Example(text: text, accepted: accepted, note: note))
        if s.examples.len() > 12 {
            if let first = s.examples.first() { s.examples.remove(first) }
        }
    }
}

/// Takes an example out of a skill.
action drop_example(s: Skill, example: Example) (icon: .trash_2, label: "Remove the example") {
    s.examples.remove(example)
}

/// The direct conversations an earlier version kept with one person alone (`person`), which
/// every member could read: in each are that person and whoever opened it, as its history says.
/// One no person opened (its maker is not known, or was software) is read by that person alone,
/// never by everyone.
migration action carry_direct_people(all: Conversations) {
    for item in all.channels {
        match item {
            c: Category => {}
            ch: TextChannel => {}
            v: VoiceChannel => {}
            d: Direct => {
                if d.people.is_empty() {
                    d.people.add(d.person)
                    if let opener = created(d)?.who {
                        if opener.kind() == .person { d.people.add(opener) }
                    }
                }
            }
        }
    }
}

/// The conversations the native Chat kept before this app took its id over, carried into these
/// once: its categories and channels in the sidebar's order, each with its name and topic, and
/// every message of a channel with what it said, whether it was pinned and the id it had there.
/// A voice channel stays a voice channel, under the id it had. The replies in a message's thread
/// are the messages of that message's thread, a reply quoting it answers it, and who wrote each
/// message, when, and its reactions are kept with it. A problem report filed there is a message of
/// the channel it was filed in, among the others by when it was filed, and that channel is the one
/// [`report_problem`] files in now. What this app has no place for is set aside in a category of its
/// own instead of dropped: an archived channel, as a channel holding its messages. Each direct
/// conversation is a direct conversation here too, of the people its id names, and read by them
/// alone. The workspace's skills come too: what each is called, the work it is for, how the work
/// is done here, the form of its result and the model, under the id it had there and in the order
/// it stood, with the examples it kept, oldest first, each with what the result was, whether the
/// person accepted it, what they said and when it was kept. Everything is carried in this one
/// migration, so the carrying is one change, and one step takes it back. The native app's own
/// documents are left as they are.
migration action carry_native_chat(all: Conversations) {
    let earlier = earlier(all)
    // Oldest first: when each was written, the same time by id.
    let messages = earlier
        .filter(e -> e.of == "messages" or e.of == "feedback")
        .sort_by(e -> e.id)
        .sort_by(e -> e.texts.get("ts") ?? "")
    let reported_in = earlier.filter(e -> e.of == "feedback").map(e -> channel_of(e))
    let categories = earlier.filter(e -> e.of == "categories")
    let known = categories.map(c -> c.id)
    let channels = earlier.filter(e -> e.of == "channels")
    let shown = channels.filter(e -> not set_aside(e))
    // Each conversation carried, by the id it had in the native app; the one reports went to
    // under the key reports go to now.
    let carried: Map<TextChannel> = Map<TextChannel>()
    for ch in shown.filter(e -> not known.contains(e.texts.get("category") ?? "")) {
        if was_voice(ch) {
            all.channels.insert(VoiceChannel(name: ch.texts.get("name") ?? ""), id: ch.id)
        } else {
            let made = if reported_in.contains(ch.id) or (ch.texts.get("name") ?? "") == "feedback" {
                all.channels.insert(native_channel(ch), key: "feedback")
            } else {
                all.channels.insert(native_channel(ch), id: ch.id)
            }
            carried.put(ch.id, made)
        }
    }
    for cat in categories {
        let c = all.channels.insert(Category(name: cat.texts.get("name") ?? ""), id: cat.id)
        for ch in shown.filter(e -> (e.texts.get("category") ?? "") == cat.id) {
            if was_voice(ch) {
                c.channels.insert(VoiceChannel(name: ch.texts.get("name") ?? ""), id: ch.id)
            } else {
                carried.put(ch.id, c.channels.insert(native_channel(ch), id: ch.id))
            }
        }
    }
    let aside = channels.filter(e -> set_aside(e))
    // A direct conversation is kept nowhere but in the ids of its messages' conversation, which
    // name its people: it comes in as a direct conversation of theirs, read by them alone.
    let directs: Map<Direct> = Map<Direct>()
    for m in messages {
        let to = channel_of(m)
        if to.starts_with("direct:") and directs.get(to) == .none {
            let people = m.members.get("channel") ?? []
            let d = all.channels.insert(Direct(name: members_of(to)), id: to)
            for p in people {
                d.people.add(p)
            }
            directs.put(to, d)
        }
    }
    if not aside.is_empty() {
        let c = all.channels.insert(Category(name: "Set aside"), id: "set-aside")
        for ch in aside {
            let kind = if was_voice(ch) {
                "An archived voice channel"
            } else { "An archived channel" }
            let topic = ch.texts.get("topic") ?? ""
            let made = c.channels.insert(
                TextChannel(
                    name: ch.texts.get("name") ?? "",
                    topic: if topic == "" {
                        "{kind} of the native Chat"
                    } else { "{kind} of the native Chat: {topic}" },
                ),
                id: ch.id,
            )
            carried.put(ch.id, made)
        }
    }
    for to in carried.keys() {
        if let ch = carried.get(to) {
            let here = messages.filter(e -> channel_of(e) == to)
            let tops = here.filter(e -> (e.texts.get("parent") ?? "") == "")
            let ids = tops.map(e -> e.id)
            // A reply whose message is gone stands in the conversation where it was written.
            let stays = here.filter(e -> not ids.contains(e.texts.get("parent") ?? ""))
            let posted: Map<Message> = Map<Message>()
            for e in stays {
                let m = if e.of == "feedback" {
                    ch.messages.insert(native_report(e), id: "m_feedback_{e.id}")
                } else {
                    ch.messages.insert(native_message(e), id: e.id)
                }
                posted.put(e.id, m)
            }
            for e in tops {
                let replies = here.filter(r -> (r.texts.get("parent") ?? "") == e.id)
                if let m = posted.get(e.id), first = replies.first() {
                    let t = ch.threads.insert(Thread(name: m.body.excerpt))
                    m.thread = t
                    for r in replies {
                        let reply = t.messages.insert(native_message(r), id: r.id)
                        if (r.lists.get("quoted") ?? []).len() > 0 {
                            reply.reply_to = m
                        }
                    }
                }
            }
        }
    }
    for to in directs.keys() {
        if let d = directs.get(to) {
            let here = messages.filter(e -> (e.texts.get("channel") ?? "") == to)
            let tops = here.filter(e -> (e.texts.get("parent") ?? "") == "")
            let ids = tops.map(e -> e.id)
            let stays = here.filter(e -> not ids.contains(e.texts.get("parent") ?? ""))
            let posted: Map<Message> = Map<Message>()
            for e in stays {
                posted.put(e.id, d.messages.insert(native_message(e), id: e.id))
            }
            for e in tops {
                let replies = here.filter(r -> (r.texts.get("parent") ?? "") == e.id)
                if let m = posted.get(e.id), first = replies.first() {
                    let t = d.threads.insert(Thread(name: m.body.excerpt))
                    m.thread = t
                    for r in replies {
                        let reply = t.messages.insert(native_message(r), id: r.id)
                        if (r.lists.get("quoted") ?? []).len() > 0 {
                            reply.reply_to = m
                        }
                    }
                }
            }
        }
    }
    // The workspace's skills, each under the id it had there and in the order it stood, with
    // the examples it kept, oldest first, and when each was kept there.
    for e in earlier.filter(e -> e.of == "skills") {
        let skill = all.skills.insert(
            Skill(
                name: e.texts.get("name") ?? "",
                work: e.texts.get("work") ?? "",
                how: e.texts.get("how") ?? "",
                form: e.texts.get("form") ?? "",
                model: e.texts.get("model") ?? "",
            ),
            id: e.id,
        )
        for example in e.records.get("examples") ?? [] {
            skill.examples.insert(
                Example(
                    text: example.get("text") ?? "",
                    accepted: (example.get("accepted") ?? "") == "true",
                    note: example.get("note") ?? "",
                    at: (example.get("at") ?? "").to_int() ?? 0,
                ),
            )
        }
    }
}

/// Who is in a direct conversation `opener` opens with `who`: the two of them, or `opener` alone
/// for their notes to self.
fn people_of(opener: Actor, who: Actor) -> List<Actor> {
    if opener == who { [opener] } else { [opener, who] }
}

/// Whether `people` are the people of `d`, each once, in any order.
fn same_people(d: Direct, people: List<Actor>) -> Bool {
    d.people.len() == people.len() and people.all(p -> d.people.contains(p))
}

/// Who of `d`'s people is not `me`: the person the conversation is with, none for notes to self.
fn other_in(d: Direct, me: Actor) -> Option<Actor> {
    d.people.filter(p -> p != me).first()
}

/// Whether the native channel `e` has no place among this app's channels: an archived one.
fn set_aside(e: host.Earlier) -> Bool {
    (e.texts.get("archived") ?? "") == "true"
}

/// Whether the native channel `e` was a voice channel.
fn was_voice(e: host.Earlier) -> Bool {
    (e.texts.get("kind") ?? "").lower() == "voice"
}

/// The native channel `e` as a channel of this app: its name and its topic.
fn native_channel(e: host.Earlier) -> TextChannel {
    TextChannel(name: e.texts.get("name") ?? "", topic: e.texts.get("topic") ?? "")
}

/// The native message `e` as a message of this app: what it said, whether it was pinned, and
/// who wrote it there, when, and its reactions.
fn native_message(e: host.Earlier) -> Message {
    Message(
        body: RichText(e.texts.get("text") ?? ""),
        pinned: (e.texts.get("pinned") ?? "") == "true",
        by: e.texts.get("author_name") ?? member_named(e.texts.get("author") ?? ""),
        written: (e.texts.get("ts") ?? "").to_int() ?? 0,
        reacted: std.reactions_said(e.lists.get("reactions") ?? []),
    )
}

/// The native conversation the message or report `e` was written in.
fn channel_of(e: host.Earlier) -> Text {
    if e.of == "feedback" {
        e.texts.get("submission.destination.channel") ?? ""
    } else { e.texts.get("channel") ?? "" }
}

/// A problem report filed in the native Chat as a message of this app: what the person wrote and
/// the apps they picked things in, and who filed it there, when, and its reactions.
fn native_report(e: host.Earlier) -> Message {
    let words = e.texts.get("submission.feedback.text") ?? ""
    let apps = (e.lists.get("submission.feedback.selections.app_name") ?? []).filter(a -> a != "")
    Message(
        body: RichText(if apps.is_empty() { words } else { "{words} (in {apps.join(", ")})" }),
        by: member_named(e.texts.get("author") ?? ""),
        written: (e.texts.get("ts") ?? "").to_int() ?? 0,
        reacted: reactions_said(e.lists.get("reactions") ?? []),
    )
}

/// The members a native direct conversation's id names, each as [`member_named`] says it.
fn members_of(id: Text) -> Text {
    member_named(id.replace("direct:", "").replace("[", "").replace("]", "")).replace(",", ", ")
}

/// A member as the native app stored one (`{"person":"<id>"}`): the id it names.
fn member_named(stored: Text) -> Text {
    stored
        .replace("\{\"person\":\"", "")
        .replace("\{\"agent\":\"", "")
        .replace("\{\"bot\":\"", "")
        .replace("\"\}", "")
}

/// The native reactions `keys` (each its emoji, then who reacted) as words: each emoji and how
/// many reacted with it, in the order they were first used.
fn reactions_said(keys: List<Text>) -> Text {
    said_from(keys.map(k -> emoji_of(k)), 0, [])
}

/// [`reactions_said`] of `emoji` from the `at`th, past the emoji already `told`.
fn said_from(emoji: List<Text>, at: Int, told: List<Text>) -> Text {
    if let e = emoji.at(at) {
        if told.contains(e) {
            said_from(emoji, at + 1, told)
        } else {
            let here = "{e} {emoji.count(x -> x == e)}"
            let rest = said_from(emoji, at + 1, [told, [e]].flat_map(x -> x))
            if rest == "" { here } else { "{here}  {rest}" }
        }
    } else { "" }
}

/// The emoji a native reaction's key names: what stands before its first separator. The key
/// ends in who reacted, written in hex, after the last one.
fn emoji_of(key: Text) -> Text {
    let unhexed = without_hex(key)
    let separator = unhexed.slice(unhexed.len() - 1, unhexed.len())
    key.split(separator).first() ?? key
}

/// `key` without the hex digits it ends in.
fn without_hex(key: Text) -> Text {
    if key.len() > 0 and "0123456789abcdef".contains(key.slice(key.len() - 1, key.len())) {
        without_hex(key.slice(0, key.len() - 1))
    } else { key }
}

/// Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback
/// channel, which the first report opens.
///
/// The people who read the reports there answer, react and start a thread under one as under any
/// message. The channel is made under one key on every device, so reports filed apart open one.
action report_problem(all: Conversations, body: RichText) -> Message (
    icon: .flag,
    label: "Report a problem",
    did: "reported a problem",
) {
    require not body.is_empty() "Say what went wrong first"
    send(all.channels.insert(TextChannel(name: "feedback"), key: "feedback"), body)
}

/// Opens the direct message between the invoker and `who`, in the sidebar, or the one already
/// open between the two of them: two people have one conversation, not a new empty one each
/// time. With yourself it is your notes to self. A person is someone of the workspace: one who
/// is not in it is refused, since no one but its members reads what is written here.
action message_person(all: Conversations, who: Actor) -> Direct (
    icon: .message_circle,
    label: "Message",
) {
    require in_workspace(who, invoker) "{who.name()} is not in this workspace"
    let people = people_of(invoker, who)
    if let d = direct_of(all, people) {
        return d
    }
    // Named by its people, so two devices opening it apart, offline, open the same one.
    let key = people.map(p -> p.id()).sort_by(x -> x).join(",")
    let d = all.channels.insert(Direct(name: who.name()), key: "direct:{key}")
    // Its fields written, so the resource holds the conversation from its first press: an item
    // stored by its id alone is found only once it says something.
    d.name = who.name()
    for p in people {
        d.people.add(p)
    }
    d
}

/// Whether `who` may be written to here by `me`: an agent, `me`, or a person of the workspace.
/// A host that knows no membership (none but a workspace's own policy names it) refuses no one
/// by it: the app's own seeded conversations are people of no real workspace, and refusing
/// every one of them would leave the workspace without the conversations it ships with.
fn in_workspace(who: Actor, me: Actor) -> Bool {
    who.kind() != .person
        or who == me
        or workspace.members.people.is_empty()
        or workspace.members.people.contains(who)
}

/// The direct conversation between the invoker and `who`, none while the two of them have none:
/// what an agent, a script or another app reads to find it, opening and making nothing. With
/// yourself it is your notes to self.
action conversation_with(all: Conversations, who: Actor) -> Option<Direct> (
    icon: .message_circle,
    label: "Conversation with",
) {
    direct_of(all, people_of(invoker, who))
}

/// The direct conversation of `people`, each once, none while they have none.
fn direct_of(all: Conversations, people: List<Actor>) -> Option<Direct> {
    for item in all.channels {
        match item {
            c: Category => {}
            ch: TextChannel => {}
            v: VoiceChannel => {}
            d: Direct => {
                if same_people(d, people) {
                    return d
                }
            }
        }
    }
    .none
}

/// How many messages of a direct conversation the person asking has not read: the number its row
/// shows beside the other person, and what an agent or a script reads of it.
action unread_count(d: Direct) -> Int (icon: .mail, label: "Unread") {
    unread_of(d)
}

/// How many of `d`'s messages the person looking has not seen: those from the first they have
/// not ([`first_unseen`]) to the last.
fn unread_of(d: Direct) -> Int {
    if let first = first_unseen(d) {
        d.messages.len() - d.messages.filter(m -> made_before(m, first)).len()
    } else { 0 }
}

/// What the person is half-way through writing in `t`, as its row in the sidebar says it: the
/// first line of their draft, and nothing for a conversation with no draft of theirs.
fn draft_line(writing: List<Draft>, t: TextChannel | Thread | Direct) -> Option<Text> {
    for d in writing {
        if d.conversation == t and not d.body.is_empty() {
            let line = d.body.excerpt
            return .some(std.say("Draft: {line}"))
        }
    }
    .none
}

/// What is working in a conversation: the agent's turn that one of its messages answers in,
/// while that turn runs or waits on the person. None when nothing is, and then the composer's
/// button sends.
fn working_turn(said: List<Message>) -> Option<host.Invoked> {
    for m in said {
        if m.turn != "" {
            if let t = invoked(m.turn) {
                if t.state == "running" or t.state == "waiting" {
                    return t
                }
            }
        }
    }
    .none
}

/// What one of an agent's answers cost, in US dollars: what its turn's model calls cost, which
/// is what its line shows beside the model. Nothing for a message a person wrote.
fn answer_cost(m: Message) -> Float {
    if m.turn == "" { 0.0 } else if let t = invoked(m.turn) { t.cost } else { 0.0 }
}

/// What the agent's answers in a conversation have cost so far, in US dollars.
fn answers_cost(said: List<Message>) -> Float {
    said.map(m -> answer_cost(m)).sum()
}

/// Whether the agent's turn that wrote `m` ended, as it may: stopped, broken off or done. An
/// answer that is still being written, or a person's message, has nothing to continue.
fn answer_ended(m: Message) -> Bool {
    if m.turn == "" {
        false
    } else if let t = invoked(m.turn) {
        t.state == "ended" or t.state == "failed" or t.state == "cancelled"
    } else { false }
}

/// How someone is in a call, as the sidebar says it under their name: their microphone or
/// hearing off, their camera on, their screen shared. Talking shows as the ring on their face.
/// Nothing while they only listen.
fn call_state(m: host.CallMember) -> Text {
    let says = [
        if m.deafened { std.say("Deafened") } else if m.muted { std.say("Muted") } else { "" },
        if m.camera { std.say("Camera on") } else { "" },
        if m.screen { std.say("Sharing their screen") } else { "" },
    ]
    says.filter(x -> x != "").join(", ")
}

/// Whether a relayed message may be linked to `member`: no one, or a person of the workspace.
fn member_here(member: Option<Actor>, me: Actor) -> Bool {
    if let p = member {
        p.kind() == .person and in_workspace(p, me)
    } else { true }
}

/// Whether `x` was made before `y`.
fn made_before(x: host.Resource, y: host.Resource) -> Bool {
    if let made = created(x)?.at {
        if let later = created(y)?.at { made < later } else { true }
    } else { false }
}

/// Opening a channel makes the draft its person writes in, when they have none: a press away and
/// back finds what they typed, on every device of theirs, and nobody else reads it.
opened action open_channel(c: TextChannel) {
    let all = root Conversations
    if not all.drafts.any(d -> d.conversation == c and (created(d)?.mine ?? false)) {
        all.drafts.insert(Draft(conversation: c, body: RichText("")))
    }
}

/// [`open_channel`] for a direct conversation.
opened action open_direct(c: Direct) {
    let all = root Conversations
    if not all.drafts.any(d -> d.conversation == c and (created(d)?.mine ?? false)) {
        all.drafts.insert(Draft(conversation: c, body: RichText("")))
    }
}

/// [`open_channel`] for a thread.
opened action open_thread(t: Thread) {
    let all = root Conversations
    if not all.drafts.any(d -> d.conversation == t and (created(d)?.mine ?? false)) {
        all.drafts.insert(Draft(conversation: t, body: RichText("")))
    }
}

/// Whether a send names nothing to say: the draft's words when it sends one, else the body given.
fn empty_writing(draft: Option<Draft>, body: RichText) -> Bool {
    if let writing = draft { writing.body.is_empty() } else { body.is_empty() }
}

/// Posts a message in a channel, a thread or a direct conversation as the invoker, answering
/// another when `reply_to` names one. History and the catch-up say what a person did in its own
/// words, counting the messages a step of them holds. A message in a direct conversation is news
/// to the other people in it: each is told once, on every device of theirs, who wrote and what,
/// and choosing the notice opens the conversation.
///
/// A composer names the person's `draft`: what it holds is the message, and the draft is emptied in
/// the same change, so the press is one step to undo. What is sent without a draft is the `body`
/// given, as a program writing a message says it. A reply an agent's `turn` writes begins empty
/// and takes the model's words as it writes them.
action send(
    to: TextChannel | Thread | Direct,
    body: RichText = RichText(""),
    reply_to: Option<Message> = .none,
    attachments: List<File> = [],
    turn: Text = "",
    draft: Option<Draft> = .none,
) -> Message uses notify (
    icon: .send,
    label: "Send",
    did: "wrote a message",
    did_many: "wrote {count} messages",
) {
    require turn != "" or not empty_writing(draft, body) "Type a message first"
    let words = if let writing = draft { writing.body } else { body }
    // What the person chose for the message waits on their draft; what a program sends with it
    // comes as an argument.
    let carried = if let writing = draft {
        [attachments, writing.files].flat_map(x -> x)
    } else { attachments }
    let m = Message(body: words, reply_to: reply_to, turn: turn)
    let sent = match to {
        c: TextChannel => c.messages.insert(m),
        t: Thread => t.messages.insert(m),
        d: Direct => {
            let sent = d.messages.insert(m)
            for p in d.people.filter(p -> p != invoker and p.kind() == .person) {
                notify(to: p, title: invoker.name(), body: words.text, opens: d)
            }
            sent
        }
    }
    // The files go onto the message once it stands: a collection belongs to a resource, and a
    // message under construction is none yet.
    for f in carried {
        sent.attachments.insert(
            Attached(name: f.name, media: f.media, size: f.size(), blob: f.url()),
        )
    }
    if let d = draft {
        d.body = RichText("")
        d.files = []
    }
    sent
}

/// Adds the files a person chose to the message they are writing: the system's chooser runs this
/// with what was chosen, and the draft holds them until it is sent. The chooser offers every kind
/// of file, since a message carries any.
action attach(d: Draft, files: List<File>) (
    icon: .paperclip,
    label: "Attach files",
    did: "attached files",
) {
    d.files = [d.files, files].flat_map(x -> x)
}

/// Takes a file off the message a person is writing, before it is sent.
action detach(d: Draft, file: File) (icon: .x, label: "Remove the file") {
    d.files = d.files.filter(f -> f.url() != file.url())
}

/// Opens the file a message carries in the app that opens its kind: the file reaches that app as
/// the same content both read, never as a path neither has.
action open_attachment(f: Attached) uses files (icon: .external_link, label: "Open attachment") {
    match File.open(File.at(f.name, f.blob)) {
        .ok => {}
        .err(_) => {}
    }
}

/// The content address an attachment's bytes read at: the hash after `tilo-blob://`.
fn attached_hash(f: Attached) -> Text {
    if let at = f.blob.find("://") { f.blob.slice(at + 3, f.blob.len()) } else { f.blob }
}

/// What the workspace's host stopped keeping of `f`, when it stopped keeping it: a limit said so,
/// and every device reads the host's refusals as the workspace's content. The bytes stay where
/// they were posted, so the file still opens from whoever keeps it.
fn refusal_of(f: Attached) -> Option<workspace.Refusal> {
    workspace.content.refusals.find(r -> r.hash == attached_hash(f))
}

/// Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the
/// one already open, else a new one in the sidebar. The message is sent as every message is.
action write_to(all: Conversations, who: Actor, body: RichText) -> Message uses notify (
    icon: .send,
    label: "Write to",
) {
    let d = message_person(all, who)
    send(d, body)
}

/// Writes down what has been said in a conversation so far, replacing what was written before:
/// what an agent's next turn there reads in place of the messages it covers.
action summarize(to: TextChannel | Thread | Direct, summary: Text) (
    icon: .file_text,
    label: "Summarize",
) {
    match to {
        c: TextChannel => {
            c.summary = summary
        }
        t: Thread => {
            t.summary = summary
        }
        d: Direct => {
            d.summary = summary
        }
    }
}

/// Sends a view as a message: its title as what the message says, its definition for anyone
/// who reads it to add to their own views.
action send_view(to: TextChannel | Thread | Direct, title: Text, definition: Text) -> Message (
    icon: .eye,
    label: "Send a view",
) {
    require definition != "" "The view is empty"
    let m = Message(body: RichText(title), sent_view: definition)
    match to {
        c: TextChannel => c.messages.insert(m),
        t: Thread => t.messages.insert(m),
        d: Direct => d.messages.insert(m),
    }
}

/// Shares one of the person's windows live into a conversation: the shell shares the window with
/// the workspace, as its own window bar does, and the message refers to that share, so the
/// conversation shows the window live where the message stands. Its sharer chooses whether the
/// members viewing it may control it.
action share_window(
    to: TextChannel | Thread | Direct,
    window: Text (label: "Window"),
    can_edit: Bool = false,
) -> Message (icon: .screen_share, label: "Share a window", did: "shared a window") {
    require device.windows.all.any(w -> w.key == window and not w.elsewhere) "Only a window open on this device can be shared from here"
    let share = if let w = device.windows.all.find(w -> w.key == window) {
        LiveShare(
            window: window,
            workspace: w.workspace_key,
            title: w.title,
            app_name: w.origin_app,
            icon: w.icon,
            can_edit: can_edit,
        )
    } else {
        LiveShare(window: window, can_edit: can_edit)
    }
    if not device.shares.sharing.any(x -> x.tab == window) {
        device.share(device.shares, tab: window)
    }
    device.allow_remote_input(device.shares, tab: window, on: can_edit)
    let m = Message(body: RichText(share.title), live_share: .some(share))
    match to {
        c: TextChannel => c.messages.insert(m),
        t: Thread => t.messages.insert(m),
        d: Direct => d.messages.insert(m),
    }
}

/// Lets the members viewing a window shared into a conversation control it, or only view it: the
/// shell's share changes, and the message says so.
action set_share_access(m: Message, can_edit: Bool) (
    icon: .mouse_pointer_2,
    label: "Change what others may do",
) {
    require created(m)?.who == invoker "Only whoever shared the window can change what others may do there"
    require still_shared(m) "The window is no longer shared"
    if let s = m.live_share {
        device.allow_remote_input(device.shares, tab: s.window, on: can_edit)
        m.live_share = .some(shared_as(s, can_edit: can_edit, ended: false))
    }
}

/// Stops sharing a window shared into a conversation: the shell's share ends, and the message
/// says the window is no longer shared in its place.
action stop_sharing(m: Message) (
    icon: .eye_off,
    label: "Stop sharing",
    did: "stopped sharing a window",
) {
    require created(m)?.who == invoker "Only whoever shared the window can stop sharing it"
    require still_shared(m) "The window is no longer shared"
    if let s = m.live_share {
        device.stop(device.shares, tab: s.window)
        m.live_share = .some(shared_as(s, can_edit: s.can_edit, ended: true))
    }
}

/// Whether `m` shares a window its sharer has not stopped sharing.
fn still_shared(m: Message) -> Bool {
    if let s = m.live_share { not s.ended } else { false }
}

/// `s` as it stands after a change of what others may do there, or its end.
fn shared_as(s: LiveShare, can_edit: Bool, ended: Bool) -> LiveShare {
    LiveShare(
        window: s.window,
        workspace: s.workspace,
        title: s.title,
        app_name: s.app_name,
        icon: s.icon,
        can_edit: can_edit,
        ended: ended,
    )
}

/// Posts a message written on another service, as it was written there: who wrote it, as that
/// service names them, where and when. A bridge relays another service's conversation this
/// way; the message's history names the bridge, and the message says whom it relays.
action relay(
    to: TextChannel | Thread | Direct,
    body: RichText,
    by: Text,
    written_in: Text,
    written: Int = 0,
    attachments: List<File> = [],
    reply_to: Option<Message> = .none,
    avatar: Text = "",
    service: Text = "",
    outside_emoji: List<OutsideEmoji> = [],
    member: Option<Actor> = .none,
) -> Message (icon: .send, label: "Relay", did: "relayed a message") {
    require by.trim() != "" "Say who wrote the message"
    require member_here(member, invoker) "The person a relayed message is linked to is not in this workspace"
    let m = Message(
        body: body,
        reply_to: reply_to,
        by: by.trim(),
        written_in: written_in,
        written: written,
        outside: .some(
            OutsideAuthor(
                name: by.trim(),
                avatar: avatar,
                service: if service == "" { written_in } else { service },
                member: member,
            ),
        ),
    )
    let sent = match to {
        c: TextChannel => c.messages.insert(m),
        t: Thread => t.messages.insert(m),
        d: Direct => d.messages.insert(m),
    }
    // The files go onto the message once it stands: a collection belongs to a resource, and a
    // message under construction is none yet.
    for f in attachments {
        sent.attachments.insert(
            Attached(name: f.name, media: f.media, size: f.size(), blob: f.url()),
        )
    }
    for e in outside_emoji {
        sent.outside_emoji.insert(e)
    }
    sent
}

/// Relays a reaction someone gave a message where it was written, as that place names them: the
/// counterpart of `relay` for a reaction. A bridge carrying another service's conversation here
/// makes these, so a reaction made there shows under the person's name rather than the bridge's;
/// `given` false takes it back. The reaction's history names the bridge, and the message says
/// whom it relays.
action relay_reaction(m: Message, emoji: Text, persona: Text, by: Text, given: Bool = true) (
    icon: .smile_plus,
    label: "Relay a reaction",
    did: "relayed a reaction",
) {
    require emoji.trim() != "" "Say which emoji"
    require persona.trim() != "" "Say who gave the reaction"
    require by.trim() != "" "Say what they are called there"
    let held = m.relayed_reactions.filter(
        r -> r.emoji == emoji.trim() and r.persona == persona.trim(),
    )
    if given {
        if held.is_empty() {
            m.relayed_reactions.insert(
                RelayedReaction(emoji: emoji.trim(), persona: persona.trim(), by: by.trim()),
                key: "reaction:{emoji.trim()}/{persona.trim()}",
            )
        } else {
            // The name that place shows may have changed since: it says the current one.
            for r in held {
                r.by = by.trim()
            }
        }
    } else {
        for r in held {
            m.relayed_reactions.remove(r)
        }
    }
}

/// Deletes a channel and what was said in it, from the sidebar or the category it is in.
destructive action delete_channel(from: Conversations | Category, ch: TextChannel | VoiceChannel) (
    icon: .trash,
    label: "Delete channel",
) {
    require moderates(invoker) "Only the workspace's moderators delete a channel"
    match from {
        all: Conversations => all.channels.remove(ch),
        c: Category => c.channels.remove(ch),
    }
}

/// Deletes a category that holds no channel.
destructive action delete_category(all: Conversations, c: Category) (
    icon: .trash,
    label: "Delete category",
) {
    require moderates(invoker) "Only the workspace's moderators delete a category"
    require c.channels.is_empty() "Move or delete its channels first"
    all.channels.remove(c)
}

/// Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among
/// the channels listed there (the end when it is negative). A move keeps the channel and what
/// was written in it.
action move_channel(
    all: Conversations,
    ch: TextChannel | VoiceChannel,
    category: Option<Category> = .none,
    at: Int = -1,
) (icon: .arrow_up_down, label: "Move channel") {
    if let c = category {
        let place = if at < 0 { c.channels.len() } else { at }
        c.channels.move(ch, to: place)
    } else {
        let place = if at < 0 { all.channels.len() } else { at }
        all.channels.move(ch, to: place)
    }
}

/// Moves a category to a place in the sidebar, at `at` among the categories and channels listed
/// there (the end when it is negative). Its own channels move with it.
action move_category(all: Conversations, c: Category, at: Int = -1) (
    icon: .arrow_up_down,
    label: "Move category",
) {
    let place = if at < 0 { all.channels.len() } else { at }
    all.channels.move(c, to: place)
}

/// Renames a text channel. What was said in it stays.
action rename_channel(ch: TextChannel, name: Text) (icon: .pencil, label: "Rename channel") {
    require moderates(invoker) "Only the workspace's moderators rename a channel"
    require name.trim() != "" "Name the channel first"
    ch.name = name.trim()
}

/// Renames a voice channel. A call in it is untouched.
action rename_voice_channel(ch: VoiceChannel, name: Text) (
    icon: .pencil,
    label: "Rename voice channel",
) {
    require moderates(invoker) "Only the workspace's moderators rename a channel"
    require name.trim() != "" "Name the channel first"
    ch.name = name.trim()
}

/// Says what a channel is for, in its topic.
action set_topic(ch: TextChannel, topic: Text) (icon: .file_text, label: "Set the topic") {
    require moderates(invoker) "Only the workspace's moderators set a channel's topic"
    ch.topic = topic.trim()
}

/// Renames a category. Its channels stay in it.
action rename_category(c: Category, name: Text) (icon: .pencil, label: "Rename category") {
    require moderates(invoker) "Only the workspace's moderators rename a category"
    require name.trim() != "" "Name the category first"
    c.name = name.trim()
}

/// Sets a channel aside, out of the sidebar, keeping everything said in it, or brings it back.
action archive_channel(ch: TextChannel, archived: Bool = not ch.archived) (
    icon: .archive,
    label: if archived { "Archive channel" } else { "Restore channel" },
) {
    require moderates(invoker) "Only the workspace's moderators archive a channel"
    ch.archived = archived
}

/// Changes what a message says. Only its author may. An agent retrying its answer writes it
/// again in the same message, naming the new turn.
action edit(
    m: Message,
    body: RichText = m.body,
    turn: Text = m.turn,
    reasoning: Text = m.reasoning,
) (icon: .pencil, label: "Edit") {
    require created(m)?.who == invoker "Only its author can edit a message"
    // An agent's answer is written as it comes, and its model may reason before it writes a
    // word: its message stands empty until then.
    require turn != "" or not body.is_empty() "Type a message first"
    m.body = body
    m.turn = turn
    m.reasoning = reasoning
}

/// Asks the agent that wrote `m` to carry on where its answer stopped: a reply to it from the
/// person, naming the agent, which the agent's next turn reads with the answer before it. An answer that ran out
/// of room, was stopped or broke off is finished this way, not written again from its start.
action continue_answer(to: TextChannel | Thread | Direct, m: Message) -> Message (
    icon: .skip_forward,
    label: "Continue",
    did: "asked an answer to continue",
) {
    require m.turn != "" "Only an agent's answer continues"
    let words = std.say(
        "Continue your answer where it stopped, without repeating it or doing anything again.",
    )
    // The reply names the agent, as a person's own "@" does: that is what starts its turn in a
    // channel, where a reply alone would not.
    let asked = Message(
        body: if let agent = created(m)?.who {
            RichText(agent, " ", words)
        } else { RichText(words) },
        reply_to: .some(m),
    )
    match to {
        c: TextChannel => c.messages.insert(asked),
        t: Thread => t.messages.insert(asked),
        d: Direct => d.messages.insert(asked),
    }
}

/// Deletes a message from the channel or thread it is in. Its author, or anyone the
/// channel's permissions allow, may: the platform undoes it rather than asking first, and the
/// removal is a version of the message, so who removed it stays answerable. Its reports stay,
/// saying it was deleted, as far as the one deleting reads them.
destructive action delete(to: TextChannel | Thread | Direct, m: Message) (
    icon: .trash,
    label: "Delete",
) {
    match to {
        c: TextChannel => {
            for r in c.reports.filter(r -> r.message == m) {
                r.message = .none
            }
            c.messages.remove(m)
        }
        t: Thread => {
            for r in t.reports.filter(r -> r.message == m) {
                r.message = .none
            }
            t.messages.remove(m)
        }
        d: Direct => {
            for r in d.reports.filter(r -> r.message == m) {
                r.message = .none
            }
            d.messages.remove(m)
        }
    }
}

/// Pins a message to its channel, or unpins it.
action pin(m: Message, pinned: Bool = not m.pinned) (
    label: if pinned { "Pin" } else { "Unpin" },
    icon: .pin,
    on: m.pinned,
) {
    m.pinned = pinned
}

/// Adds your reaction with an emoji to a message, or takes it back when you had reacted with it.
action react(m: Message, emoji: Text) (icon: .smile_plus, label: "React") {
    let mine = if let who = m.reactions.get(emoji) { who.any(p -> p == invoker) } else { false }
    if mine { m.reactions.remove(emoji, invoker) } else { m.reactions.add(emoji, invoker) }
}

/// Reports a message of a conversation to the workspace's moderators, saying why when the
/// reporter does. Who reported it is the report's history's, so moderators see who asked; the
/// conversation keeps it, with who wrote the message and what it said, after the message is
/// deleted.
action report(to: TextChannel | Thread | Direct, m: Message, reason: Text = "") -> Report (
    icon: .flag,
    label: "Report",
    did: "reported a message",
) {
    let filed = Report(
        message: m,
        person: created(m)?.who,
        said: m.body.excerpt,
        reason: reason.trim(),
    )
    match to {
        c: TextChannel => c.reports.insert(filed),
        t: Thread => t.reports.insert(filed),
        d: Direct => d.reports.insert(filed),
    }
}

/// Copies what a message says, as plain words, to the clipboard.
action copy_text(m: Message) uses clipboard (icon: .copy, label: "Copy text") {
    clipboard.copy(m.body.text)
}

/// Reports a person to the workspace's moderators, for how they behave here rather than for one
/// message: what the reporter says is the whole of it, and the moderators decide it as any other
/// report.
action report_person(all: Conversations, who: Actor, reason: Text) -> Report (
    icon: .flag,
    label: "Report person",
    did: "reported a person",
) {
    require who != invoker "You can't report yourself"
    require reason.trim() != "" "Say what happened"
    all.reports.insert(Report(message: .none, person: who, said: "", reason: reason.trim()))
}

/// Settles a report: what a moderator decided about it. A report whose decision was overturned
/// is decided again this way.
action decide(r: Report, decision: Text) (icon: .shield, label: "Decide") {
    require moderates(invoker) "Only the workspace's moderators settle a report"
    require decision.trim() != "" "Say what was decided"
    r.decision = decision.trim()
    r.overturned = ""
}

/// Overturns what was decided about a report, saying why: the report is open again, for a
/// moderator to decide anew, and the decision overturned stays in its history. A ban it decided
/// is lifted, so the person may come back.
action overturn(r: Report, why: Text) (icon: .undo_2, label: "Overturn decision") {
    require moderates(invoker) "Only the workspace's moderators overturn a decision"
    require r.decision != "" "Only a decided report can be overturned"
    require why.trim() != "" "Say why the decision is overturned"
    if r.banned {
        if let who = r.person {
            workspace.unban(workspace.members, person: who)
        }
        r.banned = false
    }
    r.overturned = why.trim()
}

/// Bans a person from the workspace: they leave it and stay out until a moderator lifts the ban.
/// The workspace's own ban decides who may, its owners and moderators; Chat keeps the ban on the
/// moderators' Reports page as a decided report, which overturning lifts.
action ban(all: Conversations, who: Actor, reason: Text) -> Report (
    icon: .ban,
    label: "Ban",
    did: "banned a person",
) {
    require who != invoker "You can't ban yourself"
    require reason.trim() != "" "Say why"
    workspace.ban(workspace.members, person: who)
    all.reports.insert(
        Report(
            message: .none,
            person: who,
            said: "",
            reason: reason.trim(),
            decision: "Banned",
            banned: true,
        ),
    )
}

/// Settles a report by banning the person it is about from the workspace, as [`ban`] does.
action ban_reported(r: Report) (icon: .ban, label: "Ban", did: "banned a person") {
    require r.banned == false "They are banned already"
    require r.person != .none "The report names nobody to ban"
    require r.person != .some(invoker) "You can't ban yourself"
    if let who = r.person {
        workspace.ban(workspace.members, person: who)
    }
    r.decision = "Banned"
    r.overturned = ""
    r.banned = true
}

/// Whether a report waits on a moderator: nothing decided yet, or its decision overturned.
fn report_open(r: Report) -> Bool {
    r.decision == "" or r.overturned != ""
}

/// Whether `who` acts on what other people wrote here, as the workspace's moderators and its
/// owner do: the roles the workspace gave them, never something Chat keeps.
fn moderates(who: Actor) -> Bool {
    let role = who.role() ?? ""
    role == "moderator" or role == "owner"
}

/// Starts a thread from a message of a channel or a direct conversation, named after what the
/// message says.
action start_thread(c: TextChannel | Direct, m: Message, name: Text = m.body.excerpt) -> Thread (
    icon: .message_square,
    label: "Start thread",
) {
    let made = match c {
        ch: TextChannel => {
            let t = ch.threads.insert(Thread(name: name))
            m.thread = t
            t
        }
        d: Direct => {
            let t = d.threads.insert(Thread(name: name))
            m.thread = t
            t
        }
    }
    made
}

/// Chat's Unread place, as another app's page shows it: the conversations with something the
/// person looking has not read, in the sidebar's order — the channels outside a category, then
/// each category's own — each drawn by its type's row view. What a conversation holds unseen is
/// its messages, the items of its own list, so the place is the ones a transcript would stand
/// its "New messages" line in (`first_unseen`, as the channel and direct views read it);
/// `unseen` of the conversation itself is the changes to it, which saying where people talk is
/// not. A message that names them is one of those messages, so a mention counts here as any
/// unread message does. A category says nothing itself, so it never stands here; its channels
/// stand where they are.
view unread(all: Conversations) {
    let needing = all.channels.flat_map(
        x -> match x {
            c: Category => unread_in(c),
            ch: TextChannel => if first_unseen(ch) != .none { [ch] } else { [] },
            v: VoiceChannel => [],
            d: Direct => if d.people.contains(invoker) and first_unseen(d) != .none {
                [d]
            } else { [] },
        },
    )
    std.place(all, "Unread", needing) { x ->
        show(x, row: true)
    }
}

/// The conversations `c` holds that the person looking has not read. A voice channel holds no
/// messages, so it never stands here.
fn unread_in(c: Category) -> List<TextChannel | Direct> {
    c.channels.flat_map(
        x -> match x {
            ch: TextChannel => if first_unseen(ch) != .none { [ch] } else { [] },
            v: VoiceChannel => [],
        },
    )
}

/// The workspace's skills: how kinds of work are done here, each as a row that opens it, with
/// the entry that starts another at their foot.
view skills(all: Conversations) {
    state opened: Option<Skill> = .none
    on show(s: Skill): set(opened, s)
    ui.Docked {
        foot: {
            stack {
                std.add_entry("New skill", new_skill(all))
            }
        }
        if all.skills.is_empty() {
            ui.EmptyState(
                "No skills yet",
                icon: .sparkles,
                message: "Skills say how work is done in this workspace, and its agents read them.",
            )
        } else {
            std.list(all.skills, opened) { (s, at) ->
                ui.ListItem(item: s, presence: at) {
                    skill_row(s)
                }
            }
        }
    }
}

/// A skill: what it is called, the work it is for, how that work is done here and the form of
/// its result, each written in place.
view skill(s: Skill) {
    std.document(s) {
        ui.TextInput(bind: s.name, title: true, placeholder: "What it is called")
        stack(gap: .group) {
            stack(gap: .tight) {
                text("When it applies", role: .label)
                ui.TextInput(
                    bind: s.work,
                    placeholder: "The kind of work this is for, in a sentence or two",
                )
            }
            stack(gap: .tight) {
                text("How it is done here", role: .label)
                ui.TextInput(
                    bind: s.how,
                    lines: 6,
                    placeholder: "What to do, step by step where that helps",
                )
            }
            stack(gap: .tight) {
                text("The form of the result", role: .label)
                ui.TextInput(bind: s.form, placeholder: "Empty when it has no fixed form")
            }
            stack(gap: .tight) {
                text("The model", role: .label)
                ui.TextInput(
                    bind: s.model,
                    placeholder: "provider/model, empty for the person's choice",
                )
            }
            if not s.examples.is_empty() {
                stack(gap: .tight) {
                    text("Examples", role: .label)
                    for e in s.examples {
                        example_row(s, e)
                    }
                }
            }
        }
        std.after_body {
            std.history(s)
        }
    }
}

/// An example a skill keeps: the result, whether the person accepted it, and what they said
/// about it, with the way to take it out.
view example_row(s: Skill, e: Example) {
    row(gap: .related, align: .start) {
        ui.Icon(
            if e.accepted { .circle_check } else { .x },
            size: .small,
            tone: if e.accepted { .success } else { .danger },
            description: if e.accepted {
                "Accepted: more like this"
            } else { "Rejected: less like this" },
        )
        stack(fill: 1.0, gap: .none) {
            text(e.text, role: .body)
            if e.note != "" {
                text(e.note, role: .caption, secondary: true)
            }
        }
        ui.Button("Remove") {
            on press: drop_example(s, e)
        }
    }
}

/// The workspace's own assistant among its agents, as the workspace's record of them keeps it:
/// the first one a person made that no MCP client connects as, by the order it was made. The
/// assistant is no app's agent: it works for the workspace, and its conversation is the one
/// under Direct messages. None where the workspace keeps none.
fn assistant(roster: workspace.WorkspaceAgent) -> Option<workspace.Agent> {
    roster.agents
        .filter(a -> a.sub == .none)
        .filter(a -> a.mcp_client == .none)
        .filter(a -> a.state == .active)
        .sort_by(a -> a.created_at)
        .first()
}

/// The sub-agents `main` started, in the order the record lists them: each with the job it was
/// started for, the role it was given and how it stands.
fn subs_of(roster: workspace.WorkspaceAgent, main: workspace.Agent) -> List<workspace.Agent> {
    roster.agents.filter(
        a -> match a.sub {
            .none => false,
            .some(sub) => sub.started_by == main.id,
        },
    )
}

/// How a sub-agent stands, in the person's words.
fn standing(sub: workspace.Sub) -> Text {
    if sub.stands == .working {
        "Working"
    } else if sub.stands == .needs_person {
        "Needs you"
    } else if sub.line.trim() != "" { "Done: {sub.line}" } else { "Done" }
}

/// What an agent has spent, in millionths of the currency, as a person reads it: two hundredths
/// when it spent any.
fn money(micros: Int) -> Text {
    let cents = micros / 10000
    let units = cents / 100
    let rest = cents - units * 100
    if rest == 0 { "{units}" } else if rest < 10 { "{units}.0{rest}" } else { "{units}.{rest}" }
}

/// What the assistant spent against its day's limit, in the person's words: the record's last
/// counted day, which a new day starts afresh.
fn spend_words(a: workspace.Agent) -> Text {
    let spent = match a.spent {
        .none => "0",
        .some(s) => money(s.cost_micros),
    }
    match a.spend_limit_micros {
        .none => "{spent} with no day limit",
        .some(limit) => "{spent} of {money(limit)} a day",
    }
}

/// The assistant: how it stands, the model it plans with, chosen in Settings, and what it
/// spent. The record of the workspace's agents is read here as the session publishes it; the
/// window keeps no copy, and the model is changed where the workspace chooses its models.
view supervision(all: Conversations) {
    let roster = workspace.agents
    match assistant(roster) {
        .none => no_assistant(roster),
        .some(a) => stack(gap: .group) {
            ui.Text("Supervision", role: .title)
            ui.ListRow(title: "{a.name} is on", icon: .bot)
            stack(gap: .tight) {
                text("Model", role: .label)
                text(
                    match a.model {
                        .none => "Chosen in Settings",
                        .some(model) => model,
                    },
                )
                ui.Link(
                    "settings://agent_models",
                    label: "Change in Settings",
                    role: .caption,
                )
            }
            stack(gap: .tight) {
                text("Spent", role: .label)
                text(spend_words(a))
            }
        },
    }
}

/// The sub-agents the assistant started: the job each was given, the role it works as and how
/// it stands. It reads the workspace's record of its agents; a sub-agent cannot read the main
/// chat, and its conversation is its own.
view sub_agents(all: Conversations) {
    state opened: Option<workspace.Agent> = .none
    let roster = workspace.agents
    match assistant(roster) {
        .none => no_assistant(roster),
        .some(a) => {
            let subs = subs_of(roster, a)
            stack(gap: .group) {
                ui.Text("Sub-agents of {a.name}", role: .title)
                if subs.is_empty() {
                    ui.EmptyState("No sub-agents", icon: .users)
                } else {
                    for s in subs {
                        if let sub = s.sub {
                            ui.Card(item: s, title: sub.job) {
                                ui.ListRow(
                                    title: sub.job,
                                    secondary: sub.role,
                                    value: standing(sub),
                                    icon: .users,
                                )
                                on activate: set(opened, s)
                            }
                        }
                    }
                }
                if let one = opened {
                    sub_agent(one)
                }
                text(
                    "Each sub-agent has its conversation of its own under Direct messages; it cannot read the main chat.",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    }
}

/// One sub-agent: the job it was started for, the role it works as, how it stands, and where
/// its conversation is. It cannot read the main chat; what it says is its own conversation's,
/// under Direct messages.
view sub_agent(s: workspace.Agent) {
    if let sub = s.sub {
        stack(gap: .group) {
            ui.Text(sub.job, role: .title)
            if sub.role.trim() != "" {
                text(sub.role, role: .caption, secondary: true)
            }
            ui.ListRow(title: s.name, value: standing(sub), icon: .users)
            text(
                "It cannot read the main chat. Its conversation is its own, under Direct messages.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// Asks the assistant to start a sub-agent for `job`, in the direct conversation it answers in:
/// the record's own `start-sub-agent` is the assistant's to call there, and the sub-agent reports
/// in that conversation, where the person follows it. None where the workspace keeps no
/// conversation with the assistant yet.
action ask_for_sub_agent(
    all: Conversations,
    agent: workspace.Agent,
    job: Text = "",
) -> Option<Message> (icon: .pencil, label: "Start a sub-agent") {
    require job.trim() != "" "Say what the job is first"
    // An agent stands in the members by its id: the list names it so, and its display name is
    // the record's own, which the member list does not read.
    let who = workspace.members.people.find(p -> p.kind() == .agent and p.name() == agent.id)
    match who {
        .none => .none,
        .some(person) => .some(
            write_to(all, who: person, body: RichText("Start a sub-agent for this job: {job}")),
        ),
    }
}

/// Starting a sub-agent: the job in the person's words, handed to the assistant in the
/// conversation the person already follows it in. The words are the request an assistant's turn
/// reads, so a job started here and one typed into the conversation are the same.
view start_sub_agent(all: Conversations) {
    state job: Text = ""
    let roster = workspace.agents
    match assistant(roster) {
        .none => no_assistant(roster),
        .some(a) => stack(gap: .group) {
            ui.Text("Start a sub-agent", role: .title)
            text(
                "{a.name} starts it in your conversation and reports there, so you follow it where it works.",
                role: .caption,
                secondary: true,
            )
            ui.TextInput(
                bind: job,
                lines: 3,
                placeholder: "What the sub-agent should do",
            ) {
                on submit: ask_for_sub_agent(all, agent: a)
            }
            ui.Button("Start", role: .primary) {
                on press: ask_for_sub_agent(all, agent: a, job: job)
            } as start_sub_agent
        },
    }
}

/// What stands where the assistant would while the workspace keeps none: the one step that
/// makes it, through the workspace's own record of its agents.
// The entry fills the action's job with what is typed; typing enter starts it.
view no_assistant(record: workspace.WorkspaceAgent) {
    stack(gap: .group) {
        ui.EmptyState(
            "You have no assistant in this workspace",
            icon: .bot,
            message: "An assistant works for the workspace and answers in the direct conversation under Direct messages.",
        )
        ui.Button("Create assistant", role: .primary) {
            on press: workspace.create_agent(record, name: "Assistant")
        } as create_assistant
    }
}

/// The sidebar: channels outside any category first, then each category with its channels.
/// Choosing one shows it beside the sidebar.
/// TODO(language): a resource's own key for a navigation item; the category and name stand in.
view sidebar(all: Conversations) {
    state shown: Text = ""
    // What the person is writing and has not sent, read once: each conversation's row says the
    // first line of its draft, so a half-written message is found again rather than lost in a
    // composer the person has to remember to open.
    let writing = all.drafts.filter(d -> created(d)?.mine ?? false)
    // Choosing a person makes their conversation and shows it here: the window opens what the
    // action made as a conversation of its own.
    on show(d: Direct): open(d)
    ui.NavShell(content_kind: .page, sidebar_title: "Channels") {
        sidebar: {
            // A workspace with no channel and no one else to talk to says so where its places
            // would be: the screen a narrow window shows its places on never shows nothing. With
            // no places and nothing open the window says it once, in place of both columns, and
            // the way to make the first is the window bar's own, which every width shows.
            if all.channels.is_empty()
                and workspace.members.people.filter(m -> m != invoker).is_empty() {
                ui.EmptyState(
                    "No channels yet",
                    icon: .hash,
                    message: "Channels are where your workspace talks, each one a topic.",
                )
            } else {
                ui.NavList(selected: shown) {
                    for item in all.channels {
                        match item {
                            c: Category => {
                                // A group is its channels: one with none shows no header over
                                // nothing.
                                if not c.channels.is_empty() {
                                    ui.NavSection(c.name, collapsible: true) {
                                        // What is done to the group itself stays on the group:
                                        // its heading's menu, not a place in it.
                                        menu: {
                                            if moderates(invoker) {
                                                ui.MenuItem("Rename category", icon: .pencil) {
                                                    on choose: rename_category(c)
                                                }
                                                ui.MenuItem("Move up", icon: .arrow_up) {
                                                    on choose: move_category(
                                                        all,
                                                        c,
                                                        at: (all.channels.index_of(c) ?? 0) - 1,
                                                    )
                                                }
                                                ui.MenuItem("Move down", icon: .arrow_down) {
                                                    on choose: move_category(
                                                        all,
                                                        c,
                                                        at: (all.channels.index_of(c) ?? 0) + 1,
                                                    )
                                                }
                                                ui.MenuSeparator()
                                                ui.MenuItem(
                                                    "Delete category",
                                                    icon: .trash,
                                                    destructive: true,
                                                ) {
                                                    on choose: delete_category(all, c)
                                                }
                                            }
                                        }
                                        for x in c.channels {
                                            match x {
                                                ch: TextChannel => {
                                                    if not ch.archived {
                                                        ui.NavItem(
                                                            "{c.name}/{ch.name}",
                                                            ch.name,
                                                            icon: .hash,
                                                            item: ch,
                                                            secondary: draft_line(writing, ch),
                                                            mentions: mentions(ch),
                                                        ) {
                                                            menu: {
                                                                ui.MenuItem(
                                                                    "Rename channel",
                                                                    icon: .pencil,
                                                                ) {
                                                                    on choose: rename_channel(ch)
                                                                }
                                                                ui.MenuItem(
                                                                    "Set the topic",
                                                                    icon: .file_text,
                                                                ) {
                                                                    on choose: set_topic(ch)
                                                                }
                                                                ui.MenuItem(
                                                                    if ch.archived {
                                                                        "Restore channel"
                                                                    } else { "Archive channel" },
                                                                    icon: .archive,
                                                                ) {
                                                                    on choose: archive_channel(
                                                                        ch,
                                                                        archived: not ch.archived,
                                                                    )
                                                                }
                                                            }
                                                            on select: set(
                                                                    shown,
                                                                    "{c.name}/{ch.name}",
                                                                )
                                                                .then(
                                                                    open(ch),
                                                                )
                                                        }
                                                    }
                                                }
                                                v: VoiceChannel => {
                                                    // A voice channel, and under it who is in
                                                    // its call now.
                                                    let key = "{c.name}/voice/{v.name}"
                                                    ui.NavItem(
                                                        key,
                                                        v.name,
                                                        icon: .volume_2,
                                                        item: v,
                                                    ) {
                                                        on select: set(shown, key).then(open(v))
                                                    }
                                                    for m in call_members(v) {
                                                        ui.NavItem(
                                                            "{key}/{m.who.name()}",
                                                            m.who.name(),
                                                            person: m.who,
                                                            speaking: m.speaking,
                                                            secondary: call_state(m),
                                                            parent: key,
                                                        ) {
                                                            on select: set(shown, key).then(open(v))
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            ch: TextChannel => {
                                if not ch.archived {
                                    ui.NavItem(
                                        "/{ch.name}",
                                        ch.name,
                                        icon: .hash,
                                        item: ch,
                                        secondary: draft_line(writing, ch),
                                        mentions: mentions(ch),
                                    ) {
                                        menu: {
                                            ui.MenuItem("Rename channel", icon: .pencil) {
                                                on choose: rename_channel(ch)
                                            }
                                            ui.MenuItem("Set the topic", icon: .file_text) {
                                                on choose: set_topic(ch)
                                            }
                                            ui.MenuItem(
                                                if ch.archived {
                                                    "Restore channel"
                                                } else { "Archive channel" },
                                                icon: .archive,
                                            ) {
                                                on choose: archive_channel(
                                                    ch,
                                                    archived: not ch.archived,
                                                )
                                            }
                                        }
                                        on select: set(shown, "/{ch.name}").then(open(ch))
                                    }
                                }
                            }
                            v: VoiceChannel => {
                                let key = "/voice/{v.name}"
                                ui.NavItem(key, v.name, icon: .volume_2, item: v) {
                                    on select: set(shown, key).then(open(v))
                                }
                                for m in call_members(v) {
                                    ui.NavItem(
                                        "{key}/{m.who.name()}",
                                        m.who.name(),
                                        person: m.who,
                                        speaking: m.speaking,
                                        secondary: call_state(m),
                                        parent: key,
                                    ) {
                                        on select: set(shown, key).then(open(v))
                                    }
                                }
                            }
                            d: Direct => {
                                // A direct conversation reads as the other person's row: their
                                // avatar and presence. The conversation with yourself is your
                                // notes, and says so. One you are not in is not yours to see.
                                if d.people.contains(invoker) {
                                    if let other = other_in(d, invoker) {
                                        ui.NavItem(
                                            "/dm/{other.id()}",
                                            other.name(),
                                            person: other,
                                            item: d,
                                            secondary: draft_line(writing, d),
                                            mentions: mentions(d),
                                        ) {
                                            on select: set(shown, "/dm/{other.id()}").then(open(d))
                                        }
                                    } else {
                                        ui.NavItem(
                                            "/dm/self",
                                            "Notes to self",
                                            icon: .bookmark,
                                            item: d,
                                            secondary: draft_line(writing, d),
                                            mentions: mentions(d),
                                        ) {
                                            on select: set(shown, "/dm/self").then(open(d))
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // The workspace's assistant: the sub-agents it started and what it runs
                    // on. Its conversation is the one under Direct messages, where the
                    // workspace's agents stand as the members they are.
                    ui.NavSection("Assistant", collapsible: true) {
                        ui.NavItem("/assistant/sub-agents", "Sub-agents", icon: .users) {
                            on select: set(shown, "/assistant/sub-agents")
                                .then(open(all, view: sub_agents))
                        }
                        ui.NavItem("/assistant/start", "Start a sub-agent", icon: .pencil) {
                            on select: set(shown, "/assistant/start")
                                .then(
                                    open(all, view: start_sub_agent),
                                )
                        }
                        ui.NavItem("/assistant/supervision", "Supervision", icon: .gauge) {
                            on select: set(shown, "/assistant/supervision")
                                .then(open(all, view: supervision))
                        }
                    }
                    // Everyone in the workspace, on a page of their own: the sidebar lists the
                    // conversations a person has, never every member, so a workspace with a
                    // thousand of them keeps its channels in sight.
                    if not workspace.members.people.is_empty() {
                        ui.NavSection("Direct messages") {
                            ui.NavItem("/people", "Everyone", icon: .users) {
                                on select: set(shown, "/people").then(open(all, view: people))
                            }
                        }
                    }
                    // How work is done here, which the workspace's agents read before they
                    // start: a window of its own.
                    ui.NavSection("Workspace") {
                        ui.NavItem("/skills", "Workspace skills", icon: .sparkles) {
                            on select: set(shown, "/skills").then(open(all, view: skills))
                        }
                        // What people reported, for those who decide it: every report of the
                        // workspace on one page, the open ones to decide.
                        if moderates(invoker) {
                            ui.NavItem("/reports", "Reports", icon: .flag) {
                                on select: set(shown, "/reports").then(open(all, view: reports))
                            }
                        }
                    }
                    // Who is looking, at the foot of the places: a person without a name reads
                    // as you, never as the key the workspace knows them by. Choosing it opens
                    // the conversation with yourself.
                    ui.NavItem(
                        "/me",
                        if invoker.name().trim() == "" { "You" } else { invoker.name() },
                        person: invoker,
                    ) {
                        on select: set(shown, "/me").then(message_person(all, invoker))
                    }
                }
            }
        }
        content: {
            // What stands where a conversation would, while the places have one to choose. With
            // no places at all the window says what is missing once, in place of both columns.
            ui.EmptyState("No channel open", icon: .hash)
        }
    }
}

/// A text channel: its messages, and a composer that can answer one of them.
view channel(c: TextChannel) {
    state replying: Option<Message> = .none
    state editing: Option<Message> = .none
    state revised: RichText = RichText("")
    let all = root Conversations
    std.transcript_by(c, c.messages, author: m -> writer_of(m), unread: first_unseen(c)) { m ->
        channel_message(c, m, c.messages, replying, editing, revised)
        composer: {
            typing_here(c)
            // What can be done to the channel itself: its name, what it is for, the page its
            // pins stand on, and setting it aside. A press that needs words opens the form that
            // asks for them. What the channel is about stands beside them, so everyone reading
            // it sees the topic without opening the menu that set it.
            row(distribute: .end, gap: .related, align: .center) {
                if c.topic != "" {
                    text(c.topic, role: .caption, secondary: true, fill: 1.0, truncate)
                }
                if c.messages.any(m -> m.pinned) {
                    ui.Button("Pinned messages", icon: .pin, size: .small, role: .plain) {
                        on press: open(c, view: pins)
                    }
                }
                // The same commands the channel's row in the sidebar offers, so either way
                // reaches one change, and, for a moderator, where it stands among the places
                // of the sidebar and taking it away.
                ui.MenuButton("Channel", icon: .more_horizontal, icon_only: true) {
                    ui.MenuItem("Rename channel", icon: .pencil) {
                        on choose: rename_channel(c)
                    }
                    ui.MenuItem("Set the topic", icon: .file_text) {
                        on choose: set_topic(c)
                    }
                    ui.MenuItem(
                        if c.archived { "Restore channel" } else { "Archive channel" },
                        icon: .archive,
                    ) {
                        on choose: archive_channel(c, archived: not c.archived)
                    }
                    if c.messages.any(m -> m.pinned) {
                        ui.MenuItem("Pinned messages", icon: .pin) {
                            on choose: open(c, view: pins)
                        }
                    }
                    if moderates(invoker) {
                        ui.MenuSeparator
                        ui.MenuItem("Move up", icon: .arrow_up) {
                            on choose: move_channel(all, c, at: (all.channels.index_of(c) ?? 0) - 1)
                        }
                        ui.MenuItem("Move down", icon: .arrow_down) {
                            on choose: move_channel(all, c, at: (all.channels.index_of(c) ?? 0) + 1)
                        }
                        ui.MenuItem("Delete channel", icon: .trash, destructive: true) {
                            on choose: delete_channel(all, c)
                        }
                    }
                }
            }
            if let d = all.drafts.find(x -> x.conversation == c and (created(x)?.mine ?? false)) {
                composer(c, c.messages, d, replying, "Message #{c.name}")
            }
        }
    }
}

/// A direct conversation: the same messages and composer as a channel, with the person's name
/// over it.
view direct(d: Direct) {
    state replying: Option<Message> = .none
    state editing: Option<Message> = .none
    state revised: RichText = RichText("")
    let all = root Conversations
    std.transcript_by(d, d.messages, author: m -> writer_of(m), unread: first_unseen(d)) { m ->
        direct_message(d, m, d.messages, replying, editing, revised)
        composer: {
            direct_typing_here(d)
            if d.people.any(p -> p.kind() == .agent) {
                row(distribute: .end) {
                    model_switcher(d.messages)
                }
            }
            if let writing = all.drafts.find(
                    x -> x.conversation == d and (created(x)?.mine ?? false),
                ) {
                composer(
                    d,
                    d.messages,
                    writing,
                    replying,
                    if let other = other_in(d, invoker) {
                        "Message {other.name()}"
                    } else { "Message {d.name}" },
                    agent: d.people.any(p -> p.kind() == .agent),
                )
            }
        }
    }
}

/// A channel as another app's page summarizes it: its name, and what it is about.
view channel_summary(c: TextChannel) {
    row(c.name, secondary: c.topic, icon: .hash)
}

/// A channel's pinned messages, as a page of their own: each as it was written, with what came
/// with it, so what the workspace keeps at hand is read without scrolling back to it. Choosing
/// one opens the conversation it stands in.
view pins(c: TextChannel) {
    let pinned = c.messages.filter(m -> m.pinned)
    std.page(c) {
        content: {
            if pinned.is_empty() {
                ui.EmptyState(
                    "Nothing pinned yet",
                    icon: .pin,
                    message: "Pin a message from its menu to keep it here for everyone.",
                )
            } else {
                for m in pinned {
                    ui.Card(item: m, title: meta(m).title) {
                        stack(gap: .tight) {
                            for f in m.attachments {
                                ui.Link(f.blob, label: f.name)
                            }
                            said(m)
                        }
                        on activate: open(c)
                    }
                }
            }
        }
    }
}

/// Everyone in the workspace, each with their face, and the way to the conversation with them: a
/// workspace's people are its membership's, never a list Chat keeps, and one with many members
/// reaches any of them here rather than from a sidebar that would hold them all.
view people(all: Conversations) {
    ui.NavShell(content_kind: .page, title: "People") {
        content: {
            ui.Scroll {
                page {
                    for p in workspace.members.people {
                        row(gap: .related, align: .center) {
                            avatar(p)
                            stack(fill: 1.0, gap: .none) {
                                text(p.name(), role: .label)
                                if p.kind() == .agent {
                                    text("Agent", role: .caption, secondary: true)
                                }
                            }
                            if p != invoker {
                                ui.Button("Message", icon: .message_circle, size: .small) {
                                    on press: message_person(all, p)
                                }
                                button(report_person(all, p), icon_only: true)
                                if moderates(invoker) {
                                    button(ban(all, p), icon_only: true)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Every report of the workspace, for its moderators: the people reported, then each channel's,
/// thread's and direct conversation's, each with who it is about, what they wrote, why it was
/// reported, and what was decided. An open one is decided here, a decided one overturned.
view reports(all: Conversations) {
    ui.NavShell(content_kind: .page, title: "Reports") {
        content: {
            ui.Scroll {
                page {
                    if not moderates(invoker) {
                        ui.EmptyState("Only moderators read reports", icon: .shield)
                    } else {
                        for r in all.reports {
                            report_row(r, "")
                        }
                        for x in all.channels {
                            match x {
                                c: Category => {
                                    for y in c.channels {
                                        match y {
                                            ch: TextChannel => channel_reports(ch),
                                            v: VoiceChannel => {}
                                        }
                                    }
                                }
                                ch: TextChannel => channel_reports(ch),
                                v: VoiceChannel => {}
                                d: Direct => {
                                    for r in d.reports {
                                        report_row(r, "")
                                    }
                                    for t in d.threads {
                                        for r in t.reports {
                                            report_row(r, t.name)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The reports of a channel and its threads, each under the place it was filed in.
view channel_reports(ch: TextChannel) {
    for r in ch.reports {
        report_row(r, "#{ch.name}")
    }
    for t in ch.threads {
        for r in t.reports {
            report_row(r, t.name)
        }
    }
}

/// One report as a moderator reads it: who it is about and where, what they wrote and why it
/// was reported, and what was decided or why that was overturned, with the one thing to do next.
view report_row(r: Report, place: Text) {
    row(gap: .related, align: .start) {
        if let p = r.person {
            avatar(p)
        }
        stack(fill: 1.0, gap: .tight) {
            if let p = r.person {
                text(if place == "" { p.name() } else { "{p.name()} in {place}" }, role: .label)
            }
            if r.said != "" {
                text(r.said, secondary: true)
            }
            if r.reason != "" {
                text("Reported: {r.reason}", role: .caption)
            }
            if r.overturned != "" {
                text("Overturned: {r.overturned}", role: .caption, secondary: true)
            } else if r.decision != "" {
                text("Decided: {r.decision}", role: .caption, secondary: true)
            }
        }
        if report_open(r) {
            button(decide(r))
            if not r.banned and r.person != .none {
                button(ban_reported(r))
            }
        } else {
            button(overturn(r))
        }
    }
}

/// A voice channel: the system's call, with this channel as its place. Joining plays each person
/// at the level this device keeps for them, and beside each person is how loud they play here.
view voice(v: VoiceChannel) {
    let levels = hearing.people.map(h -> set_call_volume(v, h.who, played(h).to_float()))
    std.call(v, join: .some(join_call(v).then(levels.many()))) { m ->
        hearing_menu(v, m.who)
    }
}

/// How this device plays `who` in the call held on `v`, as a menu named by it: silenced for the
/// person alone, or a level from nothing to twice as loud. A choice is kept for the device and
/// played at once.
view hearing_menu(v: VoiceChannel, who: Actor) {
    let heard = heard_of(who)
    stack(min: 8rem, max: 14rem) {
        ui.MenuButton(
            if heard.silenced { "Muted for you" } else { "Volume {heard.percent}%" },
            icon: if played(heard) == 0 { .volume_x } else { .volume_2 },
        ) {
            ui.MenuItem("Mute for me", icon: .volume_x, checked: heard.silenced) {
                on choose: silence(hearing, who, on: not heard.silenced)
                    .then(
                        set_call_volume(
                            v,
                            who,
                            if heard.silenced { heard.percent.to_float() } else { 0.0 },
                        ),
                    )
            }
            ui.MenuSeparator()
            for percent in [25, 50, 75, 100, 150, 200] {
                ui.MenuItem(
                    "Volume {percent}%",
                    checked: not heard.silenced and heard.percent == percent,
                ) {
                    on choose: set_hearing(hearing, who, percent)
                        .then(set_call_volume(v, who, percent.to_float()))
                }
            }
        }
    }
}

/// How this device plays `who` in a call, as it keeps it: as they arrive until the person set
/// otherwise here.
fn heard_of(who: Actor) -> Heard {
    hearing.people.find(h -> h.who == who) ?? Heard(who: who)
}

/// How loud this device plays a person, in percent of as they arrive: nothing while silenced.
fn played(h: Heard) -> Int {
    if h.silenced { 0 } else { h.percent }
}

/// Plays `who` in calls on this device at `percent` of as they arrive, and hears them again if
/// they were silenced. Only this device plays them so; nobody else hears a difference.
action set_hearing(h: Hearing, who: Actor, percent: Int) (
    icon: .volume_2,
    label: "Set how loud someone plays",
    did: "set how loud someone plays",
) {
    require percent >= 0 and percent <= 200 "A level is between 0 and 200 percent"
    if let had = h.people.find(x -> x.who == who) {
        had.percent = percent
        had.silenced = false
    } else {
        h.people.insert(Heard(who: who, percent: percent))
    }
}

/// Silences `who` in calls on this device, or hears them again at the level they had. Only
/// this person stops hearing them; everyone else in the call still does.
action silence(h: Hearing, who: Actor, on: Bool = true) (
    icon: .volume_x,
    label: "Mute someone for me",
    did: "muted someone for themselves",
) {
    if let had = h.people.find(x -> x.who == who) {
        had.silenced = on
    } else if on {
        h.people.insert(Heard(who: who, silenced: true))
    }
}

/// A voice channel as another app's page summarizes it: its name, and who is in its call.
view voice_summary(v: VoiceChannel) {
    let people = call_members(v)
    row(
        v.name,
        secondary: if people.is_empty() {
            ""
        } else if people.len() == 1 { "1 in the call" } else { "{people.len()} in the call" },
        icon: .volume_2,
    )
}

/// A direct conversation as another app's page summarizes it: whose it is, the last thing said in
/// it and how many of its messages the person looking has not read. The conversation with
/// yourself is your notes, as the sidebar names it.
view direct_summary(d: Direct) {
    let last = d.messages.last()
    let unread = unread_of(d)
    row(
        if let other = other_in(d, invoker) { other.name() } else { "Notes to self" },
        secondary: if let m = last { m.body.excerpt } else { "" },
        badge: if unread > 0 { "{unread}" } else { "" },
        icon: .message_circle,
    )
}

/// A category as another app's page summarizes it: its name and how many channels it holds.
view category_summary(c: Category) {
    row(
        c.name,
        secondary: if c.channels.len() == 1 { "1 channel" } else { "{c.channels.len()} channels" },
        icon: .folder,
    )
}

/// A skill as another app's page summarizes it: what it is called, and the work it is for.
view skill_row(s: Skill) {
    ui.ListRow(title: meta(s).title, secondary: s.work, icon: .sparkles)
}

/// A message of a direct conversation, with what can be done to it on hover.
view direct_message(
    d: Direct,
    m: Message,
    shown: List<Message>,
    replying: State<Option<Message>>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    message(m, shown, editing, revised)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
        if m.thread == .none {
            button(start_thread(d, m), icon_only: true)
        }
        edit_button(m, editing, revised)
        if answer_ended(m) {
            button(continue_answer(d, m), icon_only: true)
        }
        button(copy_text(m), icon_only: true)
        button(pin(m), icon_only: true)
        button(report(d, m), icon_only: true)
        ui.EmojiPicker {
            on pick(e): react(m, e)
        }
    }
}

/// A message of a channel, with what can be done to it on hover.
view channel_message(
    c: TextChannel,
    m: Message,
    shown: List<Message>,
    replying: State<Option<Message>>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    message(m, shown, editing, revised)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
        if m.thread == .none {
            button(start_thread(c, m), icon_only: true)
        }
        edit_button(m, editing, revised)
        if answer_ended(m) {
            button(continue_answer(c, m), icon_only: true)
        }
        button(copy_text(m), icon_only: true)
        button(pin(m), icon_only: true)
        button(report(c, m), icon_only: true)
        ui.EmojiPicker {
            on pick(e): react(m, e)
        }
    }
}

/// A thread: its messages and a composer, the same way.
view thread(t: Thread) {
    state replying: Option<Message> = .none
    state editing: Option<Message> = .none
    state revised: RichText = RichText("")
    let all = root Conversations
    std.transcript_by(t, t.messages, author: m -> writer_of(m)) { m ->
        thread_message(t, m, t.messages, replying, editing, revised)
        composer: {
            thread_typing_here(t)
            if let d = all.drafts.find(x -> x.conversation == t and (created(x)?.mine ?? false)) {
                composer(t, t.messages, d, replying, "Message {t.name}")
            }
        }
    }
}

/// A message of a thread, with what can be done to it on hover.
view thread_message(
    t: Thread,
    m: Message,
    shown: List<Message>,
    replying: State<Option<Message>>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    message(m, shown, editing, revised)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
        edit_button(m, editing, revised)
        if answer_ended(m) {
            button(continue_answer(t, m), icon_only: true)
        }
        button(copy_text(m), icon_only: true)
        button(pin(m), icon_only: true)
        button(report(t, m), icon_only: true)
        ui.EmojiPicker {
            on pick(e): react(m, e)
        }
    }
}

/// The foot of a conversation: the message being answered, over the input and its send button.
/// `said` is what the conversation holds, whose speakers its `@` drawer offers first.
///
/// In a conversation an agent answers in, a line that reads as a tilolang call
/// (`tasks.add("Buy milk")`) runs as the person's own step rather than being said: every call it
/// makes is theirs, what it answers shows above the composer, and what it changed is in the
/// records it changed. `agent` says the conversation is one with an agent before it answered
/// anything.
view composer(
    to: TextChannel | Thread | Direct,
    said: List<Message>,
    draft: Draft,
    replying: State<Option<Message>>,
    placeholder: Text,
    agent: Bool = false,
) {
    for l in lines_run(to) {
        line_run(l)
    }
    if let r = replying {
        std.reply_banner(r, cancel: set(replying, .none))
    }
    // What the message will carry, each file with the way to leave it behind.
    if not draft.files.is_empty() {
        row(gap: .tight, wrap: true) {
            for f in draft.files {
                ui.Chip(f.name, icon: .paperclip) {
                    on remove: detach(draft, f)
                }
            }
        }
    }
    // While a turn works here, the button that would send stops it instead, until the person
    // types something to say.
    let stop: Option<Action> = if let t = working_turn(said) {
        .some(host.cancel(t.record))
    } else { .none }
    share_menu(to)
    std.composer_draft(
        draft,
        placeholder,
        if (agent or said.any(m -> m.turn != "")) and reads_as_call(draft.body.text) {
            run_line(to, draft.body.text)
        } else {
            send(to, reply_to: replying, draft: .some(draft)).then(set(replying, .none))
        },
        speakers: speakers(said),
        attach: .some(file.choose(attach(draft))),
        stop: stop,
    )
}

/// Whether `text` reads as a tilolang call the person runs rather than says: one line, a dotted
/// name with no space in it before its first parenthesis, and a closing one at its end.
fn reads_as_call(text: Text) -> Bool {
    let line = text.trim()
    if line.contains("\n") or not line.ends_with(")") {
        return false
    }
    if let name = line.split("(").first() {
        name != "" and name.contains(".") and not name.contains(" ")
    } else { false }
}

/// The model the workspace's agents answer with, as a conversation with one offers it: every
/// model of every provider the person set up, the one in use marked, and what the answers here
/// have cost so far. Choosing one is the same change Settings' Agent models pane makes, and the
/// choice before it can be taken back.
view model_switcher(said: List<Message>) {
    let m = settings.agent_models
    ui.PopoverButton(
        if m.in_use.model_label != "" { m.in_use.model_label } else { std.say("Model") },
        icon: .sparkles,
        side: .above,
        trailing: true,
    ) {
        stack(gap: .tight) {
            ui.Text(std.say("Agents answer with"), role: .caption, secondary: true)
            for provider in m.providers.filter(p -> p.enabled) {
                ui.Text(provider.label, role: .label)
                for model in provider.models {
                    ui.ListRow(
                        model.label,
                        secondary: model.id,
                        selected: provider.id == m.in_use.provider and model.id == m.in_use.model,
                        icon: .sparkles,
                    ) {
                        on activate: settings.use_model(m, provider.id, model.id)
                    }
                }
            }
            if m.in_use.model != "" {
                let cost = answers_cost(said).round(4)
                ui.Text(std.say("Answers so far: ${cost}"), role: .caption, secondary: true)
                ui.Button(std.say("Undo the choice"), size: .small) {
                    on press: settings.undo_model(settings.agent_models)
                }
            }
        }
    }
}

/// A line the person ran from the composer, and what it answered: Running while it runs.
view line_run(l: host.LineRun) {
    stack(gap: .none) {
        ui.Text(l.line, role: .label, truncate)
        if l.running {
            ui.Text("Running…", role: .caption, secondary: true)
        } else if l.said != "" {
            ui.Text(l.said, role: .caption, secondary: not l.failed)
        }
    }
}

/// Who is typing here, from the system's presence: the lenses on `of` whose person is
/// composing a message in it (`l.composing`), never the person looking. A conversation with
/// no one typing says nothing, so the composer does not move.
view typing_here(of: TextChannel) {
    let typists = lenses(of).filter(l -> l.composing and not l.mine)
    if let first = typists.first() {
        let who = first.who.name()
        ui.Text(
            if typists.len() == 1 {
                "{who} is typing…"
            } else { "{who} and others are typing…" },
            role: .caption,
            secondary: true,
        )
    }
}

/// [`typing_here`] for a direct conversation.
view direct_typing_here(of: Direct) {
    let typists = lenses(of).filter(l -> l.composing and not l.mine)
    if let first = typists.first() {
        let who = first.who.name()
        ui.Text(
            if typists.len() == 1 {
                "{who} is typing…"
            } else { "{who} and others are typing…" },
            role: .caption,
            secondary: true,
        )
    }
}

/// [`typing_here`] for a thread.
view thread_typing_here(of: Thread) {
    let typists = lenses(of).filter(l -> l.composing and not l.mine)
    if let first = typists.first() {
        let who = first.who.name()
        ui.Text(
            if typists.len() == 1 {
                "{who} is typing…"
            } else { "{who} and others are typing…" },
            role: .caption,
            secondary: true,
        )
    }
}

/// Edit, for whoever may edit the message (its author): puts the editor in its place.
view edit_button(m: Message, editing: State<Option<Message>>, revised: State<RichText>) {
    if created(m)?.who == invoker and not m.body.is_empty() {
        ui.Button("Edit", icon: .pencil, icon_only: true) {
            on press: set(revised, m.body).then(set(editing, m))
        }
    }
}

/// Who wrote a message here: the person whose press or agent whose turn wrote it. None for one
/// carried in from another service: it shows the account that wrote it there (`persona`), not
/// whoever carried it, so it opens no run of the carrier's.
fn writer_of(m: Message) -> Option<Actor> {
    if m.by != "" or m.outside != .none { .none } else { created(m)?.who }
}

/// Who wrote a message where it came from, when it was carried here from another service, in
/// the place its author's face would stand: the account's own face and name, in the colour that
/// name draws, with where it wrote.
view persona(m: Message) {
    row(gap: .tight, align: .center) {
        ui.Avatar(name: m.by)
        ui.Text(m.by, role: .label)
        ui.Text("in {m.written_in}", role: .caption, secondary: true)
    }
}

/// What one message says: the message it answers, its text (or, while it is being edited, the
/// editor in its place), and the thread it started. `shown` is the transcript's own messages:
/// the list draws them where they stand, so what a message was made from reads once, as the
/// reference above it, and is left out of the links under it.
view message(
    m: Message,
    shown: List<Message>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    if let r = m.reply_to {
        std.quote(r)
    }
    std.origin(m)
    if let o = m.outside {
        row(gap: .tight, align: .center) {
            // A linked account is drawn as the member it belongs to, in their colour, wearing
            // the account's own picture when it has one.
            if let p = o.member {
                ui.Avatar(
                    who: p,
                    name: o.name,
                    picture: if o.avatar != "" { o.avatar } else { .none },
                )
            } else if o.avatar != "" {
                ui.Avatar(name: o.name, picture: o.avatar)
            }
            ui.Text("{o.name} wrote this in {o.service}", role: .caption, secondary: true)
        }
    } else if m.by != "" {
        persona(m)
    }
    if m.pinned {
        ui.Text("Pinned", role: .caption, secondary: true)
    }
    reasoning(m)
    if let e = editing {
        if e == m {
            revision(m, shown, editing, revised)
        } else {
            said(m)
        }
    } else {
        said(m)
    }
    std.reach_note(m)
    if m.turn != "" {
        turn_steps(m)
    }
    if not m.attachments.is_empty() {
        stack(gap: .tight) {
            row(gap: .tight, wrap: true) {
                for f in m.attachments {
                    if let _ = refusal_of(f) {
                        ui.Chip(f.name, icon: .triangle_alert, tone: .warning) {
                            on press: open_attachment(f)
                        }
                    } else {
                        ui.Chip(f.name, icon: .paperclip) {
                            on press: open_attachment(f)
                        }
                    }
                }
            }
            for f in m.attachments {
                if let r = refusal_of(f) {
                    ui.StatusLine(
                        if r.limit == "file" {
                            "The host didn't keep {f.name}: it is larger than the {std.mib(r.allowed)} it keeps of one file."
                        } else if r.limit == "member" {
                            "The host didn't keep {f.name}: this person's files take more than their {std.mib(r.allowed)}."
                        } else if r.limit == "hourly" {
                            "The host didn't keep {f.name}: this person posted more than {std.mib(r.allowed)} this hour."
                        } else if r.limit == "workspace" {
                            "The host didn't keep {f.name}: the workspace keeps more than {std.mib(r.allowed)} on it."
                        } else {
                            "The host didn't keep {f.name}: it is more than the {std.mib(r.allowed)} this host keeps."
                        },
                        status: .warning,
                    )
                    ui.Text(
                        if r.raise == "host" {
                            "The host's person can raise it in the server's settings."
                        } else {
                            "The workspace's owners can raise it in Settings, under Content limits."
                        },
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
    }
    if not m.outside_emoji.is_empty() {
        row(gap: .tight, align: .center) {
            for e in m.outside_emoji {
                ui.Avatar(name: ":{e.name}:", picture: e.picture, square: true)
            }
        }
    }
    if m.reacted != "" {
        ui.Text(m.reacted, role: .caption, secondary: true)
    }
    if not m.reactions.keys().is_empty() or not m.relayed_reactions.is_empty() {
        ui.Reactions(
            reactions: m.reactions,
            relayed: m.relayed_reactions.map(r -> "{r.emoji}\t{r.by}"),
        ) {
            on toggle(e): react(m, e)
        }
    }
    if let t = m.thread {
        std.thread_chip(t, count: t.messages.len())
    }
    let listed = linked(m).filter(l -> shown.any(x -> l.of == x)).map(l -> l.of)
    std.linked(m, compact: true, without: listed)
}

/// The agent's turn a reply is, as the invocation that wrote it stands on the system's dispatch:
/// each step it took, what it asks the person, and what its model calls cost. Stop while it runs,
/// which cancels the invocation; Retry once it ended, which runs it again into this message.
view turn_steps(m: Message) {
    if let t = invoked(m.turn) {
        // What the turn is doing stands on one line, and what it did folds under it: an answer
        // read for its words is not run through a dozen steps, and one that is working shows
        // them as they come. Stop and Retry stay on the line, where the turn stands.
        ui.DisclosureRow(
            turn_said(t),
            dense: true,
            expanded: if t.state == "running" or t.state == "waiting" {
                .some(true)
            } else { .none },
        ) {
            trailing: {
                if t.state == "running" or t.state == "waiting" {
                    button(host.cancel(t.record), icon_only: true)
                } else if t.state == "ended" or t.state == "failed" or t.state == "cancelled" {
                    button(host.retry(t.record), icon_only: true)
                }
            }
            for c in t.calls {
                if let s = invoked(c) {
                    turn_step(s)
                }
            }
            if t.failure != "" {
                ui.Text(t.failure, role: .caption)
            }
        }
    }
}

/// How a turn stands in a line: what it is doing, the model it asked and what it cost.
fn turn_said(t: host.Invoked) -> Text {
    let state = if t.state == "running" {
        "Working…"
    } else if t.state == "waiting" {
        "Waiting for your answer"
    } else if t.state == "failed" {
        "Failed"
    } else if t.state == "cancelled" { "Stopped" } else { "Done" }
    let model = if t.model != "" { " · {t.provider}/{t.model}" } else { "" }
    let cost = if t.cost > 0.0 { " · ${t.cost.round(4)}" } else { "" }
    "{state}{model}{cost}"
}

/// One step of an agent's turn: what it did, and, for one that asks the person, Allow, Always
/// and Deny, answered through the system's ask-answer action on its record.
view turn_step(s: host.Invoked) {
    stack(gap: .none) {
        row(gap: .tight, align: .center) {
            ui.Text(
                if s.title != "" {
                    s.title
                } else if s.summary != "" { s.summary } else { s.action },
                role: .caption,
                secondary: s.state != "waiting",
                fill: 1.0,
                truncate,
            )
            if s.state == "running" {
                ui.Text("Working…", role: .caption, secondary: true)
            } else if s.state == "failed" {
                ui.Text("Failed", role: .caption)
            }
        }
        if s.state == "waiting" {
            row(gap: .tight) {
                ui.Button("Allow", role: .primary) {
                    on press: host.resolve(s.record, "approve")
                }
                ui.Button("Always") {
                    on press: host.resolve(s.record, "always")
                }
                ui.Button("Deny", role: .destructive) {
                    on press: host.resolve(s.record, "decline")
                }
            }
        }
    }
}

/// A turn the person asked on the canvas, standing where they asked: the agent's answer, under
/// the person's message it answers as a quiet line, with the turn's steps and controls. The
/// conversation it is in opens from its place's handle.
view turn(m: Message) {
    stack(gap: .tight) {
        if let r = m.reply_to {
            text(r.body.excerpt, secondary: true)
        }
        reasoning(m)
        said(m)
        if m.turn != "" {
            turn_steps(m)
        }
    }
}

/// What the agent's model reasoned toward an answer, folded above it: one quiet line that opens
/// the reasoning, so the answer reads first and how it was reached is a press away. Nothing for a
/// message that carries none.
view reasoning(m: Message) {
    if m.reasoning != "" {
        ui.DisclosureRow(std.say("Reasoning"), dense: true) {
            ui.Text(m.reasoning, role: .caption, secondary: true)
        }
    }
}

/// What a message says, and that it was edited when its history holds an edit.
view said(m: Message) {
    if let s = m.live_share {
        live_share(m, s)
    } else if not m.body.is_empty() {
        rich_text(m.body)
    }
    for p in m.parts {
        text(p.body)
    }
    if m.sent_view != "" {
        // The view the message sends, drawn where the message stands, as its author wrote it.
        if let made = created(m) {
            embed(m.sent_view, author: made.who, invocation: .none)
        }
        ui.Text("A view, to add to your own views", role: .caption, secondary: true)
    }
    if history(m).any(v -> v.is(edit)) {
        edit_history(m)
    }
}

/// A window shared live into the conversation: the window itself, as the shell pictures it live,
/// under its title, its app and what the others may do there. The sharer switches that and stops
/// the share here; a share that ended says so in the window's place. The window stands as this
/// device's shell holds it: the sharer's own window, or the place another member's share shows in.
view live_share(m: Message, s: LiveShare) {
    let mine = created(m)?.who == invoker
    let held = device.shares.sharing.find(x -> x.tab == s.window)
    let watched = device.shares.watching.find(x -> x.tab == s.window)
    // The sharer's own device knows the share ended when the shell no longer shares the window,
    // however it was stopped: from here, from the window's bar, or by closing it.
    let here = device.windows.all.any(w -> w.key == s.window and not w.elsewhere)
    let ended = s.ended or here and held == .none
    let can_edit = if let h = held { h.input_allowed } else { s.can_edit }
    ui.Card {
        stack(gap: .tight) {
            row(gap: .tight, align: .center) {
                ui.Icon(.screen_share, secondary: true)
                ui.Text(
                    if s.title != "" { s.title } else { s.app_name },
                    role: .label,
                    lines: 1,
                    truncate,
                    fill,
                )
                if ended {
                    ui.Badge("No longer shared", tone: .neutral)
                } else {
                    ui.Badge("Live", tone: .info)
                }
            }
            if ended {
                ui.Text(
                    if s.app_name != "" {
                        "{s.app_name} is no longer shared"
                    } else { "No longer shared" },
                    role: .caption,
                    secondary: true,
                )
            } else {
                layers(aspect: 1.6) {
                    if let h = held {
                        ui.WindowPreview(s.window)
                    } else if let w = watched {
                        ui.WindowPreview(w.shown_in)
                    }
                    if held == .none and watched == .none {
                        row(fill, align: .center, distribute: .center) {
                            ui.Button("Show it live", icon: .eye) {
                                on press: device.watch(
                                    device.shares,
                                    tab: s.window,
                                    workspace: s.workspace,
                                )
                            }
                        }
                    }
                }
                row(gap: .tight, align: .center) {
                    ui.Text(
                        if s.app_name != "" {
                            "{s.app_name} · {if can_edit { "Can edit" } else { "View only" }}"
                        } else if can_edit { "Can edit" } else { "View only" },
                        role: .caption,
                        secondary: true,
                        fill,
                    )
                    if mine {
                        ui.ToggleGroup(caption: "What others may do") {
                            ui.Segment("View only", chosen: not can_edit) {
                                on press: set_share_access(m, can_edit: false)
                            }
                            ui.Segment("Can edit", chosen: can_edit) {
                                on press: set_share_access(m, can_edit: true)
                            }
                        }
                        ui.Button("Stop sharing", icon: .eye_off, role: .destructive) {
                            on press: stop_sharing(m)
                        }
                    }
                }
            }
        }
    }
}

/// The windows of this device a person may share into `to`, each through the shell's own share,
/// the one they used last first.
view share_menu(to: TextChannel | Thread | Direct) {
    let mine = device.windows.all.filter(w -> not w.elsewhere).sort_by(w -> w.recency)
    ui.MenuButton("Share a window", icon: .screen_share, icon_only: true) {
        if mine.is_empty() {
            ui.MenuItem("Open the window you want to share, then come back here.", disabled: true)
        }
        for w in mine {
            ui.MenuItem(std.say("Share {w.title}"), icon: .app_window) {
                on choose: share_window(to, window: w.key)
            }
        }
    }
}

/// What a message said before it was edited: one quiet line, which opens each version it had
/// with who changed it and when, so what it said before is read without leaving the
/// conversation. The words above are the message as it stands.
view edit_history(m: Message) {
    ui.DisclosureRow(std.say("Edited"), dense: true) {
        for v in history(m).filter(v -> v.is(edit)) {
            for c in v.changes.filter(c -> c.field == "body") {
                stack(gap: .tight) {
                    ui.Text("{v.actor} · {v.at}", role: .caption, secondary: true)
                    c.before
                }
            }
        }
    }
}

/// A message being edited in place: its text in an editor, saved or left as it was.
view revision(
    m: Message,
    said: List<Message>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    rich_input(bind: revised, placeholder: "Edit message", speakers: speakers(said)) {
        on submit: edit(m, revised).then(set(editing, .none))
    }
    row {
        ui.Button("Cancel") {
            on press: set(editing, .none)
        }
        ui.Button("Save", role: .primary) {
            on press: edit(m, revised).then(set(editing, .none))
        }
    }
}

/// A team's chat two mornings in: categories of channels, a busy channel with replies and a
/// thread, its last messages the next day.
scenario "a team's channels" -> TextChannel {
    let mia = person("Mia")
    let leo = person("Leo")
    let ada = person("Ada")
    let general = invoke(new_channel(conversations, "general", topic: "Anything, really"))
    invoke(new_channel(conversations, "random"))
    let team = invoke(new_category(conversations, "Team"))
    let design = invoke(
        new_channel(conversations, "design", topic: "Reviews and critique", category: .some(team)),
    )
    invoke(new_channel(conversations, "release", category: .some(team)))
    invoke(new_channel(conversations, "planning", category: .some(team)))
    invoke(new_voice_channel(conversations, "Standup", category: .some(team)))
    let social = invoke(new_category(conversations, "Social"))
    invoke(new_channel(conversations, "weekend", category: .some(social)))
    invoke(new_channel(conversations, "books", category: .some(social)))
    invoke(new_voice_channel(conversations, "Lounge"))
    invoke(
        send(design, RichText("The new onboarding build is on the test machines.")),
        by: mia,
        at: 2026-06-29T07:02Z,
    )
    let launch = invoke(
        send(design, RichText("First launch is down to a second and a half on the old ThinkPad.")),
        by: mia,
        at: 2026-06-29T07:03Z,
    )
    invoke(react(launch, "🎉"), by: leo, at: 2026-06-29T07:05Z)
    invoke(react(launch, "🎉"), by: ada, at: 2026-06-29T07:06Z)
    invoke(react(launch, "🚀"), by: ada, at: 2026-06-29T07:06Z)
    invoke(pin(launch), by: ada, at: 2026-06-29T07:06Z)
    invoke(react(launch, "👍"), by: mia, at: 2026-06-29T07:07Z)
    let ask = invoke(
        send(design, RichText("Can we move the account step to the end?")),
        by: leo,
        at: 2026-06-29T07:20Z,
    )
    let t = invoke(start_thread(design, ask, "Account step"), by: ada, at: 2026-06-29T07:31Z)
    invoke(
        send(t, RichText("Sign-in first keeps sync simple, but people bounce there.")),
        by: ada,
        at: 2026-06-29T07:31Z,
    )
    invoke(
        send(t, RichText("Then let them in first and ask once they save something.")),
        by: leo,
        at: 2026-06-29T07:40Z,
    )
    invoke(
        send(design, RichText("Agreed. I'll update the flow this afternoon."), ask),
        by: leo,
        at: 2026-06-29T07:44Z,
    )
    invoke(
        send(design, RichText("Shots of the new sidebar are in the review folder.")),
        by: ada,
        at: 2026-06-30T08:10Z,
    )
    invoke(
        send(design, RichText("The spacing above the categories looks right now.")),
        by: mia,
        at: 2026-06-30T08:12Z,
    )
    invoke(
        send(general, RichText("Coffee machine on the third floor is fixed.")),
        by: leo,
        at: 2026-06-29T08:30Z,
    )
    let dm = invoke(message_person(conversations, leo))
    invoke(
        send(dm, RichText("Are we still on for the walk on Saturday?")),
        by: leo,
        at: 2026-06-30T09:02Z,
    )
    // The conversation with yourself: your notes, which the sidebar labels as yours.
    let notes = invoke(message_person(conversations, invoker))
    invoke(send(notes, RichText("Ask Leo about the walk on Saturday.")))
    design
}

/// A message made from another: the one it was made from draws the made message as a card, as
/// picking the message a menu's Make is open on makes it.
/// A conversation a bridge carries, with a message from outside it: its author as that place
/// shows them, and a custom emoji carried in their words.
scenario "a message from Discord" -> TextChannel {
    let mia = person("Mia")
    let design = invoke(new_channel(conversations, "design"))
    invoke(bridge(design, "Discord", outside_id: "456"), by: mia)
    invoke(
        relay(
            design,
            RichText("Look at this :party:!"),
            by: "Ada (Discord)",
            written_in: "Discord",
            avatar: "https://cdn.discordapp.com/avatars/1/abc.png",
            service: "Discord",
            outside_emoji: [
                OutsideEmoji(name: "party", picture: "https://cdn.discordapp.com/emojis/2.png"),
            ],
        ),
        by: mia,
    )
    design
}

scenario "a message made from another" -> TextChannel {
    let design = new_channel(conversations, "design")
    let ask = send(design, RichText("Can we move the account step to the end?"))
    invoke(
        send(design, RichText("Then let them in first and ask once they save something.")),
        from: ask,
    )
    design
}

/// A thread a question started: its window, the replies in it and one replying to another.
scenario "a thread" -> Thread {
    let mia = person("Mia")
    let leo = person("Leo")
    let design = new_channel(conversations, "design")
    let ask = send(design, RichText("Can we move the account step to the end?"))
    let t = start_thread(design, ask, "Account step")
    let first = invoke(
        send(t, RichText("Sign-in first keeps sync simple, but people bounce there.")),
        by: mia,
    )
    invoke(
        send(t, RichText("Then let them in first and ask once they save something."), first),
        by: leo,
    )
    t
}

/// A workspace holding no conversation yet, as a first window opens on it: the places say so
/// and the bar offers the way to make the first channel.
scenario "no channels yet" -> Conversations {
    conversations
}
