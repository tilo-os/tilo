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
            for item in all {
                match item {
                    c: Category => {
                        ui.NavList(title: c.name, selected: shown) {
                            for ch in c.channels {
                                ui.NavItem("{c.name}/{ch.name}", ch.name, icon: .hash) {
                                    on select: set(shown, "{c.name}/{ch.name}").then(open(ch))
                                }
                            }
                        }
                    }
                    ch: TextChannel => {
                        ui.NavList(selected: shown) {
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
    std.transcript(c, c.messages) { m ->
        channel_message(c, m, replying)
        composer: {
            composer(c, draft, replying, "Message #{c.name}")
        }
    }
}

/// A message of a channel, with what can be done to it on hover.
view channel_message(c: TextChannel, m: Message, replying: State<Option<Message>>) {
    message(m)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
        if m.thread == .none {
            button(start_thread(c, m), icon_only: true)
        }
    }
}

/// A thread: its messages and a composer, the same way.
view thread(t: Thread) {
    lens draft: RichText = RichText("")
    lens replying: Option<Message> = .none
    std.transcript(t, t.messages) { m ->
        thread_message(m, replying)
        composer: {
            composer(t, draft, replying, "Message {t.name}")
        }
    }
}

/// A message of a thread, with what can be done to it on hover.
view thread_message(m: Message, replying: State<Option<Message>>) {
    message(m)
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true) {
            on press: set(replying, m)
        }
    }
}

/// The foot of a conversation: the message being answered, over the input and its send button.
view composer(to: TextChannel | Thread, draft: State<RichText>, replying: State<Option<Message>>, placeholder: Text) {
    if let r = replying {
        std.reply_banner(r, cancel: set(replying, .none))
    }
    row {
        rich_input(bind: draft, placeholder: placeholder, fill)
        ui.Button("Send", icon: .send, icon_only: true) {
            on press: send(to, draft, replying).then(set(draft, RichText(""))).then(set(replying, .none))
        }
    }
}

/// What one message says: the message it answers, its text, and the thread it started.
view message(m: Message) {
    if let r = m.reply_to {
        std.quote(r)
    }
    if not m.body.is_empty() {
        rich_text(m.body)
    }
    for p in m.parts {
        text(p.body)
    }
    if let t = m.thread {
        std.thread_chip(t, count: t.messages.len())
    }
}

/// A team's chat a morning in: categories of channels, a busy channel with replies and a thread.
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
    add_channel(team, "standup")
    let social = new_category(channels, "Social")
    add_channel(social, "weekend")
    add_channel(social, "books")
    as mia at 2026-06-29T07:02Z {
        send(design, RichText("The new onboarding build is on the test machines."))
    }
    as mia at 2026-06-29T07:03Z {
        send(design, RichText("First launch is down to a second and a half on the old ThinkPad."))
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
    as ada at 2026-06-29T08:10Z {
        send(design, RichText("Shots of the new sidebar are in the review folder."))
    }
    as mia at 2026-06-29T08:12Z {
        send(design, RichText("The spacing above the categories looks right now."))
    }
    as leo at 2026-06-29T08:30Z {
        send(general, RichText("Coffee machine on the third floor is fixed."))
    }
    design
}
