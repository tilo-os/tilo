# Chat: the flagship

A Discord-class chat (text and voice channels in categories, threads, replies, reactions,
mentions, slash commands, uploads) written in as little tilolang as the language allows, and as
good as the best chat apps. It's the test case for the language changes, the UI host's
interaction model and the system's defaults: if Chat is excellent in about 150 lines, the
defaults are right for every app.

## The app

```
app so.tilo.lang.chat "Chat" (icon: .message_circle, category: .network)

type Channels = List<Category | Channel>
root channels: Channels

/// A named group of channels in the sidebar.
type Category {
    name: Text (title)
    channels: List<Channel>
}

type Channel = TextChannel | VoiceChannel

/// Messages in one stream; any message can start a thread.
type TextChannel {
    name: Text (title)
    topic: Text
    messages: Collection<Message>
}

/// Join to talk live. Who's in it is presence, never stored.
type VoiceChannel {
    name: Text (title)
    limit: Option<Int>
}

/// A message. Who wrote it, when, and its edits come from the log.
type Message {
    body: RichText (body)
    reply_to: Option<Message>
    reactions: Map<Emoji, Set<Principal>>
    pinned: Bool = false
    thread: Option<Thread>

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

default view sidebar for channels
default view channel for TextChannel
default view voice for VoiceChannel
default view thread for Thread

/// Posts a message as the invoker, optionally answering another.
action send(to: TextChannel | Thread, body: RichText, reply_to: Option<Message> = .none) (icon: .send) {
    require not body.is_empty() else "Type a message first"
    to.messages.insert(Message(body: body, reply_to: reply_to))
}

/// Changes what a message says. Only its author may.
action edit(m: Message, body: RichText) (icon: .pencil) {
    require m.created.by == invoker else "Only {m.created.by} can edit this"
    m.body = body
}

/// Deletes a message.
destructive action delete(in: TextChannel | Thread, m: Message) (icon: .trash) {
    in.messages.remove(m)
}

/// Adds or removes your reaction.
action react(m: Message, emoji: Emoji, on: Bool = not m.reactions[emoji].contains(invoker)) {
    if on { m.reactions[emoji].add(invoker) } else { m.reactions[emoji].remove(invoker) }
}

/// Starts a thread from a message.
action start_thread(m: Message, name: Text = m.body.excerpt) -> Thread (icon: .messages_square) {
    require m.thread == .none else "This message already has a thread"
    m.thread = .some(Thread(name: name))
    m.thread!
}

/// Pins or unpins a message.
action pin(m: Message, pinned: Bool = not m.pinned) (icon: .pin, on: m.pinned) {
    m.pinned = pinned
}

/// Joins a voice channel: the system's call, with this channel as its place.
action join(v: VoiceChannel) uses call (icon: .phone) {
    require v.limit == .none or call.participants(v).count < v.limit! else "{v.name} is full"
    call.join(v)
}

/// The sidebar: uncategorized channels first, then each category.
view sidebar(items: Channels) {
    std.sidebar(items, empty: "No channels yet") { item ->
        match item {
            c: Category => section(c.name, collapsible: true) { for ch in c.channels { channel_row(ch) } },
            ch: Channel => channel_row(ch),
        }
    }
}

/// A channel in the sidebar; voice channels show who's in them.
view channel_row(ch: Channel) {
    match ch {
        t: TextChannel => row(t.name, icon: .hash),
        v: VoiceChannel => stack {
            row(v.name, icon: .volume)
            for p in call.participants(v) { row(p, indent: 1, speaking: call.speaking(p)) }
        },
    }
}

/// A text channel: its messages and a composer that can answer one of them.
view channel(c: TextChannel) {
    lens draft: RichText
    lens replying: Option<Message>
    conversation(c, draft: draft, replying: replying)
}

/// A thread, the same way.
view thread(t: Thread) {
    lens draft: RichText
    lens replying: Option<Message>
    conversation(t, draft: draft, replying: replying)
}

/// Messages above, composer below. Shared by channels and threads.
view conversation(in: TextChannel | Thread, draft: lens RichText, replying: lens Option<Message>) {
    std.transcript(in, in.messages, author: .created.by, time: .created.at) { m ->
        message_row(in, m, replying: replying)
    } composer: {
        if let r = replying { reply_banner(r, cancel: replying = .none) }
        rich_input(bind: draft, placeholder: "Message {in.name}") {
            complete "@" from: workspace.members as mention
            complete "#" from: channels as link
            complete ":" from: std.emoji as emoji
            complete "/" from: actions(in) as command
            on paste(files), drop(files): std.attach(draft, files)
            key up when draft.is_empty: edit_last(in)
            key escape when replying != .none: replying = .none
            key enter: send(in, body: draft, reply_to: replying)
                then { draft = .empty; replying = .none }
        }
    }
}

/// One message, with what you can do to it on hover, swipe and right-click.
view message_row(in: TextChannel | Thread, m: Message, replying: lens Option<Message>) {
    if let r = m.reply_to { quote(r) }
    rich_text(m.body)
    reactions(m.reactions, toggle: e -> react(m, e))
    if let t = m.thread { thread_chip(t, count: t.messages.count, last: t.messages.last()?.created.at) }

    hover_toolbar {
        for e in std.frequent_emoji(limit: 3) { button(react(m, e), label: e) }
        emoji_picker(pick: e -> react(m, e))
        button(icon: .reply, label: "Reply", then: replying = .some(m))
        if m.thread == .none { button(start_thread(m), icon_only: true) }
    }
    swipe(trailing: { replying = .some(m) }, leading: react(m, .thumbs_up))
    context_menu: {
        emoji_bar(std.frequent_emoji(limit: 6), pick: e -> react(m, e), more: emoji_picker)
        separator
        item(icon: .reply, label: "Reply", then: replying = .some(m))
        item(start_thread(m))
        item(pin(m))
        item(edit(m), when: m.created.by == me)
        separator
        item(delete(in, m))
    }
}

/// A voice channel: the system's call UI with this channel as its place.
view voice(v: VoiceChannel) {
    std.call(v)
}
```

