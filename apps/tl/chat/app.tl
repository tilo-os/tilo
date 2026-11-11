app so.tilo.lang.chat "Chat" (icon: .message_circle, category: .network)

/// The workspace's conversations in the sidebar's order: channels outside any category, the
/// categories each with its own, and the direct messages, in the list as people.
type Conversations {
    channels: Collection<Category | TextChannel | Direct>
}

root conversations: Conversations

/// A named group of channels in the sidebar.
type Category {
    name: Text
    /// The server's channels.
    // TODO: `Collection<Channel>` with `type Channel = TextChannel | VoiceChannel |
    // ForumChannel` once voice and forum channels land.
    channels: Collection<TextChannel>
}

/// Messages in one stream; any message can start a thread. A post is a change of its channel,
/// so it is unseen to everyone but its author: Chat's Unread place counts the conversations
/// holding one.
type TextChannel {
    name: Text
    topic: Text
    messages: Collection<Message>
    /// The threads its messages started, each the one a message's `thread` names.
    /// TODO(language): an owned `Option<Thread>` on the message; a reference needs a home.
    threads: Collection<Thread>
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
    /// Who reacted, by emoji.
    reactions: Map<Set<Principal>>
    /// A view the message sends, as its sender had it among their views: a reader adds it to
    /// their own views, and its author is whoever sent the message. Empty for a message that
    /// sends none.
    sent_view: Text

    search {
        filter pinned (icon: .pin)
    }
}

/// A side conversation branched off one message.
type Thread {
    name: Text
    messages: Collection<Message>
    archived: Bool = false
}

/// A private conversation: one other person, or a named group of them, in the same message
/// model as a channel. Who is in it is its `people`, drawn from the workspace's members; its
/// place in the sidebar is a person (`ui.NavItem(person: ...)`), so a direct message reads as
/// the person's row and a group as its own. A one-to-one talks like any conversation.
type Direct {
    name: Text
    /// The one person it is with, for a direct message. TODO(language): a group holds several
    /// (`people: Set<Principal>`); the language has no set literal to make one yet.
    person: Principal
    messages: Collection<Message>
    threads: Collection<Thread>
}

/// A piece of a message kept as parts rather than as rich text: its text, or a block's source.
type Part {
    /// Whether the piece is text or a block's source.
    kind: Text merge register
    body: Text
}

default view sidebar for Conversations

default view channel for TextChannel

default view direct for Direct

default view thread for Thread

row view channel_summary for TextChannel

row view direct_summary for Direct

row view category_summary for Category

/// Adds a text channel to the sidebar.
primary action new_channel(all: Conversations, name: Text = say("new-channel")) -> TextChannel (
    icon: .plus,
    label: "New channel",
) {
    all.channels.insert(TextChannel(name: name))
}

/// Adds a category to the sidebar.
action new_category(all: Conversations, name: Text = say("New category")) -> Category (
    icon: .folder_plus,
    label: "New category",
) {
    all.channels.insert(Category(name: name))
}

/// Adds a text channel to a category.
action add_channel(c: Category, name: Text = say("new-channel")) -> TextChannel (
    icon: .plus,
    label: "New channel",
) {
    c.channels.insert(TextChannel(name: name))
}

/// Opens the direct message with `who`, in the sidebar, or the one already open with them: a
/// person has one conversation, not a new empty one each time.
action message_person(all: Conversations, who: Principal) -> Direct (
    icon: .message_circle,
    label: "Message",
) {
    // The one already open with them, else a new one in the sidebar: a person has one
    // conversation, not a new empty one each time.
    for item in all.channels {
        match item {
            c: Category => {}
            ch: TextChannel => {}
            d: Direct => {
                if d.person == who {
                    return d
                }
            }
        }
    }
    let d = all.channels.insert(Direct(name: who.name(), person: who))
    // Its fields written, so the record holds the conversation from its first press: an item
    // stored by its id alone is found only once it says something.
    d.name = who.name()
    d.person = who
    d
}

