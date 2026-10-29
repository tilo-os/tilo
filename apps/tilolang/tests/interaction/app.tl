app so.tilo.lang.interaction "Interaction" (icon: .message_circle, category: .network)

/// A message: what it says and the reactions on it. Who wrote it and when come from the log.
type Message {
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
    c.messages.insert(Message(body: body))
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
    std.transcript(c, c.messages, row: message) {
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
        post(c, "The new onboarding build is on the test machines.")
        post(c, "First launch is down to a second and a half on the old ThinkPad.")
    }
    as leo {
        post(c, "Can we move the account step to the end?")
    }
    post(c, "Agreed. I'll update the flow this afternoon.")
    c
}
