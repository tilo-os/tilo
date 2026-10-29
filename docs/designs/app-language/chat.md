# Chat: the flagship

A Discord-class chat (text and voice channels in categories, threads, replies, reactions,
mentions, slash commands, uploads) written in as little tilolang as the language allows, and as
good as the best chat apps. It's the test case for the language changes, the UI host's
interaction model and the system's defaults: if Chat is excellent in about 150 lines, the
defaults are right for every app.

## The app

```
app so.tilo.lang.chat "Chat" (icon: .message_circle, category: .network)

type Channels = Collection<Category | Channel>
root channels: Channels

/// A named group of channels in the sidebar.
type Category {
    name: Text (title)
    channels: Collection<Channel>
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
    let t = Thread(name: name)
    m.thread = .some(t)
    t
}

/// Pins or unpins a message.
action pin(m: Message, pinned: Bool = not m.pinned) (icon: .pin, on: m.pinned) {
    m.pinned = pinned
}

/// Joins a voice channel: the system's call, with this channel as its place.
action join(v: VoiceChannel) uses call (icon: .phone) {
    if let max = v.limit { require call.participants(v).count < max else "{v.name} is full" }
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
            on paste(files): std.attach(draft, files)
            on drop(files): std.attach(draft, files)
        }
    }
}

/// One message, with what you can do to it on hover, swipe and right-click.
view message_row(in: TextChannel | Thread, m: Message, replying: lens Option<Message>) {
    if let r = m.reply_to { quote(r) }
    rich_text(m.body)
    reactions(m.reactions, toggle: e -> react(m, e))
    if let t = m.thread { thread_chip(t, count: t.messages.count, last: t.messages.last()?.created.at) }

    ui.HoverToolbar {
        for e in std.frequent_emoji(limit: 3) { button(react(m, e), label: e) }
        ui.EmojiPicker { on pick(e): react(m, e) }
        button(icon: .reply, label: "Reply") { on activate: set(replying, .some(m)) }
        if m.thread == .none { button(start_thread(m), icon_only: true) }
    }
    ui.Swipe {
        trailing: { button(icon: .reply, label: "Reply") { on activate: set(replying, .some(m)) } }
        leading: { button(react(m, .thumbs_up)) }
    }
    ui.ContextMenu {
        ui.EmojiBar(std.frequent_emoji(limit: 6)) { on pick(e): react(m, e) }
        ui.MenuSeparator
        ui.MenuItem(icon: .reply, label: "Reply") { on activate: set(replying, .some(m)) }
        ui.MenuItem(start_thread(m))
        ui.MenuItem(pin(m))
        if m.created.by == me { ui.MenuItem(edit(m)) }
        ui.MenuSeparator
        ui.MenuItem(delete(in, m))
    }
}

/// A voice channel: the system's call UI with this channel as its place.
view voice(v: VoiceChannel) {
    std.call(v)
}
```

## Completion drawers

The drawers are the system's behaviour of every rich input; no view declares them. The
system gives the sources (`@` the people in this conversation, then the members; `#` channels
and other resources from the search index; `:` the emoji; `/` the actions available here),
once, and everything below happens in the window as each key is typed.

- **The query is the token.** What's typed after the trigger stays in the input as a tinted
  pending token (`@mi`); there's no separate search field. It opens on the trigger keystroke,
  anchored at the caret (above a bottom composer, below otherwise), and closes on Esc (the text
  stays), when the caret leaves the token, or on a space with no match.
- **Size.** Desktop: a popover about 22rem wide, 8 rows visible, scrolling beyond. Phone: a
  full-width panel docked above the keyboard, 44dp rows.
- **Rows** are the type's row view at list size: people with avatar, name (matched letters bold),
  handle, and a right-side label ("in this channel", role); channels with icon, name, topic,
  unread count; tasks with state, identifier, title, assignee.
- **Order with no letters yet:** "In this conversation" (recent speakers, newest first, up to
  5), then "Members" (by how often you interact), then group mentions if you may use them.
- **Order while typing:** one list, no headers: names or handles starting with the query, then a
  word in the name starting with it, then substrings, then close fuzzy matches; within each tier,
  this conversation, then the channel, then the workspace, then people outside it (labelled).
  Accents and case fold as in system search.
- **Stability.** A still-matching selected row keeps its place; the list height animates;
  results from slow outside sources arrive under their own header below the local ones, with a
  progress indicator, and never reorder what's shown.
- **Keys and touch.** The first row starts selected; ↑ ↓ (wrapping), Enter or Tab inserts, Esc
  closes, Page Up and Down. Hover highlights, click inserts. Touch taps insert; scrolling keeps
  the keyboard up.
- **Insert.** The token becomes the resource's inline view plus a space; one undo turns it back
  into the typed text.
- **No match.** One row saying so; for channels and tasks, "Create … "xyz"" when allowed.
- **Preview.** On wide screens the selected row shows its card beside the drawer.
- **Accessibility.** A list with the result count and the selected item announced.
- **Commands (`/`).** Rows: app icon, `/name`, a one-line description, parameter hints
  (`person duration message`); grouped by app when many; recently used, then this channel's,
  then the rest; filtering by name, then description words. Picking one turns the input into the
  command: a chip followed by labelled inline fields, Tab between them; a person field opens the
  people drawer, a duration takes `2h` or `tomorrow 9:00`; errors underline with a message below;
  Enter runs it once required fields are filled; destructive commands preview first.

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
- **Attached components:** `ui.ContextMenu`, `ui.HoverToolbar` and `ui.Swipe` draw nothing
  where they stand and attach to the view they're in. No new statement syntax; the same meaning
  everywhere. Destructive items are undone afterwards, not confirmed first (the platform rule).
  Keys are default shortcuts on the actions offered, a separate design.
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
| `RichText` with references | type, storage as tilo-doc rich text, merge, references (one form: the catalogue's resource reference), search and mention notices built; `rich_text` renders each reference as the reader's inline view (no-access shown as only existing, click opens) ; `rich_input` built, with its drawers (next row) |
| `ui.ContextMenu`, `ui.HoverToolbar`, `ui.Swipe` (attached, run in the UI host) | done |
| `rich_input` with its completion drawers (`@` this conversation's recent speakers then `members(r)`, and `:` emoji built; `#` index and `/` action sources to wire) | in progress |
| Transitions, emoji picker, reactions, quote, reply banner, thread chip | to build |
| Others' lens state (typing, read position) and delivery facts readable by `std` | to build |
| `uses call`, `call.participants`, `std.call` over the native call stack | to build |
