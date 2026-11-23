//! What a declared name is called in a sentence: its words, one of it and several.
//!
//! A type's noun is its name in words (`TodoList` is `todo list`) and its plural what English
//! writes for it (`todo lists`). A name that already names several — a collection, a page of
//! many things — is the many it is (`Tasks` is one `task`, several `tasks`), and a word English
//! counts no number of (`data`, `feedback`, `news`, `today`) keeps one form for both.
//!
//! The rules are English's own: a consonant before `y` takes `ies`, a sibilant (`s`, `x`, `z`,
//! `ch`, `sh`) takes `es`, and a word's plural that no rule makes is in [`IRREGULAR`]. A noun an
//! app says otherwise is a word about that app, not about English, and is the app's catalogue's
//! to write (`d-<id>.noun` in `locales/en-US.ftl`): a `Note` an app calls a `notebook`.

/// The fewest letters a word keeps when its `s` is read as a plural ending: `gas` is no `ga`.
const SHORTEST_STEM: usize = 3;

/// Words English counts no number of, or counts the same either way: the one and the several are
/// one word.
const SAME: &str = "\
advice aircraft bison cod data deer equipment evidence feedback fish furniture information \
jeans knowledge luggage means media metadata money moose music news offspring police research \
rice salmon series sheep software species staff swine today traffic trout weather wheat wildlife";

/// Whether English counts no number of `word`.
fn same(word: &str) -> bool {
    SAME.split(' ').any(|one| one == word)
}

/// The plurals English keeps of their own, by the one they are several of.
const IRREGULAR: &[(&str, &str)] = &[
    ("analysis", "analyses"),
    ("basis", "bases"),
    ("child", "children"),
    ("crisis", "crises"),
    ("criterion", "criteria"),
    ("die", "dice"),
    ("foot", "feet"),
    ("goose", "geese"),
    ("man", "men"),
    ("mouse", "mice"),
    ("ox", "oxen"),
    ("person", "people"),
    ("phenomenon", "phenomena"),
    ("thesis", "theses"),
    ("tooth", "teeth"),
    ("woman", "women"),
];

/// A declared name in words, lower case: `TodoList` and `todo_list` are `todo list`.
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

/// What several of `noun` are called: `notes`, `entries`, `boxes`, `people`. A noun that already
/// names several keeps the form it is (`tasks`), and so does one English counts no number of.
#[must_use]
pub fn plural(noun: &str) -> String {
    inflected(noun, many)
}

/// What one of `noun` is called: `note` for `notes`, `entry` for `entries`, `person` for
/// `people`, `task` for `tasks`. A noun that names one keeps the form it is.
#[must_use]
pub fn singular(noun: &str) -> String {
    inflected(noun, one)
}

/// `noun` with its last word inflected by `form`: a phrase is several by its last word
/// (`shell parts` is several `shell part`s).
fn inflected(noun: &str, form: fn(&str) -> String) -> String {
    let start = noun.rfind(' ').map_or(0, |space| space.saturating_add(1));
    let (before, word) = noun.split_at(start);
    format!("{before}{}", form(word))
}

/// What one `word` is called.
fn one(word: &str) -> String {
    if same(word) {
        return word.to_owned();
    }
    if let Some((one, _)) = IRREGULAR.iter().find(|(_, many)| *many == word) {
        return (*one).to_owned();
    }
    singular_stem(word).unwrap_or_else(|| word.to_owned())
}

/// What several `word` are called.
fn many(word: &str) -> String {
    if same(word) {
        return word.to_owned();
    }
    if let Some((_, many)) = IRREGULAR.iter().find(|(one, _)| *one == word) {
        return (*many).to_owned();
    }
    if IRREGULAR.iter().any(|(_, many)| *many == word) || singular_stem(word).is_some() {
        return word.to_owned();
    }
    written(word)
}

