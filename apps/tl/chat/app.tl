app so.tilo.lang.chat "Chat" (icon: .message_circle, category: .network)

/// The workspace's channels in the sidebar's order: channels outside any category, and the
/// categories, each with its own.
type Channels = Collection<Category | TextChannel>

root channels: Channels

/// A named group of channels in the sidebar.
type Category {
    name: Text (title)
    /// TODO: `Collection<Channel>` with `type Channel = TextChannel | VoiceChannel |
    /// ForumChannel` once voice and forum channels land.
    channels: Collection<TextChannel>
}

/// Messages in one stream; any message can start a thread.
type TextChannel {
    name: Text (title)
    topic: Text
    messages: Collection<Message>
    /// The threads its messages started, each the one a message's `thread` names.
    /// TODO(language): an owned `Option<Thread>` on the message; a reference needs a home.
    threads: Collection<Thread>
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

    search {
        filter pinned (icon: .pin)
    }
}

/// A side conversation branched off one message.
type Thread {
    name: Text (title)
    messages: Collection<Message>
    archived: Bool = false
}

/// A piece of a message from before messages were rich text: its text, or a block's source.
type Part {
    /// `text` or `block`.
    kind: Text merge register
    body: Text
}

default view sidebar for channels

default view channel for TextChannel

default view thread for Thread

/// Adds a text channel to the sidebar.
primary action new_channel(all: Channels, name: Text = "new-channel") -> TextChannel (icon: .plus, label: "New channel") {
    all.insert(TextChannel(name: name))
}

/// Adds a category to the sidebar.
action new_category(all: Channels, name: Text = "New category") -> Category (icon: .folder_plus, label: "New category") {
    all.insert(Category(name: name))
}

/// Adds a text channel to a category.
action add_channel(c: Category, name: Text = "new-channel") -> TextChannel (icon: .plus, label: "New channel") {
    c.channels.insert(TextChannel(name: name))
}

/// Posts a message in a channel or a thread as the invoker, answering another when `reply_to`
/// names one.
action send(to: TextChannel | Thread, body: RichText, reply_to: Option<Message> = .none) -> Message (icon: .send, label: "Send") {
    let m = Message(body: body, reply_to: reply_to)
    match to {
        c: TextChannel => c.messages.insert(m),
        t: Thread => t.messages.insert(m),
    }
}

/// Changes what a message says. Only its author may.
action edit(m: Message, body: RichText = m.body) (icon: .pencil, label: "Edit") {
    require created(m)?.who == .some(invoker) "Only its author can edit a message"
    require not body.is_empty() "Type a message first"
    m.body = body
}

/// Pins a message to its channel, or unpins it.
action pin(m: Message, pinned: Bool = not m.pinned) (label: if pinned { "Pin" } else { "Unpin" }, icon: .pin, on: m.pinned) {
    m.pinned = pinned
}

/// Adds your reaction with an emoji to a message, or takes it back when you had reacted with it.
action react(m: Message, emoji: Text) (icon: .smile_plus, label: "React") {
    let mine = if let who = m.reactions.get(emoji) { who.any(p -> p == invoker) } else { false }
    if mine { m.reactions.remove(emoji, invoker) } else { m.reactions.add(emoji, invoker) }
}

/// Starts a thread from a message of a channel, named after what the message says.
action start_thread(c: TextChannel, m: Message, name: Text = m.body.excerpt) -> Thread (icon: .message_square, label: "Start thread") {
    let t = c.threads.insert(Thread(name: name))
    m.thread = t
    t
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
                                    ui.NavItem("{c.name}/{ch.name}", ch.name, icon: .hash) {
                                        on select: set(shown, "{c.name}/{ch.name}").then(open(ch))
                                    }
                                }
                            }
                        }
                        ch: TextChannel => {
                            ui.NavItem("/{ch.name}", ch.name, icon: .hash) {
                                on select: set(shown, "/{ch.name}").then(open(ch))
                            }
                        }
                    }
                }
            }
        }
        actions: {
            std.header(actions(all))
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
    std.transcript(c, c.messages) { m ->
        channel_message(c, m, replying, editing, revised)
        composer: {
            composer(c, draft, replying, "Message #{c.name}")
        }
    }
}

/// A message of a channel, with what can be done to it on hover.
view channel_message(c: TextChannel, m: Message, replying: State<Option<Message>>, editing: State<Option<Message>>, revised: State<RichText>) {
    message(m, editing, revised)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
        if m.thread == .none {
            button(start_thread(c, m), icon_only: true)
        }
        edit_button(m, editing, revised)
        pin_button(m)
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
            composer(t, draft, replying, "Message {t.name}")
        }
    }
}

/// A message of a thread, with what can be done to it on hover.
view thread_message(m: Message, replying: State<Option<Message>>, editing: State<Option<Message>>, revised: State<RichText>) {
    message(m, editing, revised)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
        edit_button(m, editing, revised)
        pin_button(m)
        ui.EmojiPicker {
            on pick(e): react(m, e)
        }
    }
}

/// The foot of a conversation: the message being answered, over the input and its send button.
view composer(to: TextChannel | Thread, draft: State<RichText>, replying: State<Option<Message>>, placeholder: Text) {
    if let r = replying {
        std.reply_banner(r, cancel: set(replying, .none))
    }
    row {
        ui.RichInput(value: draft, placeholder: placeholder, fill) {
            on submit: send(to, draft, replying).then(set(draft, RichText(""))).then(set(replying, .none))
        }
        ui.Button("Send", icon: .send, icon_only: true) {
            on press: send(to, draft, replying).then(set(draft, RichText(""))).then(set(replying, .none))
        }
    }
}

/// Edit, for whoever may edit the message (its author): puts the editor in its place.
view edit_button(m: Message, editing: State<Option<Message>>, revised: State<RichText>) {
    if actions(m).any(a -> a.label == "Edit") {
        ui.Button("Edit", icon: .pencil, icon_only: true) {
            on press: set(revised, m.body).then(set(editing, m))
        }
    }
}

/// Pin, or Unpin on a pinned message.
/// TODO(runtime): `button(pin(m), icon_only: true)`; in a hover toolbar its label stays as it was
/// first presented when the message is pinned.
view pin_button(m: Message) {
    ui.Button(if m.pinned { "Unpin" } else { "Pin" }, icon: .pin, icon_only: true) {
        on press: pin(m)
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
}

/// What a message says.
/// TODO(language): "edited" once a view can read when the body last changed (a field's own
/// versions); the message's last version is any change of it, a pin or a reaction too.
view said(m: Message) {
    if not m.body.is_empty() {
        rich_text(m.body)
    }
    for p in m.parts {
        text(p.body)
    }
}

/// A message being edited in place: its text in an editor, saved or left as it was.
view revision(m: Message, editing: State<Option<Message>>, revised: State<RichText>) {
    ui.RichInput(value: revised, placeholder: "Edit message") {
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
        let launch = send(design, RichText("First launch is down to a second and a half on the old ThinkPad."))
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
    design
}
