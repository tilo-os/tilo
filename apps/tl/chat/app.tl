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
    list(chat.conversations, empty: "No conversations yet")
}

/// What a message says in a line: its text, or that it holds a block.
fn said(m: Message) -> Text {
    match m.parts.first() {
        .some(p) => if p.kind == "text" { p.body } else { "Sent a block" },
        .none => "",
    }
}

/// One conversation in a list: its title, and the last message with who wrote it and when.
view conversation_row(c: Conversation) {
    row(c.title, secondary: last_said(c), meta: last_at(c), avatar: last_author(c))
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
    page {
        stack(gap: .section) {
            for m in c.messages {
                message(m)
            }
        }
        composer {
            row {
                input(bind: c.draft, placeholder: "Message", fill)
                button(send_draft(c)) as send
            }
        }
    }
}

/// One message: who wrote it, beside what they wrote.
view message(m: Message) {
    row(gap: .related, align: .start) {
        avatar(m.author)
        stack(gap: .tight, fill) {
            row(gap: .related) {
                text(m.author.name(), role: .caption)
                text(m.at, role: .caption)
            }
            parts(m)
        }
    }
}

view parts(m: Message) {
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
        post(plans, "Is everyone still up for the walk on Saturday?", "")
    }
    as leo {
        post(plans, "I can bring sandwiches. The forecast says sun until three.", "")
    }
    post(plans, "Count me in.", "")
    as ada {
        post(plans, "Let's meet at the station at ten.", "")
    }
    let release = chat.conversations.insert(Conversation(title: "Release checklist"))
    as leo {
        post(release, "Docs still need the new screenshots.", "")
    }
    as ada {
        post(release, "I'll take the screenshots after lunch.", "")
    }
    let flat = chat.conversations.insert(Conversation(title: "Flat hunting"))
    post(flat, "The one on Elm Street has a balcony.", "")
    as mia {
        post(flat, "Viewing is Saturday at eleven, bring the deposit papers.", "")
    }
    let books = chat.conversations.insert(Conversation(title: "Book club: Piranesi"))
    as ada {
        post(books, "Halfway through. The statues are wonderful.", "")
    }
    let design = chat.conversations.insert(Conversation(title: "Design review"))
    as leo {
        post(design, "The sidebar collapses too early at large text.", "")
    }
    post(design, "Agreed, I'll look at the thresholds.", "")
    let birthday = chat.conversations.insert(Conversation(title: "Sam's birthday"))
    as mia {
        post(birthday, "Concert tickets or a kitchen knife?", "")
    }
    let garden = chat.conversations.insert(Conversation(title: "Garden allotment"))
    as leo {
        post(garden, "The tomatoes need water by Thursday.", "")
    }
    let running = chat.conversations.insert(Conversation(title: "Running group"))
    as ada {
        post(running, "Long run on Sunday, fifteen kilometres.", "")
    }
    let importer = chat.conversations.insert(Conversation(title: "Importer bugs"))
    as leo {
        post(importer, "The importer drops tables with merged cells.", "")
    }
    let recipes = chat.conversations.insert(Conversation(title: "Recipes"))
    as mia {
        post(recipes, "The lentil soup needs more lemon than you think.", "")
    }
    let interviews = chat.conversations.insert(Conversation(title: "Interview loop"))
    post(interviews, "Ask about the last project they were proud of.", "")
    as ada {
        post(interviews, "I'll take the system design round.", "")
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