/// What English pluralizes `word` from, when `word` is that plural: `tasks` is `task`, `entries`
/// an `entry`, `boxes` a `box`. Nothing for a word no rule reverses, and nothing when the plural
/// English writes back differs from this word, so `class` and `status` stay what they are.
fn singular_stem(word: &str) -> Option<String> {
    if let Some(stem) = word.strip_suffix("ies") {
        let one = format!("{stem}y");
        return (written(&one) == word).then_some(one);
    }
    if let Some(stem) = word.strip_suffix("es")
        && sibilant(stem)
    {
        return Some(stem.to_owned());
    }
    if ["ss", "us", "is"].iter().any(|end| word.ends_with(end)) {
        return None;
    }
    let stem = word.strip_suffix('s')?;
    if stem.chars().count() < SHORTEST_STEM {
        return None;
    }
    (written(stem) == word).then(|| stem.to_owned())
}

/// The plural English writes with its rules: a consonant before `y` takes `ies` (`entry` is
/// `entries`), a sibilant takes `es` (`box` is `boxes`), and anything else `s` (`note` is
/// `notes`).
fn written(word: &str) -> String {
    let consonant_y = word.strip_suffix('y').is_some_and(|stem| {
        stem.chars()
            .last()
            .is_some_and(|c| !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
    });
    if consonant_y {
        format!("{}ies", word.strip_suffix('y').unwrap_or(word))
    } else if sibilant(word) {
        format!("{word}es")
    } else {
        format!("{word}s")
    }
}

/// Whether `word` ends in a sound English spells with `es`: `s`, `x`, `z`, `ch` or `sh`.
fn sibilant(word: &str) -> bool {
    ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|end| word.ends_with(end))
}

#[cfg(test)]
mod tests {
    use super::{in_words, plural, singular};

    #[test]
    fn a_declared_name_reads_as_words() {
        assert_eq!(in_words("TodoList"), "todo list");
        assert_eq!(in_words("file_system"), "file system");
        assert_eq!(in_words("file-explorer"), "file explorer");
        assert_eq!(in_words("WatchHistory"), "watch history");
    }

    /// A name that already names several — a collection of a task, a page of views — is the many
    /// it is, and one of it is the singular of that.
    #[test]
    fn a_name_that_already_names_several_is_its_own_plural() {
        for (one, many) in [
            ("task", "tasks"),
            ("conversation", "conversations"),
            ("comment", "comments"),
            ("view", "views"),
            ("basic", "basics"),
            ("place", "places"),
            ("control", "controls"),
            ("shell part", "shell parts"),
            ("entry", "entries"),
            ("box", "boxes"),
            ("category", "categories"),
        ] {
            assert_eq!(singular(many), one, "{many} is one {one}");
            assert_eq!(plural(many), many, "{many} is already several {many}");
            assert_eq!(plural(one), many, "{one} is several {many}");
            assert_eq!(singular(one), one, "{one} is already one {one}");
        }
    }

    #[test]
    fn a_word_english_counts_no_number_keeps_one_form() {
        for word in ["data", "feedback", "media", "news", "series", "today"] {
            assert_eq!(singular(word), word, "{word}");
            assert_eq!(plural(word), word, "{word}");
        }
    }

    #[test]
    fn the_irregular_plurals_are_englishs() {
        for (one, many) in [
            ("person", "people"),
            ("child", "children"),
            ("foot", "feet"),
        ] {
            assert_eq!(plural(one), many, "{one}");
            assert_eq!(singular(many), one, "{many}");
        }
    }

    /// A singular word ending in `s` is no plural: only a word the plural English writes back
    /// reaches is read as several.
    #[test]
    fn a_singular_ending_in_s_stays_singular() {
        for (one, many) in [
            ("class", "classes"),
            ("status", "statuses"),
            ("bus", "buses"),
            ("gas", "gases"),
            ("analysis", "analyses"),
        ] {
            assert_eq!(singular(one), one, "{one} is one {one}");
            assert_eq!(plural(one), many, "{one} is several {many}");
        }
    }

    /// A phrase is several by its last word, and its first words are the app's own.
    #[test]
    fn a_phrase_is_inflected_by_its_last_word() {
        assert_eq!(plural("todo list"), "todo lists");
        assert_eq!(singular("todo lists"), "todo list");
        assert_eq!(singular("watch histories"), "watch history");
    }
}
