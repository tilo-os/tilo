app so.tilo.chat "Chat" (
    icon: .message_circle,
    category: .network,
    tagline: "Workspaces for messages, calls and shared work",
    keywords: ["messenger", "instant messaging", "conversation"],
    previously: so.tilo.lang.chat,
    splash_budget: 250,
)

use tilo.workspace as workspace

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
}

root conversations: Conversations

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
    /// Who reacted, by emoji.
    reactions: Map<Set<Actor>>
    /// A view the message sends, as its sender had it among their views: a reader adds it to
    /// their own views, and its author is whoever sent the message. Empty for a message that
    /// sends none.
    sent_view: Text
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
/// alone. The native app's own documents are left as they are.
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
}

/// The workspace's skills the native Chat kept before this app took its id over, carried into
/// these once: what each is called, the work it is for, how the work is done here, the form of
/// its result and the model, under the id it had there and in the order it stood, and the
/// examples it kept, oldest first, each with what the result was, whether the person accepted
/// it, what they said and when it was kept. The native app's own document is left as it is; a
/// document whose examples do not read as records of their own carries nothing, so no skill
/// comes without the examples it kept.
migration action carry_native_skills(all: Conversations) {
    let earlier = earlier(all)
    let skills = earlier.filter(e -> e.of == "skills")
    if not skills.any(e -> examples_problem(e) != "") {
        for e in skills {
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
            // Each example it kept, oldest first, with when it was kept there.
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
}

/// What of the examples the native skill `e` kept cannot be carried: a skill whose examples do
/// not read as records of their own, or an example that says nothing. Empty when they all read.
fn examples_problem(e: host.Earlier) -> Text {
    let kept = e.lists.get("examples") ?? []
    let held = e.records.get("examples") ?? []
    if kept.len() != held.len() {
        "examples that do not read as records"
    } else if held.any(example -> (example.get("text") ?? "").trim() == "") {
        "an example that says nothing"
    } else { "" }
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
fn in_workspace(who: Actor, me: Actor) -> Bool {
    who.kind() != .person or who == me or workspace.members.people.contains(who)
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
/// given, as a program writing a message says it.
action send(
    to: TextChannel | Thread | Direct,
    body: RichText = RichText(""),
    reply_to: Option<Message> = .none,
    attachments: List<File> = [],
    turn: Text = "",
    draft: Option<Draft> = .none,
) -> Message uses notify (icon: .send, label: "Send", did: "wrote a message") {
    require not empty_writing(draft, body) "Type a message first"
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
) -> Message (icon: .send, label: "Relay", did: "relayed a message") {
    require by.trim() != "" "Say who wrote the message"
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
action edit(m: Message, body: RichText = m.body, turn: Text = m.turn) (
    icon: .pencil,
    label: "Edit",
) {
    require created(m)?.who == invoker "Only its author can edit a message"
    require not body.is_empty() "Type a message first"
    m.body = body
    m.turn = turn
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

/// Settles a report: what a moderator decided about it.
action decide(r: Report, decision: Text) (icon: .shield, label: "Decide") {
    require moderates(invoker) "Only the workspace's moderators settle a report"
    require decision.trim() != "" "Say what was decided"
    r.decision = decision.trim()
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

/// The sidebar: channels outside any category first, then each category with its channels.
/// Choosing one shows it beside the sidebar.
/// TODO(language): a resource's own key for a navigation item; the category and name stand in.
view sidebar(all: Conversations) {
    state shown: Text = ""
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
                                        for x in c.channels {
                                            match x {
                                                ch: TextChannel => {
                                                    if not ch.archived {
                                                        ui.NavItem(
                                                            "{c.name}/{ch.name}",
                                                            ch.name,
                                                            icon: .hash,
                                                            item: ch,
                                                            unread: first_unseen(ch) != .none,
                                                            mentions: mentions(ch),
                                                        ) {
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
                                        unread: first_unseen(ch) != .none,
                                        mentions: mentions(ch),
                                    ) {
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
                                            unread: first_unseen(d) != .none,
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
                                            unread: first_unseen(d) != .none,
                                            mentions: mentions(d),
                                        ) {
                                            on select: set(shown, "/dm/self").then(open(d))
                                        }
                                    }
                                }
                            }
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
            // asks for them.
            row(distribute: .end) {
                if c.messages.any(m -> m.pinned) {
                    ui.Button("Pinned messages", icon: .pin, size: .small, role: .plain) {
                        on press: open(c, view: pins)
                    }
                }
                ui.MenuButton("Channel", icon: .more_horizontal) {
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
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A voice channel: the system's call, with this channel as its place.
view voice(v: VoiceChannel) {
    std.call(v)
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
            if o.avatar != "" {
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
        row(gap: .tight, wrap: true) {
            for f in m.attachments {
                ui.Chip(f.name, icon: .paperclip) {
                    on press: open_attachment(f)
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
        stack(gap: .tight) {
            for c in t.calls {
                if let s = invoked(c) {
                    turn_step(s)
                }
            }
            if t.failure != "" {
                ui.Text(t.failure, role: .caption)
            }
            row(gap: .tight, align: .center) {
                ui.Text(turn_said(t), role: .caption, secondary: true, fill: 1.0, truncate)
                if t.state == "running" or t.state == "waiting" {
                    button(host.cancel(t.record), icon_only: true)
                } else if t.state == "ended" or t.state == "failed" or t.state == "cancelled" {
                    button(host.retry(t.record), icon_only: true)
                }
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
        said(m)
        if m.turn != "" {
            turn_steps(m)
        }
    }
}

/// What a message says, and that it was edited when its history holds an edit.
view said(m: Message) {
    if not m.body.is_empty() {
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
        ui.Text("edited", role: .caption, secondary: true)
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
