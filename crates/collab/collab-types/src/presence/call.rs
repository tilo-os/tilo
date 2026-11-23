//! The call a person is in, as their presence carries it beside how
//! available they are.

/// The voice channel a person is in, as their presence carries it beside
/// how available they are: the channel's id (what joining it names) and its
/// name (what the people here read, "In voice"). It rides in presence's one
/// `call` text as the id, a line break, then the name; a text without the
/// break is an id alone, its name not said. A bridge carrying the call to
/// another service says that service on a third line.
///
/// The session, a page and an app read the same shape: the toolkit re-exports
/// it (`ui::CallPresence`), so an app names it where it always has.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CallPresence {
    /// Chat's id of the voice channel.
    pub channel: String,
    /// What the channel is called, as the people in it say; empty when
    /// nobody named it.
    pub name: String,
    /// The service a bridge carries this call to (`discord`), said by the
    /// bridge's own device in the call; `None` for anyone else. A call view
    /// shows the call as bridged there, and the bridge as nobody's face.
    pub bridge: Option<String>,
}

/// Written as the one `call` text presence carries it in.
impl serde::Serialize for CallPresence {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.encode())
    }
}

impl<'de> serde::Deserialize<'de> for CallPresence {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
        Self::decode(&text).ok_or_else(|| serde::de::Error::custom("not a call"))
    }
}

impl CallPresence {
    /// Longest channel name carried; a longer one is cut.
    pub const MAX_NAME_CHARS: usize = 64;

    /// The `call` text that carries this.
    #[must_use]
    pub fn encode(&self) -> String {
        let name = clean_name(&self.name);
        match self.bridge.as_deref().map(clean_name) {
            Some(service) if !service.is_empty() => {
                format!("{}\n{name}\n{service}", self.channel)
            }
            _ if name.is_empty() => self.channel.clone(),
            _ => format!("{}\n{name}", self.channel),
        }
    }

    /// What a `call` text carries; `None` for an empty one.
    #[must_use]
    pub fn decode(call: &str) -> Option<Self> {
        let mut lines = call.splitn(3, '\n');
        let channel = lines.next().unwrap_or_default().trim();
        let name = lines.next().unwrap_or_default();
        let bridge = lines
            .next()
            .map(clean_name)
            .filter(|service| !service.is_empty());
        if channel.is_empty() || channel.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            channel: channel.to_owned(),
            name: clean_name(name),
            bridge,
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
            bridge: None,
        };
        assert_eq!(CallPresence::decode(&voice.encode()), Some(voice));
        assert_eq!(
            CallPresence::decode("c_voice"),
            Some(CallPresence {
                channel: "c_voice".into(),
                name: String::new(),
                bridge: None,
            })
        );
        // A bridge says where it carries the call, with or without a name.
        for name in ["voice", ""] {
            let bridged = CallPresence {
                channel: "c_voice".into(),
                name: name.into(),
                bridge: Some("discord".into()),
            };
            assert_eq!(CallPresence::decode(&bridged.encode()), Some(bridged));
        }
        assert_eq!(CallPresence::decode(""), None);
        let long = CallPresence {
            channel: "c".into(),
            name: "x".repeat(200),
            bridge: None,
        };
        assert_eq!(
            CallPresence::decode(&long.encode()).map(|call| call.name.chars().count()),
            Some(CallPresence::MAX_NAME_CHARS)
        );
    }
}
