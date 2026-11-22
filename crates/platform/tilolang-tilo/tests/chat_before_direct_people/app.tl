app so.tilo.chat "Chat" (icon: .message_circle, category: .network)

/// The Chat whose direct conversation kept only the one person it was opened with, and so was
/// read by every member: what the Chat of direct conversations read by their people alone
/// migrates.
type Conversations {
    channels: Collection<Category | TextChannel | Direct>
}

root conversations: Conversations

type Category {
    name: Text
    channels: Collection<TextChannel>
}

type TextChannel {
    name: Text
    topic: Text
    messages: Collection<Message>
    threads: Collection<Thread>
}

type Message {
    body: RichText
}

type Thread {
    name: Text
    messages: Collection<Message>
}

type Direct {
    name: Text
    person: Actor
    messages: Collection<Message>
    threads: Collection<Thread>
}

default view sidebar for Conversations

/// Opens the direct message with `who`.
action message_person(all: Conversations, who: Actor) -> Direct {
    let d = all.channels.insert(Direct(name: who.name(), person: who))
    d.name = who.name()
    d.person = who
    d
}

/// Posts a message in a direct conversation.
action send(to: Direct, body: RichText) -> Message {
    to.messages.insert(Message(body: body))
}

view sidebar(all: Conversations) {
    stack {
        for item in all.channels {
            match item {
                c: Category => text(c.name),
                ch: TextChannel => text(ch.name),
                d: Direct => text(d.name),
            }
        }
    }
}
