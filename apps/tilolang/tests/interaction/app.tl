app so.tilo.lang.interaction "Interaction" (icon: .message_circle, category: .network)

/// A message: what it says, who wrote it and when, and the reactions on it.
type Message {
    author: Principal
    at: Time = Time("1970-01-01T00:00:00Z")
    body: Text (body)
    pinned: Bool = false
    thumbs: Int merge counter
    /// The emoji reacted with last.
    reacted: Text
}

/// One conversation.
type Channel {
    name: Text (title)
    messages: Collection<Message>
}

root Channel

default view channel for Channel

/// Posts a message.
action post(c: Channel, body: Text) -> Message (icon: .send) {
    c.messages.insert(Message(author: invoker, at: now, body: body))
}

/// Reacts with an emoji.
action react(m: Message, emoji: Text = "👍") (icon: .thumbs_up) {
    m.thumbs += 1
    m.reacted = emoji
}

/// Pins or unpins a message.
action pin(m: Message, pinned: Bool = not m.pinned) (icon: .pin, on: m.pinned) {
    m.pinned = pinned
}

/// Marks a message read.
action mark_read(m: Message) (icon: .check) {
    m.reacted = ""
}

/// Takes back what a message says.
destructive action retract(m: Message) (icon: .trash) {
    m.body = ""
}

/// The channel: its messages above a composer.
view channel(c: Channel) {
    lens draft: Text
    std.transcript(c, c.messages, row: message, author: m -> m.author, time: m -> m.at) {
        composer: {
            input(bind: draft, placeholder: "Message {c.name}")
        }
    }
}

/// One message, with what can be done to it on hover, swipe and right-click.
view message(m: Message) {
    text(m.body)
    if m.pinned {
        text("Pinned", role: .caption)
    }
    ui.HoverToolbar {
        ui.Button("👍", icon_only: false) {
            on press: react(m)
        }
        button(pin(m), icon_only: true)
    }
    ui.Swipe {
        leading: {
            ui.SwipeAction("Thumbs up", icon: .thumbs_up) {
                on swipe: react(m)
            }
        }
        trailing: {
            ui.SwipeAction("Pin", icon: .pin) {
                on swipe: pin(m)
            }
        }
    }
    ui.ContextMenu {
        ui.EmojiBar(["👍", "🎉", "❤️", "😂"], more: true) {
            on pick(e): react(m, e)
        }
        ui.MenuSeparator()
        ui.MenuItem("Pin", icon: .pin) {
            on choose: pin(m)
        }
    }
}

/// A conversation under way.
scenario "a conversation" -> Channel {
    let c = root Channel
    c.name = "design"
    let mia = person("Mia")
    let leo = person("Leo")
    as mia {
        let m1 = post(c, "The new onboarding build is on the test machines.")
        m1.at = Time("2026-06-29T07:02:00Z")
        let m2 = post(c, "First launch is down to a second and a half on the old ThinkPad.")
        m2.at = Time("2026-06-29T07:03:00Z")
    }
    as leo {
        let m3 = post(c, "Can we move the account step to the end?")
        m3.at = Time("2026-06-29T07:20:00Z")
    }
    let m4 = post(c, "Agreed. I'll update the flow this afternoon.")
    m4.at = Time("2026-06-29T07:24:00Z")
    c
}
