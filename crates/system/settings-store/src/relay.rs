//! The relay this device's endpoints use, as the person set it (`network.relay`).
//!
//! Unset, a device relays through number 0's public relays and finds its peers on number 0's
//! directory, as it always has. `"none"` uses no relay at all: direct and local-network
//! connections only. Any other value is one relay URL of the person's own, and the device uses
//! that relay alone, with number 0's DNS off, so nothing it does depends on number 0.
//!
//! Every reader of the setting is an endpoint's construction
//! ([`tilo_endpoint_reachability::relays`]), so this module only says what the person chose; it
//! holds no relay of its own and no iroh type.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};

/// The settings document's section.
pub const SECTION: &str = "network";

/// The setting's leaf in [`SECTION`]: `"default"`, `"none"`, or a relay URL.
pub const RELAY_KEY: &str = "relay";

/// What the person's setting says this device's endpoints relay through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Relays {
    /// Number 0's public relays and its directory: what a device does unless the person said
    /// otherwise.
    #[default]
    N0,
    /// No relay at all: direct and local-network connections only.
    None,
    /// One relay of the person's own, and no number 0 DNS.
    Own(String),
}

impl Relays {
    /// What the setting stores for this choice: `"default"`, `"none"`, or the relay's URL.
    #[must_use]
    pub fn stored(&self) -> &str {
        match self {
            Self::N0 => "default",
            Self::None => "none",
            Self::Own(url) => url,
        }
    }

    /// What `text` chooses: `"none"` for no relay, `"default"` (or nothing) for number 0's, and
    /// anything else as the one relay URL it names. A URL that does not parse is the endpoint's
    /// to refuse; it reads as [`Self::N0`] there rather than leaving a device unreachable.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        if text.eq_ignore_ascii_case("none") {
            Self::None
        } else if text.is_empty() || text.eq_ignore_ascii_case("default") {
            Self::N0
        } else {
            Self::Own(text.to_owned())
        }
    }
}

impl Serialize for Relays {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.stored())
    }
}

/// Only a string chooses: a stored `true` or `3` is not a relay, and reads as the default.
impl<'de> Deserialize<'de> for Relays {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Self::parse(&text))
    }
}

/// This device's relay setting, as the person has it.
pub const RELAY: crate::Key<Relays> = crate::Key::new(SECTION, RELAY_KEY, || Relays::N0);

/// The person's choice now: [`Relays::N0`] until they say otherwise.
#[must_use]
pub fn relays() -> Relays {
    RELAY.get()
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::{RELAY, Relays};

    #[test]
    fn a_device_relays_through_number_0_until_the_person_says_otherwise() {
        assert_eq!(Relays::parse(""), Relays::N0);
        assert_eq!(Relays::parse("default"), Relays::N0);
        assert_eq!(Relays::parse(" None "), Relays::None);
        assert_eq!(
            Relays::parse("https://relay.example.com."),
            Relays::Own("https://relay.example.com.".into())
        );
    }

    #[test]
    fn the_setting_reads_through_the_document_and_stores_what_each_choice_means() {
        assert_eq!(RELAY.from_document(&Map::new()), Relays::N0);
        let mut document = Map::new();
        document.insert("network".into(), json!({ "relay": "none" }));
        assert_eq!(RELAY.from_document(&document), Relays::None);
        document.insert(
            "network".into(),
            json!({ "relay": "https://relay.example.com." }),
        );
        assert_eq!(
            RELAY.from_document(&document),
            Relays::Own("https://relay.example.com.".into())
        );
        document.insert("network".into(), json!({ "relay": true }));
        assert_eq!(
            RELAY.from_document(&document),
            Relays::N0,
            "a truth value is not a relay"
        );
        for choice in [
            Relays::N0,
            Relays::None,
            Relays::Own("https://relay.example.com.".into()),
        ] {
            assert_eq!(Relays::parse(choice.stored()), choice);
        }
    }
}
