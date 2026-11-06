//! What a resource is called, for the resources its type publishes.
//!
//! Every resource says what it is ([`tilo_actions::ResourceType`]): its
//! type's noun, its app's name, and whether it is that app's root. The type's
//! noun is its own name in words, down to what a collection's items are. The
//! app's name is not the type's to say: the framework app hosting the types
//! names itself once ([`name_app`]), and every type it publishes, at any
//! depth and whatever its kind, says it without writing anything.
//!
//! A kind names its app by its first part (`tube` in `tube.channel`,
//! `file-explorer` in `file-explorer.entry`): the part every kind of one app
//! shares, and the part that keeps two apps' kinds apart. A host that
//! publishes a type none registered (a test's, a tool's) gets its app's name
//! from [`tilo_actions::in_words`] of that part.
//!
//! **A noun is refined in the app's catalogue, never in code.** A type whose
//! name English cannot derive a noun for (`Person` is a `person` and
//! `people`, `Sheep` a `sheep` and `sheep`) writes the message
//! `noun-<kind>` in its `locales/<lang>.ftl`, one message with `$count`, as
//! i18n.md has a tilolang type write `<id>.noun`. The host lending its
//! catalogue ([`refine_with`]) reads it in the reader's language; without a
//! catalogue, or a message no catalogue holds, the derived noun stands.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use tilo_actions::{Noun, ResourceType};
use tilo_lock::MutexRecover as _;

/// What one app is called, and which kind is its root.
#[derive(Clone, Debug)]
struct AppWords {
    /// Its name as people read it: `Tube`, `tubes`.
    app: Noun,
    /// The kind of its root: the one resource of it a workspace holds, which
    /// a query naming the app means.
    root: &'static str,
}

fn registry() -> &'static Mutex<BTreeMap<&'static str, AppWords>> {
    static APPS: OnceLock<Mutex<BTreeMap<&'static str, AppWords>>> = OnceLock::new();
    APPS.get_or_init(Mutex::default)
}

/// Name the app that publishes records of the kind `root` — its title, as
/// people read it — for every kind the app declares: `tube` for
/// `tube.library`, so `tube.channel` and `tube.video` say the same app.
///
/// Called by the framework app once, where it declares its types; calling it
/// again with the same title is free, and with another (a launch naming the
/// app otherwise) renames it from the next publication.
pub fn name_app(title: &'static str, root: &'static str) {
    let Some(segment) = segment_of(root) else {
        return;
    };
    let mut app = registry().lock_recover();
    let many = tilo_actions::plural(&title.to_lowercase());
    app.insert(
        segment,
        AppWords {
            app: Noun::new(title, many),
            root,
        },
    );
}

/// A catalogue that refines the words a type's name derives: the message
/// `key`, formatted with `count` (`None` for the noun itself, `Some(1)` and
/// `Some(2)` for its singular and plural), and `None` when the catalogue does
/// not hold it.
pub type Refiner = std::rc::Rc<dyn Fn(&str, Option<u32>) -> Option<String>>;

thread_local! {
    /// The catalogue the reader's language resolves through, once a host has
    /// lent one ([`refine_with`]).
    static REFINED: std::cell::RefCell<Option<Refiner>> = const { std::cell::RefCell::new(None) };
}

/// Resolve a type's nouns through the catalogue `refiner` names, in the
/// reader's language: a noun the code derives is refined where the app's
/// catalogue says otherwise (i18n.md). A host lends its own
/// ([`refine_with`]); a thread without one keeps the derived nouns.
pub fn refine_with(refiner: impl Fn(&str, Option<u32>) -> Option<String> + 'static) {
    REFINED.with(|kept| *kept.borrow_mut() = Some(std::rc::Rc::new(refiner)));
}

/// Forget the catalogue [`refine_with`] lent this thread.
pub fn forget_catalogue() {
    REFINED.with(|kept| *kept.borrow_mut() = None);
}

/// The noun `derived` for the type of `kind`, refined by the catalogue where
/// it holds `noun-<kind>` (one message with `$count`). The kind stands in
/// for a tilolang type's stable id: it is what every resource of the type
/// names itself by across apps.
#[must_use]
pub fn noun_of(kind: &str, derived: Noun) -> Noun {
    let refinements = REFINED.with(|kept| kept.borrow().clone());
    let Some(refiner) = refinements else {
        return derived;
    };
    let key = noun_key(kind);
    let one = refiner(&key, Some(1));
    let many = refiner(&key, Some(2));
    Noun {
        one: one.unwrap_or(derived.one),
        many: many.unwrap_or(derived.many),
    }
}

