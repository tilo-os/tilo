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
}

/// The workspace's conversations.
type Conversations = Collection<Conversation>

root conversations: Conversations

default view chats for conversations

default view thread for Conversation

row view conversation_row for Conversation

/// Starts a conversation with a title.
action start(all: Conversations, title: Text) -> Conversation {
    all.insert(Conversation(title: title))
}

/// Starts a conversation.
primary action new_chat(all: Conversations) -> Conversation (icon: .plus) {
    all.insert(Conversation(title: "New chat"))
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
view chats(all: Conversations) {
    std.list(all, all.sort_by(c -> updated(c), desc), empty: "No conversations yet")
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
    lens draft: Text
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
    let plans = conversations.insert(Conversation(title: "Weekend plans"))
    as mia at 2026-06-29T07:02Z {
        post(plans, "Is everyone still up for the walk on Saturday?", "")
    }
    as mia at 2026-06-29T07:03Z {
        post(plans, "The long loop, round the reservoir and back over the hill.", "")
    }
    as leo at 2026-06-29T07:20Z {
        post(plans, "I can bring sandwiches. The forecast says sun until three.", "")
    }
    as invoker at 2026-06-29T07:24Z {
        post(plans, "Count me in.", "")
    }
    as ada at 2026-06-30T07:51Z {
        post(plans, "Let's meet at the station at ten.", "")
    }
    as ada at 2026-06-30T07:52Z {
        post(plans, "Platform four, by the coffee stand.", "")
    }
    // Each conversation is started when its first message is posted, so the list's order,
    // the latest activity first, is that of their last messages.
    as leo at 2026-06-30T06:40Z {
        let release = conversations.insert(Conversation(title: "Release checklist"))
        post(release, "Docs still need the new screenshots.", "")
        as ada at 2026-06-30T07:15Z {
            post(release, "I'll take the screenshots after lunch.", "")
        }
    }
    as invoker at 2026-06-29T15:10Z {
        let flat = conversations.insert(Conversation(title: "Flat hunting"))
        post(flat, "The one on Elm Street has a balcony.", "")
        as mia at 2026-06-29T16:45Z {
            post(flat, "Viewing is Saturday at eleven, bring the deposit papers.", "")
        }
    }
    as ada at 2026-06-28T19:30Z {
        let books = conversations.insert(Conversation(title: "Book club: Piranesi"))
        post(books, "Halfway through. The statues are wonderful.", "")
    }
    as leo at 2026-06-27T10:05Z {
        let design = conversations.insert(Conversation(title: "Design review"))
        post(design, "The sidebar collapses too early at large text.", "")
        as invoker at 2026-06-27T10:12Z {
            post(design, "Agreed, I'll look at the thresholds.", "")
        }
    }
    as mia at 2026-06-26T18:00Z {
        let birthday = conversations.insert(Conversation(title: "Sam's birthday"))
        post(birthday, "Concert tickets or a kitchen knife?", "")
    }
    as leo at 2026-06-25T08:30Z {
        let garden = conversations.insert(Conversation(title: "Garden allotment"))
        post(garden, "The tomatoes need water by Thursday.", "")
    }
    as ada at 2026-06-24T06:15Z {
        let running = conversations.insert(Conversation(title: "Running group"))
        post(running, "Long run on Sunday, fifteen kilometres.", "")
    }
    as leo at 2026-06-22T13:40Z {
        let importer = conversations.insert(Conversation(title: "Importer bugs"))
        post(importer, "The importer drops tables with merged cells.", "")
    }
    as mia at 2026-06-20T17:55Z {
        let recipes = conversations.insert(Conversation(title: "Recipes"))
        post(recipes, "The lentil soup needs more lemon than you think.", "")
    }
    as invoker at 2026-06-18T09:00Z {
        let interviews = conversations.insert(Conversation(title: "Interview loop"))
        post(interviews, "Ask about the last project they were proud of.", "")
        as ada at 2026-06-18T09:20Z {
            post(interviews, "I'll take the system design round.", "")
        }
    }
    conversations.insert(Conversation(title: "Holiday in August"))
    plans
}

/// One conversation with a sum worked out inline.
scenario "a sum worked out inline" {
    let c = conversations.insert(Conversation(title: "Quick sums"))
    post(c, "What is a week of standups, at fifteen minutes each?", "")
    post(c, "Here it is in minutes:", "5 * 15")
}