/// Posts a message in a channel, a thread or a direct conversation as the invoker, answering
/// another when `reply_to` names one.
action send(
    to: TextChannel | Thread | Direct,
    body: RichText,
    reply_to: Option<Message> = .none,
) -> Message (icon: .send, label: "Send") {
    let m = Message(body: body, reply_to: reply_to)
    match to {
        c: TextChannel => c.messages.insert(m),
        t: Thread => t.messages.insert(m),
        d: Direct => d.messages.insert(m),
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

/// Changes what a message says. Only its author may.
action edit(m: Message, body: RichText = m.body) (icon: .pencil, label: "Edit") {
    require created(m)?.who == invoker "Only its author can edit a message"
    require not body.is_empty() "Type a message first"
    m.body = body
}

/// Deletes a message from the channel or thread it is in. Its author, or anyone the
/// channel's permissions allow, may: the platform undoes it rather than asking first, and the
/// removal is a version of the message, so who removed it stays answerable.
destructive action delete(to: TextChannel | Thread | Direct, m: Message) (
    icon: .trash,
    label: "Delete",
) {
    match to {
        c: TextChannel => c.messages.remove(m),
        t: Thread => t.messages.remove(m),
        d: Direct => d.messages.remove(m),
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

/// Starts a thread from a message of a channel or a direct conversation, named after what the
/// message says.
action start_thread(c: TextChannel | Direct, m: Message, name: Text = m.body.excerpt) -> Thread (
    icon: .message_square,
    label: "Start thread",
) {
    match c {
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
            d: Direct => if first_unseen(d) != .none { [d] } else { [] },
        },
    )
    std.place(all, "Unread", needing) { x ->
        show(x, row: true)
    }
}

/// The conversations `c` holds that the person looking has not read.
fn unread_in(c: Category) -> List<TextChannel | Direct> {
    c.channels.filter(ch -> first_unseen(ch) != .none)
}

/// The sidebar: channels outside any category first, then each category with its channels.
/// Choosing one shows it beside the sidebar.
/// TODO(language): a resource's own key for a navigation item; the category and name stand in.
view sidebar(all: Conversations) {
    state shown: Text = ""
    ui.NavShell(content_kind: .page, sidebar_title: "Channels") {
        sidebar: {
            // A workspace with no channel and no one else to talk to says so where its places
            // would be: the screen a narrow window shows its places on never shows nothing. With
            // no places and nothing open the window says it once, in place of both columns, and
            // the way to make the first is the window bar's own, which every width shows.
            if all.channels.is_empty()
                and workspace.members.filter(m -> m != workspace.me).is_empty() {
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
                                        for ch in c.channels {
                                            ui.NavItem(
                                                "{c.name}/{ch.name}",
                                                ch.name,
                                                icon: .hash,
                                                item: ch,
                                                mentions: mentions(ch),
                                            ) {
                                                on select: set(shown, "{c.name}/{ch.name}")
                                                    .then(
                                                        open(ch),
                                                    )
                                            }
                                        }
                                    }
                                }
                            }
                            ch: TextChannel => {
                                ui.NavItem(
                                    "/{ch.name}",
                                    ch.name,
                                    icon: .hash,
                                    item: ch,
                                    mentions: mentions(ch),
                                ) {
                                    on select: set(shown, "/{ch.name}").then(open(ch))
                                }
                            }
                            d: Direct => {
                                // A direct conversation reads as the person's row: their avatar and
                                // presence. The conversation with yourself is your notes, and says
                                // so.
                                if d.person == workspace.me {
                                    ui.NavItem(
                                        "/dm/{d.name}",
                                        "Notes to self",
                                        icon: .bookmark,
                                        item: d,
                                        mentions: mentions(d),
                                    ) {
                                        on select: set(shown, "/dm/{d.name}").then(open(d))
                                    }
                                } else {
                                    ui.NavItem(
                                        "/dm/{d.name}",
                                        d.name,
                                        person: d.person,
                                        item: d,
                                        mentions: mentions(d),
                                    ) {
                                        on select: set(shown, "/dm/{d.name}").then(open(d))
                                    }
                                }
                            }
                        }
                    }
                    // Everyone in the workspace: a press opens the conversation with them, or the
                    // one already there. A person is a principal the workspace's membership knows,
                    // never a copy Chat keeps.
                    if not workspace.members.is_empty() {
                        ui.NavSection("Direct messages", collapsible: true) {
                            for p in workspace.members {
                                // Your own place is your notes, which the sidebar says above.
                                if p != workspace.me {
                                    ui.NavItem("/person/{p.name()}", p.name(), person: p) {
                                        on select: set(shown, "/person/{p.name()}")
                                            .then(
                                                message_person(all, p),
                                            )
                                    }
                                }
                            }
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
    state draft: RichText = RichText("")
    state replying: Option<Message> = .none
    state editing: Option<Message> = .none
    state revised: RichText = RichText("")
    std.transcript(c, c.messages, unread: first_unseen(c)) { m ->
        channel_message(c, m, c.messages, replying, editing, revised)
        composer: {
            typing_here(c)
            composer(c, c.messages, draft, replying, "Message #{c.name}")
        }
    }
}

/// A direct conversation: the same messages and composer as a channel, with the person's name
/// over it.
view direct(d: Direct) {
    state draft: RichText = RichText("")
    state replying: Option<Message> = .none
    state editing: Option<Message> = .none
    state revised: RichText = RichText("")
    std.transcript(d, d.messages, unread: first_unseen(d)) { m ->
        direct_message(d, m, d.messages, replying, editing, revised)
        composer: {
            direct_typing_here(d)
            composer(d, d.messages, draft, replying, "Message {d.name}")
        }
    }
}

/// A channel as another app's page summarizes it: its name, and what it is about.
view channel_summary(c: TextChannel) {
    row(c.name, secondary: c.topic, icon: .hash)
}

/// A direct conversation as another app's page summarizes it: whose it is and the last thing
/// said in it. The conversation with yourself is your notes, as the sidebar names it.
view direct_summary(d: Direct) {
    let last = d.messages.last()
    row(
        if d.person == workspace.me { "Notes to self" } else { d.name },
        secondary: if let m = last { m.body.excerpt } else { "" },
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
        ui.EmojiPicker {
            on pick(e): react(m, e)
        }
    }
}

/// A thread: its messages and a composer, the same way.
view thread(t: Thread) {
    state draft: RichText = RichText("")
    state replying: Option<Message> = .none
    state editing: Option<Message> = .none
    state revised: RichText = RichText("")
    std.transcript(t, t.messages) { m ->
        thread_message(m, t.messages, replying, editing, revised)
        composer: {
            thread_typing_here(t)
            composer(t, t.messages, draft, replying, "Message {t.name}")
        }
    }
}

/// A message of a thread, with what can be done to it on hover.
view thread_message(
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
        ui.EmojiPicker {
            on pick(e): react(m, e)
        }
    }
}

/// The foot of a conversation: the message being answered, over the input and its send button.
/// `said` is what the conversation holds, whose speakers its `@` drawer offers first.
view composer(
    to: TextChannel | Thread | Direct,
    said: List<Message>,
    draft: State<RichText>,
    replying: State<Option<Message>>,
    placeholder: Text,
) {
    if let r = replying {
        std.reply_banner(r, cancel: set(replying, .none))
    }
    std.composer(
        draft,
        placeholder,
        send(to, draft, replying).then(set(replying, .none)),
        speakers: speakers(said),
    )
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
    if created(m)?.who == workspace.me and not m.body.is_empty() {
        ui.Button("Edit", icon: .pencil, icon_only: true) {
            on press: set(revised, m.body).then(set(editing, m))
        }
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
    if not m.reactions.keys().is_empty() {
        ui.Reactions(reactions: m.reactions) {
            on toggle(e): react(m, e)
        }
    }
    if let t = m.thread {
        std.thread_chip(t, count: t.messages.len())
    }
    let listed = linked(m).filter(l -> shown.any(x -> l.of == x)).map(l -> l.of)
    std.linked(m, compact: true, without: listed)
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
    let general = new_channel(conversations, "general")
    general.topic = "Anything, really"
    new_channel(conversations, "random")
    let team = new_category(conversations, "Team")
    let design = add_channel(team, "design")
    design.topic = "Reviews and critique"
    add_channel(team, "release")
    add_channel(team, "planning")
    let social = new_category(conversations, "Social")
    add_channel(social, "weekend")
    add_channel(social, "books")
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
    let dm = message_person(conversations, leo)
    invoke(
        send(dm, RichText("Are we still on for the walk on Saturday?")),
        by: leo,
        at: 2026-06-30T09:02Z,
    )
    // The conversation with yourself: your notes, which the sidebar labels as yours.
    let notes = message_person(conversations, invoker)
    send(notes, RichText("Ask Leo about the walk on Saturday."))
    design
}

/// A message made from another: the one it was made from draws the made message as a card, as
/// picking the message a menu's Make is open on makes it.
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
