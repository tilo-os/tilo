//! The call a person is in, as their presence carries it beside how
//! available they are.

/// The voice channel a person is in, as their presence carries it beside
/// how available they are: the channel's id (what joining it names) and its
/// name (what the people here read, "In voice"). It rides in presence's one
/// `call` text as the id, a line break, then the name; a text without the
/// break is an id alone, its name not said.
///
/// The session, a page and an app read the same shape: the toolkit re-exports
/// it (`ui::CallPresence`), so an app names it where it always has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallPresence {
    /// Chat's id of the voice channel.
    pub channel: String,
    /// What the channel is called, as the people in it say; empty when
    /// nobody named it.
    pub name: String,
}

impl CallPresence {
    /// Longest channel name carried; a longer one is cut.
    pub const MAX_NAME_CHARS: usize = 64;

    /// The `call` text that carries this.
    #[must_use]
    pub fn encode(&self) -> String {
        let name = clean_name(&self.name);
        if name.is_empty() {
            self.channel.clone()
        } else {
            format!("{}\n{name}", self.channel)
        }
    }

    /// What a `call` text carries; `None` for an empty one.
    #[must_use]
    pub fn decode(call: &str) -> Option<Self> {
        let (channel, name) = call.split_once('\n').unwrap_or((call, ""));
        let channel = channel.trim();
        if channel.is_empty() || channel.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            channel: channel.to_owned(),
            name: clean_name(name),
        })
    }
}

/// A channel name as a call carries it: no control characters, at most
/// [`CallPresence::MAX_NAME_CHARS`], trimmed.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .take(CallPresence::MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::CallPresence;

    /// A call carries its channel's id and name in one text, and reads back
    /// the same; an id alone reads with no name; an empty text is no call.
    #[test]
    fn a_call_carries_its_channel_id_and_name() {
        let voice = CallPresence {
            channel: "c_voice".into(),
            name: "voice".into(),
        };
        assert_eq!(CallPresence::decode(&voice.encode()), Some(voice));
        assert_eq!(
            CallPresence::decode("c_voice"),
            Some(CallPresence {
                channel: "c_voice".into(),
                name: String::new(),
            })
        );
        assert_eq!(CallPresence::decode(""), None);
        let long = CallPresence {
            channel: "c".into(),
            name: "x".repeat(200),
        };
        assert_eq!(
            CallPresence::decode(&long.encode()).map(|call| call.name.chars().count()),
            Some(CallPresence::MAX_NAME_CHARS)
        );
    }
}
