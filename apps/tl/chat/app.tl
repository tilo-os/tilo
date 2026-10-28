app so.tilo.lang.chat "Chat" (icon: .message_circle, category: .network)

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
    /// When it was posted.
    at: Time = Time("1970-01-01T00:00:00Z")
    parts: Collection<Part>
}

/// A conversation: its messages, in the order they were posted.
type Conversation {
    title: Text (title)
    messages: Collection<Message>
    /// The text being typed.
    draft: Text
}

/// The workspace's conversations.
type Chat {
    conversations: Collection<Conversation>
}

root Chat

default view chats for Chat

default view thread for Conversation

row view conversation_row for Conversation

/// Starts a conversation with a title.
action start(chat: Chat, title: Text) -> Conversation {
    chat.conversations.insert(Conversation(title: title))
}

/// Starts a conversation.
primary action new_chat(chat: Chat) -> Conversation (icon: .plus) {
    chat.conversations.insert(Conversation(title: "New chat"))
}

/// Posts a message as the invoker: a text part when `text` is not empty, then a block part
/// holding `block` as source when it is not empty. People and agents post the same way. A
/// block that applies an action is run once, by the host right after the post commits, as the
/// invoker; the outcome is recorded in the part.
action post(c: Conversation, text: Text, block: Text) -> Message {
    let m = c.messages.insert(Message(author: invoker, at: now))
    if text != "" {
        m.parts.insert(Part(kind: "text", body: text))
    }
    if block != "" {
        let p = m.parts.insert(Part(kind: "block", body: block))
        run_block(p, block)
    }
    m
}

/// Posts the typed text as the invoker and clears the entry.
action send_draft(c: Conversation) (icon: .send, label: "Send") {
    if c.draft != "" {
        post(c, c.draft, "")
        c.draft = ""
    }
}

/// The chat's window: its conversations, each opening in a window of its own.
view chats(chat: Chat) {
    std.list(chat, chat.conversations, empty: "No conversations yet")
}

/// What a message says in a line: its text, or that it holds a block.
fn said(m: Message) -> Text {
    match m.parts.first() {
        .some(p) => if p.kind == "text" { p.body } else { "Sent a block" },
        .none => "",
    }
}

/// One conversation in a list: its title, and the last message with who wrote it and when.
/// Its avatar is whoever wrote last; before anyone has, the group's.
view conversation_row(c: Conversation) {
    row(c.title, secondary: last_said(c), meta: last_at(c), avatar: last_author(c), icon: .users)
}

/// What the last message of `c` says, or that there is none yet.
fn last_said(c: Conversation) -> Text {
    match c.messages.last() {
        .some(m) => said(m),
        .none => "No messages yet",
    }
}

/// When the last message of `c` was posted.
fn last_at(c: Conversation) -> Option<Time> {
    match c.messages.last() {
        .some(m) => m.at,
        .none => .none,
    }
}

/// Who wrote the last message of `c`.
fn last_author(c: Conversation) -> Option<Principal> {
    match c.messages.last() {
        .some(m) => m.author,
        .none => .none,
    }
}

