//! The catalogue keys a resource kind's words are stored under.

/// The catalogue key a kind's name is stored under: the kind's own spelling
/// with its separators as dashes, so `add-app.question` reads as
/// `add-app-question`. Kinds are unique across the system, so the key names
/// one resource type.
#[must_use]
pub fn message_key(kind: &str) -> String {
    kind.replace(['.', '_', ':'], "-")
}

/// The catalogue key a kind's noun is stored under: [`message_key`] and
/// `-noun`, one message with `$count` (`{ $count -> [one] person *[other]
/// people }`). A type whose name English cannot pluralise writes it; a
/// catalogue that does not hold it leaves the derived noun.
#[must_use]
pub fn noun_key(kind: &str) -> String {
    format!("{}-noun", message_key(kind))
}
