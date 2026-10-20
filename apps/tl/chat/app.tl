app so.tilo.chat "Chat"

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
    return m
}

/// Posts the typed text as the invoker and clears the entry.
action send_draft(c: Conversation) {
    if c.draft != "" {
        post(c, c.draft, "")
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
            text(m.author.name(), style: "caption")
            parts(m)
        }
    } else {
        row(gap: related, align: start) {
            text(m.author.name(), style: "caption", min: 8rem)
            parts(m)
        }
    }
}

view parts(m: Message) {
    stack(gap: related) {
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
        button("Send", on: send_draft(c)) #send
    }
}
