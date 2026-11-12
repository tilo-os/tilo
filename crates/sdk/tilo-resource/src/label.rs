//! What a person calls a resource, in the language they read.
//!
//! A resource declares its name once, on its type, in the language the
//! declaration is written in ([`Resource::LABEL`](crate::Resource::LABEL)).
//! That name is the source text of one catalogue entry per kind, keyed by
//! [`message_key`], so a resource published on a desktop that speaks another
//! language is published named in it — and named by its declaration where
//! no catalogue of theirs holds the key.
//!
//! An action is named the same way: what it declares is a [`Word`], a key of
//! the catalogue beside the crate that declares it, its source text read
//! from that catalogue when the crate was built. It is read in the person's
//! language each time the action is published ([`Word::read`]).
//!
//! A host that keeps a catalogue installs the lookup with
//! [`install`]; until it does, every kind is named by its declaration text.
//! This crate has no rendering or catalogue dependency, so the one path a
//! translated name is read through stays the host's.

use std::sync::RwLock;

/// How a host resolves a key in the language the person reads, joining the
/// [`Catalogue`] that holds it first when one is named; `None` when none of
/// their catalogues holds it.
pub type Resolver = fn(Option<&'static Catalogue>, &str) -> Option<String>;

/// The catalogue beside a crate, as a declaration of it names its words by:
/// its source language embedded, its other languages read where language
/// packs are (`<root>/<locale>/<id>.ftl`) or beside the crate.
#[derive(Debug)]
pub struct Catalogue {
    /// Its name in a language pack: the crate's package name.
    pub id: &'static str,
    /// The source language's Fluent text (`locales/en-US.ftl`).
    pub source: &'static str,
    /// The crate's `locales/` directory where it was built.
    pub dir: &'static str,
    /// Every locale the crate translates it into, its source among them.
    pub locales: &'static [&'static str],
}

/// A declared word: a key of its crate's [`Catalogue`] and the text the
/// catalogue's source language gives it.
#[derive(Clone, Copy, Debug)]
pub struct Word {
    pub key: &'static str,
    /// What the source language says: the text where no catalogue of the
    /// person's holds the key.
    pub source: &'static str,
    pub catalogue: Option<&'static Catalogue>,
}

impl Word {
    /// A word no catalogue holds: `source` in every language.
    #[must_use]
    pub const fn plain(source: &'static str) -> Self {
        Self {
            key: "",
            source,
            catalogue: None,
        }
    }

    /// The word in the person's language.
    #[must_use]
    pub fn read(&self) -> String {
        if self.key.is_empty() {
            return self.source.to_owned();
        }
        resolve(self.catalogue, self.key).unwrap_or_else(|| self.source.to_owned())
    }
}

/// `key` through the installed resolver, `None` without one.
fn resolve(catalogue: Option<&'static Catalogue>, key: &str) -> Option<String> {
    let resolver = *RESOLVER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    resolver.and_then(|(resolve, _)| resolve(catalogue, key))
}

/// How a host resolves [`noun_key`] with the count `1` (one of it) or `2`
/// (several); `None` when none of their catalogues holds it.
pub type NounResolver = fn(&str, u32) -> Option<String>;

/// What the words resolve at now on this thread: it moves when the person's
/// language, or a catalogue of theirs, does.
pub type Revision = fn() -> u64;

static RESOLVER: RwLock<Option<(Resolver, Revision)>> = RwLock::new(None);
static NOUN_RESOLVER: RwLock<Option<NounResolver>> = RwLock::new(None);

/// Let every kind published from now on be named through `resolver`.
///
/// A host installs its catalogue's lookup once per process; a later call
/// replaces an earlier one.
///
/// # Panics
/// Only if the lock was poisoned by a panic while installing, which is not
/// a state any caller can be handed.
pub fn install(resolver: Resolver, revision: Revision) {
    *RESOLVER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((resolver, revision));
}

/// What the words resolve at now on this thread ([`Revision`]); `0` before
/// a host installed its catalogue. What is kept of a published action is
/// built again when it moves, so it is published in the language read now.
#[must_use]
pub fn revision() -> u64 {
    RESOLVER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .map_or(0, |(_, revision)| revision())
}

/// Let every kind published from now on read its noun through `resolver`:
/// one message with `$count`, one for a count of `1` and one for several
/// ([`noun_key`]). A later call replaces an earlier one.
///
/// # Panics
/// Only if the lock was poisoned by a panic while installing.
pub fn install_noun(resolver: NounResolver) {
    *NOUN_RESOLVER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(resolver);
}

