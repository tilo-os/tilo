app so.tilo.lang.chat "Chat" (icon: .message_circle, category: .network)

/// The workspace's conversations in the sidebar's order: channels outside any category, the
/// categories each with its own, and the direct messages, in the list as people.
type Channels = Collection<Category | TextChannel | Direct>

root channels: Channels

/// A named group of channels in the sidebar.
type Category {
    name: Text (title)
    /// The server's channels.
    // TODO: `Collection<Channel>` with `type Channel = TextChannel | VoiceChannel |
    // ForumChannel` once voice and forum channels land.
    channels: Collection<TextChannel>
}

/// Messages in one stream; any message can start a thread. A post is a change of its channel,
/// so `mark others` makes it unseen to everyone but its author: the sidebar counts the new ones.
type TextChannel {
    name: Text (title)
    topic: Text
    messages: Collection<Message>
    /// The threads its messages started, each the one a message's `thread` names.
    /// TODO(language): an owned `Option<Thread>` on the message; a reference needs a home.
    threads: Collection<Thread>

    search {
        mark others
    }
}

/// A message. Who wrote it, when, and its edits come from the log.
type Message {
    body: RichText (body)
    /// The message this one answers inline, quoted above it.
    reply_to: Option<Message>
    /// The side conversation branched off this message.
    thread: Option<Thread>
    /// What a message said before messages were rich text, kept as it was written.
    parts: Collection<Part>
    /// Whether it is pinned to its channel, which search finds.
    pinned: Bool = false
    /// Who reacted, by emoji.
    reactions: Map<Set<Principal>>
    /// A view the message sends, as its sender had it among their views: its tilolang
    /// definition, which a reader adds to their own views (Views' `add`), its author whoever
    /// sent the message. Empty for a message that sends none.
    sent_view: Text

    search {
        filter pinned (icon: .pin)
        mark others
    }
}

/// A side conversation branched off one message.
type Thread {
    name: Text (title)
    messages: Collection<Message>
    archived: Bool = false
}

/// A private conversation: one other person, or a named group of them, in the same message
/// model as a channel. Who is in it is its `people`, drawn from the workspace's members; its
/// place in the sidebar is a person (`ui.NavItem(person: ...)`), so a direct message reads as
/// the person's row and a group as its own. A one-to-one talks like any conversation.
type Direct {
    name: Text (title)
    /// The one person it is with, for a direct message. TODO(language): a group holds several
    /// (`people: Set<Principal>`); the language has no set literal to make one yet.
    person: Principal
    messages: Collection<Message>
    threads: Collection<Thread>

    search {
        mark others
    }
}

/// A piece of a message from before messages were rich text: its text, or a block's source.
type Part {
    /// `text` or `block`.
    kind: Text merge register
    body: Text
}

default view sidebar for channels

default view channel for TextChannel

default view direct for Direct

default view thread for Thread

/// Adds a text channel to the sidebar.
primary action new_channel(all: Channels, name: Text = "new-channel") -> TextChannel (
    icon: .plus,
    label: "New channel",
) {
    all.insert(TextChannel(name: name))
}

/// Adds a category to the sidebar.
action new_category(all: Channels, name: Text = "New category") -> Category (
    icon: .folder_plus,
    label: "New category",
) {
    all.insert(Category(name: name))
}

/// Adds a text channel to a category.
action add_channel(c: Category, name: Text = "new-channel") -> TextChannel (
    icon: .plus,
    label: "New channel",
) {
    c.channels.insert(TextChannel(name: name))
}

