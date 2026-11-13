//! What a resource is, in the words people use for it: its type's noun, the
//! name of the app whose type it is, and whether it is that app's root.
//!
//! [`ResourceType`] is what search matches a query naming *what* something is
//! against ("my mails", "notes", "youtube videos of mkbdc"), rather than its
//! title. A tilolang type publishes one from its declaration; a Rust app's
//! `#[resource]` type publishes the same one
//! ([`tilo_resource::words::type_words`]), so the one search names either
//! alike. The words are [`tilo_words`] for both: a type is called by its own
//! name (`People` is one `person`, several `people`), and a noun English does
//! not derive that an app words otherwise (a `Note` it calls a `notebook`) is
//! the app's catalogue's to say.

pub use tilo_words::{in_words, plural, singular};

/// What one of something is called and what several are, in a sentence:
/// `mailbox` and `mailboxes`, `person` and `people`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Noun {
    pub one: String,
    pub many: String,
}

impl Noun {
    #[must_use]
    pub fn new(one: impl Into<String>, many: impl Into<String>) -> Self {
        Self {
            one: one.into(),
            many: many.into(),
        }
    }

    /// What one of a type called `name` is called in a sentence, and what
    /// several are: its name in words, lower case, in the singular and in the
    /// plural (`Channel` is `channel` and `channels`, `WatchHistory` is
    /// `watch history` and `watch histories`, `Tasks` is `task` and `tasks`).
    ///
    /// A noun an app words otherwise (a `Note` it calls a `notebook`) is the
    /// source catalogue's to say.
    #[must_use]
    pub fn of_type(name: &str) -> Self {
        let words = in_words(name);
        Self {
            many: plural(&words),
            one: singular(&words),
        }
    }
}

/// What a resource is, in the words people use for it: its type's noun, the
/// name of the app whose type it is, and whether it is that app's root.
/// What search matches a query naming what something is ("my mails",
/// "notes") against, rather than its title.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ResourceType {
    /// Its type: `mailbox`, `mailboxes`.
    pub noun: Noun,
    /// Its app, as people call it: `Mail`, `mails`.
    pub app: Noun,
    /// The app's root: its one resource of the type, which a search naming
    /// the app means ("my mails" is Mail's root, drawn as its default view).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub root: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_name_reads_as_words_and_its_plural() {
        assert_eq!(Noun::of_type("Channel"), Noun::new("channel", "channels"));
        assert_eq!(Noun::of_type("Entry"), Noun::new("entry", "entries"));
        assert_eq!(Noun::of_type("Box"), Noun::new("box", "boxes"));
        assert_eq!(
            Noun::of_type("Mailbox"),
            Noun::new("mailbox", "mailboxes"),
            "a `box` takes `es`"
        );
        assert_eq!(
            Noun::of_type("Person"),
            Noun::new("person", "people"),
            "an irregular plural is English's, not an app's"
        );
        assert_eq!(
            Noun::of_type("Conversations"),
            Noun::new("conversation", "conversations"),
            "a name that already names several is one of them and all of them"
        );
    }

    #[test]
    fn a_plural_reads_back_to_its_singular() {
        for (many, one) in [
            ("notes", "note"),
            ("entries", "entry"),
            ("boxes", "box"),
            ("channels", "channel"),
            ("history", "history"),
        ] {
            assert_eq!(singular(many), one, "{many} is several {one}");
        }
    }
}
