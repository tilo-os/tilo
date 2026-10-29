app so.tilo.test.chatreader "Chat reader"

use so.tilo.chat as chat

/// How a channel reads in the list: a voice channel says so.
fn channel_label(c: chat.Channel) -> Text {
    if c.kind == .voice { "{c.name} (voice)" } else { "#{c.name}" }
}

/// The messages of the channel `id`, oldest first.
fn said_in(m: chat.Messages, id: Text) -> List<chat.Message> {
    m.messages.filter(msg -> msg.channel == id)
}

/// Opens a voice channel in the room: a call of Chat's own action.
flow open_voice(w: chat.Workspace, name: Text) {
    chat.create_channel(w, name, .none, chat.ChannelKind.voice, "Talk")
}

view channels(w: chat.Workspace, m: chat.Messages) {
    page {
        section("Channels") {
            for c in w.channels {
                text(channel_label(c))
                for msg in said_in(m, c.name) {
                    text(msg.text, role: .caption)
                }
            }
        }
        section("People") {
            for p in w.people {
                text("{p.name}: {p.color}")
            }
        }
    }
}
