app so.tilo.test.block_chat "Block chat" (icon: .message_circle, category: .network)

/// A chat whose messages hold blocks of tilolang that render inline and may apply actions:
/// the fixture these tests run agents' and strangers' blocks through.
///
/// One piece of a message: plain text, or the source of a block that renders inline.
type Part {
    /// `text` or `block`.
    kind: Text merge register
    body: Text
    /// For a block that applies an action: what became of it, recorded by the host when the
    /// message was posted. The embed draws it; rendering never runs the action.
    invocation: Option<host.Invocation>
}

/// A message is a list of parts and the principal that wrote it: a person, or an agent task.
/// The host records the author, so it cannot be claimed. It is also the code author of the
/// message's blocks.
type Message {
    author: Principal
    parts: Collection<Part>
}

/// A conversation: its messages, in the order they were posted.
type Conversation {
    title: Text
    messages: Collection<Message>
}

/// The workspace's conversations.
type BlockChat {
    items: Collection<Conversation>
}

root conversations: BlockChat

default view chats for conversations

default view thread for Conversation

row view conversation_row for Conversation

/// Starts a conversation with a title.
action start(all: BlockChat, title: Text) -> Conversation {
    all.items.insert(Conversation(title: title))
}

/// Starts a conversation.
primary action new_chat(all: BlockChat) -> Conversation (icon: .plus) {
    all.items.insert(Conversation(title: "New chat"))
}

/// Posts a message as the invoker: a text part when `text` is not empty, then a block part
/// holding `block` as source when it is not empty. People and agents post the same way. A
/// block that applies an action is run once, by the host right after the post commits, as the
/// invoker; the outcome is recorded in the part.
action post(c: Conversation, text: Text, block: Text) -> Message {
    let m = c.messages.insert(Message(author: invoker))
    if text != "" {
        m.parts.insert(Part(kind: "text", body: text))
    }
    if block != "" {
        let p = m.parts.insert(Part(kind: "block", body: block))
        run_block(p, block)
    }
    m
}

/// Posts what someone typed, as them.
action send(c: Conversation, text: Text) (icon: .send, label: "Send") {
    if text != "" { post(c, text, "") }
}

/// The chat's window: its conversations, the latest active first, each opening in a window of
/// its own. A post is a change of its conversation; an edit of a message, or a draft, is not.
view chats(all: BlockChat) {
    let shown = all.items.sort_by(c -> updated(c), desc)
    ui.NavShell(
        content_kind: .document,
        overview_title: .some(meta(BlockChat).title),
        overview_count: .some(shown.len()),
    ) {
        overview: {
            if shown.is_empty() {
                ui.EmptyState("No conversations yet", icon: meta(Conversation).icon)
            } else {
                std.list(shown) { (c, at) ->
                    ui.ListItem(item: c, presence: at) {
                        conversation_row(c)
                    }
                }
            }
        }
        content: {
            ui.EmptyState(
                phrase("tilolang-none-selected", kind: meta(Conversation).noun),
                icon: meta(Conversation).icon,
            )
        }
    }
}

/// What a message says in a line: its text, or that it holds a block.
fn said(m: Message) -> Text {
    if let p = m.parts.first() {
        if p.kind == "text" { p.body } else { "Sent a block" }
    } else { "" }
}

/// One conversation in a list: its title, and the last message with who wrote it and when.
/// Its avatar is whoever wrote last; before anyone has, the group's.
view conversation_row(c: Conversation) {
    let last = c.messages.last()
    let line = if let m = last { said(m) } else { "No messages yet" }
    let at = if let m = last { created(m)?.at } else { .none }
    row(c.title, secondary: line, meta: at, avatar: last?.author, icon: .users)
}

/// A conversation's window: its messages, the latest at the foot, above the composer. What is
/// typed there is the person's own until they send it.
view thread(c: Conversation) {
    state draft: Text
    std.transcript(c, c.messages, row: message) {
        composer: {
            row {
                input(bind: draft, placeholder: "Message", fill)
                button(send(c, draft).then(set(draft, "")), icon_only: true) as send
            }
        }
    }
}

/// What one message says: its parts, in order. The transcript shows who wrote it and when.
view message(m: Message) {
    stack(gap: .related) {
        for p in m.parts {
            if p.kind == "block" {
                embed(p.body, author: m.author, invocation: p.invocation)
            } else {
                text(p.body)
            }
        }
    }
}