## What the system does, so the app doesn't

- **Sending and status:** a sent message appears in the same frame (applied provisionally);
  "sending", "delivered" and "seen" are its finality and delivery facts, shown by
  `std.transcript`. Failures show their reason and a retry; offline shows "will send when you're
  back online".
- **Presence:** "typing" comes from others' lens drafts (never their text); read position is
  lens state, which gives the unread divider, "jump to unread", badges and read receipts; voice
  participants and who's speaking come from call presence.
- **Who and when:** author, time, "edited" and edit history come from the log.
- **Scrolling:** the list is virtual and pages from the index; following new messages at the
  bottom, never jumping while reading history, a "new messages" pill.
- **Motion:** keyed inserts, removals, edits and reactions animate on the platform's springs;
  swipes follow the finger and commit or spring back; reduced motion is respected.
- **Menus and input:** the menu's typing field (filter, or ask the agent), remaining allowed
  actions, keyboard navigation, long-press, phone sheets; completion lists, markdown shortcuts and
  uploads with progress. All of it runs in the UI host at frame rate; the app answers data and
  actions only.
- **Mentions, links, notifications, unread, search, link previews:** system-level, from
  `RichText` references and the index.
- **Members, roles, channel permissions:** the workspace's own.

## Acceptance

- Budgets, enforced as counted work in the app's performance matrix: keystroke to glyph under one
  frame; send to visible in the same frame; a peer's message visible within the network round
  trip plus one frame; channel switch under 50 ms; scrolling 100k messages at native refresh
  rate.
- Three people typing, reacting and replying in one channel with voice running: no dropped
  frames, nothing jumps, nobody's typing touches anyone else's.
- Every action reachable by keyboard, touch, palette and agents.
- Shots at desktop and phone, light and dark, reviewed against docs/design-language.md.
- The source stays near the size above.

## Needs

| Need | Status |
|---|---|
| Union types, `Map`, roots as values, `type X = …`, key paths, `if let`, `?.`/`??`, lens parameters | language changes, in progress |
| `RichText` with references | filed |
| `context_menu:`, `hover_toolbar`, `swipe`, completion, key bindings, transitions, run in the UI host | to build |
| Others' lens state (typing, read position) and delivery facts readable by `std` | to build |
| `uses call`, `call.participants`, `std.call` over the native call stack | to build |