/// The catalogue key of the type of `kind`'s noun: `noun-<kind>`, its dots
/// and other punctuation read as these separators (a Fluent message id takes
/// letters, digits, `_` and `-` only).
#[must_use]
pub fn noun_key(kind: &str) -> String {
    let mut key = String::from("noun-");
    key.extend(kind.chars().map(|c| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        }
    }));
    key
}

/// The first part of a kind: the app it belongs to (`tube` in
/// `tube.channel`).
fn segment_of(kind: &'static str) -> Option<&'static str> {
    let end = kind.find('.').unwrap_or(kind.len());
    let segment = kind.get(..end)?;
    (!segment.is_empty()).then_some(segment)
}

/// What the app of the kind `kind` is called, and whether `kind` is its
/// root. A kind no app registered names its app by its first part in words.
#[must_use]
pub fn app_words_of(kind: &str) -> (Noun, bool) {
    let segment = kind.split('.').next().unwrap_or(kind);
    let words = registry().lock_recover().get(segment).cloned();
    match words {
        Some(words) => (words.app, words.root == kind),
        None => (Noun::of_type(segment), false),
    }
}

/// What a resource of `kind`, whose type is called `label` as people read it
/// (a `#[resource]`'s `label`, e.g. `Channel`), is: the type's noun made of
/// its name in words and refined by the app's catalogue ([`noun_of`]), its
/// app's name, and whether it is that app's root. The app is the kind's
/// first part, as [`app_words_of`] reads it.
///
/// The one place a Rust type's [`ResourceType`] is built, so a published
/// type reads to search, the palette and agents exactly as a tilolang type
/// does.
#[must_use]
pub fn type_words(kind: &str, label: &str) -> ResourceType {
    let (app, root) = app_words_of(kind);
    ResourceType {
        noun: noun_of(kind, Noun::of_type(label)),
        app,
        root,
    }
}

/// What the items of the collection `field` of a resource of `kind` are: the
/// collection's noun in the singular (`video` for a `videos` field), with its
/// plural, and the app named as the resource names it. An item is not its
/// app's root, and the app's catalogue may refine the noun as it may a
/// type's.
#[must_use]
pub fn item_words(kind: &str, field: &str) -> ResourceType {
    let (app, _) = app_words_of(kind);
    let many = tilo_actions::in_words(field);
    let one = tilo_actions::singular(&many);
    ResourceType {
        noun: noun_of(&format!("{kind}.{field}"), Noun::new(one, many)),
        app,
        root: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_names_every_kind_it_publishes() {
        name_app("Word Smith", "wordsmith.library");
        let (app, root) = app_words_of("wordsmith.library");
        assert_eq!(app, Noun::new("Word Smith", "word smiths"));
        assert!(root, "the kind the app is named by is its root");
        let (also, root) = app_words_of("wordsmith.document");
        assert_eq!(also, app, "another kind of the app says the same app");
        assert!(!root);
    }

    #[test]
    fn a_kind_no_app_named_reads_its_app_in_words() {
        let (app, root) = app_words_of("no-such-app.thing");
        assert_eq!(app, Noun::new("no such app", "no such apps"));
        assert!(!root);
    }

    #[test]
    fn a_type_reads_its_noun_from_its_name_in_words() {
        name_app("Tube", "tube.library");
        let words = type_words("tube.channel", "Channel");
        assert_eq!(words.noun, Noun::new("channel", "channels"));
        assert_eq!(words.app, Noun::new("Tube", "tubes"));
        assert!(!words.root);
        assert!(type_words("tube.library", "Tube").root);
        // A noun English cannot derive is the catalogue's, not code's.
        assert_eq!(
            type_words("tube.person", "Person").noun,
            Noun::new("person", "persons"),
            "the derived noun, until a catalogue refines it"
        );
    }

    #[test]
    fn an_items_noun_is_its_collections_in_the_singular() {
        name_app("Tube", "tube.library");
        let words = item_words("tube.library", "videos");
        assert_eq!(words.noun, Noun::new("video", "videos"));
        assert_eq!(words.app, Noun::new("Tube", "tubes"));
        assert!(!words.root);
    }

    #[test]
    fn the_catalogue_refines_a_noun_and_the_derived_one_stands_without_it() {
        name_app("People", "people.library");
        let person = || type_words("people.person", "Person").noun;
        assert_eq!(person(), Noun::new("person", "persons"));
        refine_with(|key, count| match (key, count) {
            ("noun-people-person", Some(1)) => Some("person".to_owned()),
            ("noun-people-person", Some(2)) => Some("people".to_owned()),
            _ => None,
        });
        assert_eq!(
            person(),
            Noun::new("person", "people"),
            "the catalogue says what several are"
        );
        forget_catalogue();
        assert_eq!(person(), Noun::new("person", "persons"));
    }
}