/// A workspace's conversations with the people in it, enough to scroll, the first one under way.
scenario "a busy workspace" -> Conversation {
    let mia = person("Mia")
    let leo = person("Leo")
    let ada = person("Ada")
    let plans = conversations.items.insert(Conversation(title: "Weekend plans"))
    invoke(
        post(plans, "Is everyone still up for the walk on Saturday?", ""),
        by: mia,
        at: 2026-06-29T07:02Z,
    )
    invoke(
        post(plans, "The long loop, round the reservoir and back over the hill.", ""),
        by: mia,
        at: 2026-06-29T07:03Z,
    )
    invoke(
        post(plans, "I can bring sandwiches. The forecast says sun until three.", ""),
        by: leo,
        at: 2026-06-29T07:20Z,
    )
    invoke(post(plans, "Count me in.", ""), at: 2026-06-29T07:24Z)
    invoke(post(plans, "Let's meet at the station at ten.", ""), by: ada, at: 2026-06-30T07:51Z)
    invoke(post(plans, "Platform four, by the coffee stand.", ""), by: ada, at: 2026-06-30T07:52Z)
    // Each conversation is started when its first message is posted, so the list's order,
    // the latest activity first, is that of their last messages.
    let release = invoke(
        conversations.items.insert(Conversation(title: "Release checklist")),
        by: leo,
        at: 2026-06-30T06:40Z,
    )
    invoke(
        post(release, "Docs still need the new screenshots.", ""),
        by: leo,
        at: 2026-06-30T06:40Z,
    )
    invoke(
        post(release, "I'll take the screenshots after lunch.", ""),
        by: ada,
        at: 2026-06-30T07:15Z,
    )
    let flat = invoke(
        conversations.items.insert(Conversation(title: "Flat hunting")),
        at: 2026-06-29T15:10Z,
    )
    invoke(post(flat, "The one on Elm Street has a balcony.", ""), at: 2026-06-29T15:10Z)
    invoke(
        post(flat, "Viewing is Saturday at eleven, bring the deposit papers.", ""),
        by: mia,
        at: 2026-06-29T16:45Z,
    )
    let books = invoke(
        conversations.items.insert(Conversation(title: "Book club: Piranesi")),
        by: ada,
        at: 2026-06-28T19:30Z,
    )
    invoke(
        post(books, "Halfway through. The statues are wonderful.", ""),
        by: ada,
        at: 2026-06-28T19:30Z,
    )
    let design = invoke(
        conversations.items.insert(Conversation(title: "Design review")),
        by: leo,
        at: 2026-06-27T10:05Z,
    )
    invoke(
        post(design, "The sidebar collapses too early at large text.", ""),
        by: leo,
        at: 2026-06-27T10:05Z,
    )
    invoke(post(design, "Agreed, I'll look at the thresholds.", ""), by: leo, at: 2026-06-27T10:12Z)
    let birthday = invoke(
        conversations.items.insert(Conversation(title: "Sam's birthday")),
        by: mia,
        at: 2026-06-26T18:00Z,
    )
    invoke(
        post(birthday, "Concert tickets or a kitchen knife?", ""),
        by: mia,
        at: 2026-06-26T18:00Z,
    )
    let garden = invoke(
        conversations.items.insert(Conversation(title: "Garden allotment")),
        by: leo,
        at: 2026-06-25T08:30Z,
    )
    invoke(post(garden, "The tomatoes need water by Thursday.", ""), by: leo, at: 2026-06-25T08:30Z)
    let running = invoke(
        conversations.items.insert(Conversation(title: "Running group")),
        by: ada,
        at: 2026-06-24T06:15Z,
    )
    invoke(
        post(running, "Long run on Sunday, fifteen kilometres.", ""),
        by: ada,
        at: 2026-06-24T06:15Z,
    )
    let importer = invoke(
        conversations.items.insert(Conversation(title: "Importer bugs")),
        by: leo,
        at: 2026-06-22T13:40Z,
    )
    invoke(
        post(importer, "The importer drops tables with merged cells.", ""),
        by: leo,
        at: 2026-06-22T13:40Z,
    )
    let recipes = invoke(
        conversations.items.insert(Conversation(title: "Recipes")),
        by: mia,
        at: 2026-06-20T17:55Z,
    )
    invoke(
        post(recipes, "The lentil soup needs more lemon than you think.", ""),
        by: mia,
        at: 2026-06-20T17:55Z,
    )
    let interviews = invoke(
        conversations.items.insert(Conversation(title: "Interview loop")),
        at: 2026-06-18T09:00Z,
    )
    invoke(
        post(interviews, "Ask about the last project they were proud of.", ""),
        at: 2026-06-18T09:00Z,
    )
    invoke(
        post(interviews, "I'll take the system design round.", ""),
        by: ada,
        at: 2026-06-18T09:20Z,
    )
    conversations.items.insert(Conversation(title: "Holiday in August"))
    plans
}

/// One conversation with a sum worked out inline.
scenario "a sum worked out inline" {
    let c = conversations.items.insert(Conversation(title: "Quick sums"))
    post(c, "What is a week of standups, at fifteen minutes each?", "")
    post(c, "Here it is in minutes:", "5 * 15")
}