/// Opens the direct message with `who`, in the sidebar, or the one already open with them: a
/// person has one conversation, not a new empty one each time.
action message_person(all: Channels, who: Principal) -> Direct (
    icon: .message_circle,
    label: "Message",
) {
    // The one already open with them, else a new one in the sidebar: a person has one
    // conversation, not a new empty one each time.
    for item in all {
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
    let d = all.insert(Direct(name: who.name(), person: who))
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
    require created(m)?.who == .some(invoker) "Only its author can edit a message"
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

/// The sidebar: channels outside any category first, then each category with its channels.
/// Choosing one shows it beside the sidebar.
/// TODO(language): a resource's own key for a navigation item; the category and name stand in.
view sidebar(all: Channels) {
    lens shown: Text = ""
    ui.NavShell(content_kind: .page, sidebar_title: "Channels") {
        sidebar: {
            ui.NavList(selected: shown) {
                for item in all {
                    match item {
                        c: Category => {
                            ui.NavSection(c.name, collapsible: true) {
                                for ch in c.channels {
                                    ui.NavItem(
                                        "{c.name}/{ch.name}",
                                        ch.name,
                                        icon: .hash,
                                        item: ch,
                                        mentions: mentions(ch),
                                    ) {
                                        on select: set(shown, "{c.name}/{ch.name}").then(open(ch))
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
        content: {
            if all.is_empty() {
                ui.EmptyState("No channels yet", icon: .hash, action: "New channel") {
                    on act: new_channel(all)
                }
            } else {
                ui.EmptyState("No channel open", icon: .hash)
            }
        }
    }
}

/// A text channel: its messages, and a composer that can answer one of them.
view channel(c: TextChannel) {
    lens draft: RichText = RichText("")
    lens replying: Option<Message> = .none
    lens editing: Option<Message> = .none
    lens revised: RichText = RichText("")
    std.transcript(c, c.messages, unread: first_unseen(c)) { m ->
        channel_message(c, m, replying, editing, revised)
        composer: {
            typing_here(c)
            composer(c, draft, replying, "Message #{c.name}")
        }
    }
}

/// A direct conversation: the same messages and composer as a channel, with the person's name
/// over it.
view direct(d: Direct) {
    lens draft: RichText = RichText("")
    lens replying: Option<Message> = .none
    lens editing: Option<Message> = .none
    lens revised: RichText = RichText("")
    std.transcript(d, d.messages, unread: first_unseen(d)) { m ->
        direct_message(d, m, replying, editing, revised)
        composer: {
            direct_typing_here(d)
            composer(d, draft, replying, "Message {d.name}")
        }
    }
}

/// A message of a direct conversation, with what can be done to it on hover.
view direct_message(
    d: Direct,
    m: Message,
    replying: State<Option<Message>>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    message(m, editing, revised)
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
    replying: State<Option<Message>>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    message(m, editing, revised)
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
    lens draft: RichText = RichText("")
    lens replying: Option<Message> = .none
    lens editing: Option<Message> = .none
    lens revised: RichText = RichText("")
    std.transcript(t, t.messages) { m ->
        thread_message(m, replying, editing, revised)
        composer: {
            thread_typing_here(t)
            composer(t, draft, replying, "Message {t.name}")
        }
    }
}

/// A message of a thread, with what can be done to it on hover.
view thread_message(
    m: Message,
    replying: State<Option<Message>>,
    editing: State<Option<Message>>,
    revised: State<RichText>,
) {
    message(m, editing, revised)
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
view composer(
    to: TextChannel | Thread | Direct,
    draft: State<RichText>,
    replying: State<Option<Message>>,
    placeholder: Text,
) {
    if let r = replying {
        std.reply_banner(r, cancel: set(replying, .none))
    }
    row {
        rich_input(bind: draft, placeholder: placeholder, fill) {
            on submit: send(to, draft, replying)
                .then(set(draft, RichText("")))
                .then(set(replying, .none))
        }
        ui.Button("Send", icon: .send, icon_only: true) {
            on press: send(to, draft, replying)
                .then(set(draft, RichText("")))
                .then(set(replying, .none))
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
    if created(m)?.who == .some(workspace.me) and not m.body.is_empty() {
        ui.Button("Edit", icon: .pencil, icon_only: true) {
            on press: set(revised, m.body).then(set(editing, m))
        }
    }
}

/// What one message says: the message it answers, its text (or, while it is being edited, the
/// editor in its place), and the thread it started.
view message(m: Message, editing: State<Option<Message>>, revised: State<RichText>) {
    if let r = m.reply_to {
        std.quote(r)
    }
    if m.pinned {
        ui.Text("Pinned", role: .caption, secondary: true)
    }
    if let e = editing {
        if e == m {
            revision(m, editing, revised)
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
    std.linked(m)
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
view revision(m: Message, editing: State<Option<Message>>, revised: State<RichText>) {
    rich_input(bind: revised, placeholder: "Edit message") {
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
    let general = new_channel(channels, "general")
    general.topic = "Anything, really"
    new_channel(channels, "random")
    let team = new_category(channels, "Team")
    let design = add_channel(team, "design")
    design.topic = "Reviews and critique"
    add_channel(team, "release")
    add_channel(team, "planning")
    let social = new_category(channels, "Social")
    add_channel(social, "weekend")
    add_channel(social, "books")
    as mia at 2026-06-29T07:02Z {
        send(design, RichText("The new onboarding build is on the test machines."))
    }
    as mia at 2026-06-29T07:03Z {
        let launch = send(
            design,
            RichText("First launch is down to a second and a half on the old ThinkPad."),
        )
        as leo at 2026-06-29T07:05Z {
            react(launch, "🎉")
        }
        as ada at 2026-06-29T07:06Z {
            react(launch, "🎉")
            react(launch, "🚀")
            pin(launch)
        }
        as invoker at 2026-06-29T07:07Z {
            react(launch, "👍")
        }
    }
    as leo at 2026-06-29T07:20Z {
        let ask = send(design, RichText("Can we move the account step to the end?"))
        as ada at 2026-06-29T07:31Z {
            let t = start_thread(design, ask, "Account step")
            send(t, RichText("Sign-in first keeps sync simple, but people bounce there."))
            as leo at 2026-06-29T07:40Z {
                send(t, RichText("Then let them in first and ask once they save something."))
            }
        }
        as invoker at 2026-06-29T07:44Z {
            send(design, RichText("Agreed. I'll update the flow this afternoon."), ask)
        }
    }
    as ada at 2026-06-30T08:10Z {
        send(design, RichText("Shots of the new sidebar are in the review folder."))
    }
    as mia at 2026-06-30T08:12Z {
        send(design, RichText("The spacing above the categories looks right now."))
    }
    as leo at 2026-06-29T08:30Z {
        send(general, RichText("Coffee machine on the third floor is fixed."))
    }
    let dm = message_person(channels, leo)
    as leo at 2026-06-30T09:02Z {
        send(dm, RichText("Are we still on for the walk on Saturday?"))
    }
    // The conversation with yourself: your notes, which the sidebar labels as yours.
    let notes = message_person(channels, invoker)
    send(notes, RichText("Ask Leo about the walk on Saturday."))
    design
}

/// A thread a question started: its window, the replies in it and one replying to another.
scenario "a thread" -> Thread {
    let mia = person("Mia")
    let leo = person("Leo")
    let design = new_channel(channels, "design")
    let ask = send(design, RichText("Can we move the account step to the end?"))
    let t = start_thread(design, ask, "Account step")
    as mia {
        let first = send(t, RichText("Sign-in first keeps sync simple, but people bounce there."))
        as leo {
            send(t, RichText("Then let them in first and ask once they save something."), first)
        }
    }
    t
}
