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

/// One conversation in a list: its title and how many messages it has.
view conversation_row(c: Conversation) {
    row(c.title, meta: "{c.messages.count(m -> true)} messages")
}

/// The titles of a chat's conversations, for showing inside another app's view.
view conversation_list(chat: Chat) {
    section("Chats") {
        stack(gap: .related) {
            for c in chat.conversations {
                button(c.title, on: open(c))
            }
        }
    }
}

/// A conversation's window: its messages, then the composer.
view thread(c: Conversation) {
    page {
        stack(gap: .section) {
            for m in c.messages {
                message(m)
            }
        }
        composer(c)
    }
}

/// One message: who wrote it, beside what they wrote.
view message(m: Message) {
    row(gap: .related, align: .start) {
        avatar(m.author)
        stack(gap: .tight, fill) {
            text(m.author.name(), role: .caption)
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

view composer(c: Conversation) {
    row {
        input(bind: c.draft, placeholder: "Message", fill)
        button(send_draft(c)) as send
    }
}

/// A workspace's conversations: enough to scroll, the first one under way.
scenario "a busy workspace" -> Conversation {
    let chat = root Chat
    let plans = chat.conversations.insert(Conversation(title: "Weekend plans"))
    post(plans, "Is everyone still up for the walk on Saturday?", "")
    post(plans, "I can bring sandwiches. The forecast says sun until three.", "")
    post(plans, "Let's meet at the station at ten.", "")
    chat.conversations.insert(Conversation(title: "Release checklist"))
    chat.conversations.insert(Conversation(title: "Flat hunting"))
    chat.conversations.insert(Conversation(title: "Book club: Piranesi"))
    chat.conversations.insert(Conversation(title: "Design review"))
    chat.conversations.insert(Conversation(title: "Sam's birthday"))
    chat.conversations.insert(Conversation(title: "Garden allotment"))
    chat.conversations.insert(Conversation(title: "Running group"))
    chat.conversations.insert(Conversation(title: "Importer bugs"))
    chat.conversations.insert(Conversation(title: "Holiday in August"))
    chat.conversations.insert(Conversation(title: "Recipes"))
    chat.conversations.insert(Conversation(title: "Interview loop"))
    plans
}

/// One conversation with a sum worked out inline.
scenario "a sum worked out inline" {
    let chat = root Chat
    let c = chat.conversations.insert(Conversation(title: "Quick sums"))
    post(c, "What is a week of standups, at fifteen minutes each?", "")
    post(c, "Here it is in minutes:", "5 * 15")
}