/// The catalogue key a kind's name is stored under: the kind's own spelling
/// with its separators as dashes, so `add-app.question` reads as
/// `add-app-question`. Kinds are unique across the system, so the key names
/// one resource type.
#[must_use]
pub fn message_key(kind: &str) -> String {
    kind.replace(['.', '_', ':'], "-")
}

/// `source`, the name the resource declares, in the person's language: the
/// catalogue entry for `kind` when one of theirs holds it, `source` itself
/// otherwise.
#[must_use]
pub fn localized(kind: &str, source: &str, catalogue: Option<&'static Catalogue>) -> String {
    resolve(catalogue, &message_key(kind)).unwrap_or_else(|| source.to_owned())
}

/// [`localized`] of a resource that is being published under `label`: its
/// declared name when the publisher kept that name, and `label` as it
/// stands when the publisher names the resource itself — a state-dependent
/// title, say — which no catalogue of the kind can know.
#[must_use]
pub fn published(
    kind: &str,
    label: &str,
    declared: &str,
    catalogue: Option<&'static Catalogue>,
) -> String {
    if label == declared {
        localized(kind, declared, catalogue)
    } else {
        label.to_owned()
    }
}

/// The catalogue key a kind's noun is stored under: [`message_key`] and
/// `-noun`, one message with `$count` (`{ $count -> [one] person *[other]
/// people }`). A type whose name English cannot pluralise writes it; a
/// catalogue that does not hold it leaves the derived noun.
#[must_use]
pub fn noun_key(kind: &str) -> String {
    format!("{}-noun", message_key(kind))
}

/// The noun `derived` for the type of `kind`, in the person's language: the
/// catalogue's message for it, one and several, where one of theirs holds
/// it, `derived` otherwise. A kind's noun is code's to derive and the
/// catalogue's to refine, never a list of irregulars in code.
#[must_use]
pub fn localized_noun(kind: &str, derived: tilo_actions::Noun) -> tilo_actions::Noun {
    let resolver = *NOUN_RESOLVER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(resolve) = resolver else {
        return derived;
    };
    let key = noun_key(kind);
    tilo_actions::Noun {
        one: resolve(&key, 1).unwrap_or(derived.one),
        many: resolve(&key, 2).unwrap_or(derived.many),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn german(_: Option<&'static Catalogue>, key: &str) -> Option<String> {
        (key == "add-app-question").then(|| "Zu den Apps hinzufügen".to_owned())
    }

    #[test]
    fn a_kind_reads_under_its_own_key() {
        assert_eq!(message_key("add-app.question"), "add-app-question");
        assert_eq!(message_key("lock.screen"), "lock-screen");
        assert_eq!(message_key("example.todo_list"), "example-todo-list");
    }

    #[test]
    fn a_kind_is_named_by_its_entry_and_by_its_declaration_without_one() {
        // The lookup is the process's, so every test here stands on a host
        // that installed one.
        install(german, || 0);
        assert_eq!(
            localized("add-app.question", "Add to apps", None),
            "Zu den Apps hinzufügen"
        );
        assert_eq!(localized("lock.screen", "Lock screen", None), "Lock screen");
    }

    #[test]
    fn a_resource_named_by_its_publisher_keeps_that_name() {
        install(german, || 0);
        assert_eq!(
            published("add-app.question", "Add to apps", "Add to apps", None),
            "Zu den Apps hinzufügen"
        );
        assert_eq!(
            published("add-app.question", "Frogs, by name", "Add to apps", None),
            "Frogs, by name"
        );
    }

    #[test]
    fn a_kind_refines_its_noun_from_its_entry_and_keeps_the_derived_one_without_it() {
        install_noun(|key, count| match (key, count) {
            ("people-person-noun", 1) => Some("person".to_owned()),
            ("people-person-noun", 2) => Some("people".to_owned()),
            _ => None,
        });
        assert_eq!(noun_key("people.person"), "people-person-noun");
        assert_eq!(
            localized_noun("people.person", tilo_actions::Noun::of_type("Person")),
            tilo_actions::Noun::new("person", "people"),
            "the catalogue says what several are"
        );
        assert_eq!(
            localized_noun("lock.screen", tilo_actions::Noun::of_type("Screen")),
            tilo_actions::Noun::new("screen", "screens"),
            "a noun no catalogue holds is the derived one"
        );
    }
}
