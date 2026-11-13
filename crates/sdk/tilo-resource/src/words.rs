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
//! **A noun an app words its own way is refined in the app's catalogue, never in
//! code.** A type called by an ordinary English noun says it already (`Person` is a
//! `person` and `people`, `Videos` a `video` and `videos`); one an app calls
//! something else (a `Note` it calls a `notebook`) writes the kind's message with
//! `$count` in its `locales/<lang>.ftl`, the same catalogue entry that names the
//! type ([`crate::label`]); a noun no catalogue of theirs holds is the derived one.

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
/// its name in words and refined by the app's catalogue
/// ([`crate::label::localized_noun`]), its
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
        noun: crate::label::localized_noun(kind, Noun::of_type(label)),
        app,
        root,
    }
}

/// A declared resource type whose resources a host publishes from outside a
/// document — the rows a surface shows that live outside it (a fetched
/// catalogue's channels and videos, a process, a container). Its words are
/// the type's, so such a row says what it is to search exactly as a record's
/// item does, and what a press does is one of the type's actions or `open`.
///
/// Every [`Resource`](crate::Resource) is one (its kind and its label are its
/// type's); a type whose state a document does not hold — Tube's fetched
/// `Channel` and `Video`, which live in the catalogue, not a workspace record
/// — implements it by hand with its kind and label, which is all a row needs
/// before any record exists.
pub trait RowType {
    /// [`Resource::KIND`](crate::Resource::KIND): what a row of it is.
    const KIND: &'static str;
    /// [`Resource::LABEL`](crate::Resource::LABEL): its name as people read
    /// it.
    const LABEL: &'static str;

    /// What a resource of it is, as people name it.
    fn words() -> ResourceType {
        type_words(Self::KIND, Self::LABEL)
    }
}

/// A `#[derive(Model)]` record's declaration is its type's: it publishes the
/// record's words too.
impl<T: crate::Resource> RowType for T {
    const KIND: &'static str = T::KIND;
    const LABEL: &'static str = T::LABEL;
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
        noun: crate::label::localized_noun(&format!("{kind}.{field}"), Noun::new(one, many)),
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
        // A noun an app words otherwise is its catalogue's, never code's.
        assert_eq!(
            type_words("tube.person", "Person").noun,
            Noun::new("person", "people"),
            "an irregular plural English writes is derived"
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
}
