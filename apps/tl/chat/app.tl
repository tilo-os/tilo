app so.tilo.chat "Chat"

/// One piece of a message: plain text, or the source of a block that renders inline.
type Part {
    /// `text` or `block`.
    kind: Text merge register
    body: Text
}

/// A message is a list of parts and the principal that wrote it: a person, or an agent task.
/// The author is also the code author of the message's blocks.
type Message {
    author: Text merge register
    parts: Collection<Part>
}

type Conversation {
    title: Text
    messages: Collection<Message>
    /// The text being typed.
    draft: Text
}

type Chat {
    conversations: Collection<Conversation>
    open: Option<Conversation>
}

/// Starts a conversation and opens it.
action start(chat: Chat, title: Text) -> Conversation {
    let c = chat.conversations.insert(Conversation(title: title))
    chat.open = c
    return c
}

/// Opens a conversation.
action open_conversation(chat: Chat, c: Conversation) {
    chat.open = c
}

/// Posts a message as `author`: a text part when `text` is not empty, then a block part
/// holding `block` as source when it is not empty. People and agents post the same way.
action post(c: Conversation, author: Text, text: Text, block: Text) -> Message {
    let m = c.messages.insert(Message(author: author))
    if text != "" {
        m.parts.insert(Part(kind: "text", body: text))
    }
    if block != "" {
        m.parts.insert(Part(kind: "block", body: block))
    }
    return m
}

/// Posts the typed text and clears the entry.
action send_draft(c: Conversation, author: Text) {
    if c.draft != "" {
        post(c, author, c.draft, "")
        c.draft = ""
    }
}

view main(chat: Chat) {
    page(full) {
        split(side: leading, collapse: navigate) {
            primary(min: 14rem) {
                conversation_list(chat)
            }
            secondary(min: 24rem) {
                if chat.open.is_some() {
                    thread(chat.open.unwrap())
                }
            }
        }
    }
}

view conversation_list(chat: Chat) {
    stack {
        row(distribute: between) {
            heading("Chats")
            button("New chat", on: start(chat, "New chat")) #new
        }
        scroll {
            for c in chat.conversations {
                button(c.title, on: open_conversation(chat, c))
            }
        }
    }
}

view thread(c: Conversation) {
    stack(gap: section) {
        input(bind: c.title, placeholder: "Title")
        scroll(fill) {
            stack(gap: sibling) {
                for m in c.messages {
                    message(m)
                }
            }
        }
        composer(c)
    }
}

view message(m: Message) {
    if space.compact {
        stack(gap: tight) {
            text(m.author, style: "caption")
            parts(m)
        }
    } else {
        row(gap: related, align: start) {
            text(m.author, style: "caption", min: 8rem)
            parts(m)
        }
    }
}

view parts(m: Message) {
    stack(gap: related) {
        for p in m.parts {
            if p.kind == "block" {
                embed(p.body, author: m.author)
            } else {
                text(p.body)
            }
        }
    }
}

view composer(c: Conversation) {
    row {
        input(bind: c.draft, placeholder: "Message", fill)
        button("Send", on: send_draft(c, "person")) #send
    }
}
