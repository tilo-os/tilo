//! What a resource is, in the words people use for it: its type's noun, the
//! name of the app whose type it is, and whether it is that app's root.
//!
//! [`ResourceType`] is what search matches a query naming *what* something is
//! against ("my mails", "notes", "youtube videos of mkbdc"), rather than its
//! title. A tilolang type publishes one from its declaration; a Rust app's
//! `#[resource]` type publishes the same one
//! ([`tilo_resource::words::type_words`]), so the one search names either
//! alike. A noun English cannot derive ("people" for a `Person`) is the
//! application catalogue's to refine, never a list of irregulars in code.

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
    /// several are: its name in words, lower case, and the plural English
    /// writes for most nouns (`Channel` is `channel` and `channels`,
    /// `WatchHistory` is `watch history` and `watch histories`).
    ///
    /// A word no rule derives ("people" for a `Person`) is the source
    /// catalogue's to say, and a type names its own words there where this is
    /// wrong: the code never keeps a list of irregulars.
    #[must_use]
    pub fn of_type(name: &str) -> Self {
        let one = in_words(name);
        let many = plural(&one);
        Self { one, many }
    }
}

/// A declared name in words, lower case: `TodoList` and `todo_list` are
/// `todo list`.
#[must_use]
pub fn in_words(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' || ch == '-' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.join(" ")
}

/// What several of `noun` are called, as English writes most: `notes`,
/// `entries`, `boxes`. A `y` after a consonant is `ies`; `s`, `x`, `z`, `ch`
/// and `sh` take `es`; anything else takes `s`.
#[must_use]
pub fn plural(noun: &str) -> String {
    let consonant_y = noun.strip_suffix('y').is_some_and(|stem| {
        stem.chars()
            .last()
            .is_some_and(|c| !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
    });
    if consonant_y {
        format!("{}ies", noun.strip_suffix('y').unwrap_or(noun))
    } else if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|end| noun.ends_with(end))
    {
        format!("{noun}es")
    } else {
        format!("{noun}s")
    }
}

/// What one of several called `noun` is called, as English writes most:
/// `notes` is a `note`, `entries` an `entry`, `boxes` a `box`. The reverse of
/// [`plural`] for the words it makes, and of what a `Collection<…>` field is
/// named (`videos`, whose items are a `video`); a noun already singular reads
/// as it is (`history`), and one no rule derives keeps its name.
#[must_use]
pub fn singular(noun: &str) -> String {
    if let Some(stem) = noun.strip_suffix("ies")
        && !stem.is_empty()
    {
        return format!("{stem}y");
    }
    if let Some(stem) = noun.strip_suffix("es")
        && ["s", "x", "z", "ch", "sh"]
            .iter()
            .any(|end| stem.ends_with(end))
    {
        return stem.to_owned();
    }
    if let Some(stem) = noun.strip_suffix('s')
        && !stem.is_empty()
        && !stem.ends_with('s')
    {
        return stem.to_owned();
    }
    noun.to_owned()
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
        assert_eq!(in_words("TodoList"), "todo list");
        assert_eq!(in_words("file_system"), "file system");
        assert_eq!(in_words("WatchHistory"), "watch history");
        assert_eq!(Noun::of_type("Channel"), Noun::new("channel", "channels"));
        assert_eq!(Noun::of_type("Entry"), Noun::new("entry", "entries"));
        assert_eq!(Noun::of_type("Box"), Noun::new("box", "boxes"));
        assert_eq!(
            Noun::of_type("Mailbox"),
            Noun::new("mailbox", "mailboxes"),
            "a `box` takes `es`"
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