/// A conversation's window: its messages, the latest at the foot, above the composer.
view thread(c: Conversation) {
    std.transcript(c, c.messages, row: message, author: m -> m.author, time: m -> m.at) {
        composer: {
            row {
                input(bind: c.draft, placeholder: "Message", fill)
                button(send_draft(c), icon_only: true) as send
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
    let chat = root Chat
    let mia = person("Mia")
    let leo = person("Leo")
    let ada = person("Ada")
    let plans = chat.conversations.insert(Conversation(title: "Weekend plans"))
    as mia {
        let m1 = post(plans, "Is everyone still up for the walk on Saturday?", "")
        m1.at = Time("2026-06-29T07:02:00Z")
        let m2 = post(plans, "The long loop, round the reservoir and back over the hill.", "")
        m2.at = Time("2026-06-29T07:03:00Z")
    }
    as leo {
        let m3 = post(plans, "I can bring sandwiches. The forecast says sun until three.", "")
        m3.at = Time("2026-06-29T07:20:00Z")
    }
    let m4 = post(plans, "Count me in.", "")
    m4.at = Time("2026-06-29T07:24:00Z")
    as ada {
        let m5 = post(plans, "Let's meet at the station at ten.", "")
        m5.at = Time("2026-06-30T07:51:00Z")
        let m6 = post(plans, "Platform four, by the coffee stand.", "")
        m6.at = Time("2026-06-30T07:52:00Z")
    }
    let release = chat.conversations.insert(Conversation(title: "Release checklist"))
    as leo {
        let m7 = post(release, "Docs still need the new screenshots.", "")
        m7.at = Time("2026-06-30T06:40:00Z")
    }
    as ada {
        let m8 = post(release, "I'll take the screenshots after lunch.", "")
        m8.at = Time("2026-06-30T07:15:00Z")
    }
    let flat = chat.conversations.insert(Conversation(title: "Flat hunting"))
    let m9 = post(flat, "The one on Elm Street has a balcony.", "")
    m9.at = Time("2026-06-29T15:10:00Z")
    as mia {
        let m10 = post(flat, "Viewing is Saturday at eleven, bring the deposit papers.", "")
        m10.at = Time("2026-06-29T16:45:00Z")
    }
    let books = chat.conversations.insert(Conversation(title: "Book club: Piranesi"))
    as ada {
        let m11 = post(books, "Halfway through. The statues are wonderful.", "")
        m11.at = Time("2026-06-28T19:30:00Z")
    }
    let design = chat.conversations.insert(Conversation(title: "Design review"))
    as leo {
        let m12 = post(design, "The sidebar collapses too early at large text.", "")
        m12.at = Time("2026-06-27T10:05:00Z")
    }
    let m13 = post(design, "Agreed, I'll look at the thresholds.", "")
    m13.at = Time("2026-06-27T10:12:00Z")
    let birthday = chat.conversations.insert(Conversation(title: "Sam's birthday"))
    as mia {
        let m14 = post(birthday, "Concert tickets or a kitchen knife?", "")
        m14.at = Time("2026-06-26T18:00:00Z")
    }
    let garden = chat.conversations.insert(Conversation(title: "Garden allotment"))
    as leo {
        let m15 = post(garden, "The tomatoes need water by Thursday.", "")
        m15.at = Time("2026-06-25T08:30:00Z")
    }
    let running = chat.conversations.insert(Conversation(title: "Running group"))
    as ada {
        let m16 = post(running, "Long run on Sunday, fifteen kilometres.", "")
        m16.at = Time("2026-06-24T06:15:00Z")
    }
    let importer = chat.conversations.insert(Conversation(title: "Importer bugs"))
    as leo {
        let m17 = post(importer, "The importer drops tables with merged cells.", "")
        m17.at = Time("2026-06-22T13:40:00Z")
    }
    let recipes = chat.conversations.insert(Conversation(title: "Recipes"))
    as mia {
        let m18 = post(recipes, "The lentil soup needs more lemon than you think.", "")
        m18.at = Time("2026-06-20T17:55:00Z")
    }
    let interviews = chat.conversations.insert(Conversation(title: "Interview loop"))
    let m19 = post(interviews, "Ask about the last project they were proud of.", "")
    m19.at = Time("2026-06-18T09:00:00Z")
    as ada {
        let m20 = post(interviews, "I'll take the system design round.", "")
        m20.at = Time("2026-06-18T09:20:00Z")
    }
    chat.conversations.insert(Conversation(title: "Holiday in August"))
    plans
}

/// One conversation with a sum worked out inline.
scenario "a sum worked out inline" {
    let chat = root Chat
    let c = chat.conversations.insert(Conversation(title: "Quick sums"))
    post(c, "What is a week of standups, at fifteen minutes each?", "")
    post(c, "Here it is in minutes:", "5 * 15")
}
