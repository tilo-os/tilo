//! What a person calls a resource, in the language they read.
//!
//! A resource declares its name once, on its type, in the language the
//! declaration is written in ([`Resource::LABEL`](crate::Resource::LABEL)).
//! That name is the source text of one catalogue entry per kind, keyed by
//! [`message_key`], so a resource published on a desktop that speaks another
//! language is published named in it — and named by its declaration where
//! no catalogue of theirs holds the key.
//!
//! A host that keeps a catalogue installs the lookup with
//! [`install`]; until it does, every kind is named by its declaration text.
//! This crate has no rendering or catalogue dependency, so the one path a
//! translated name is read through stays the host's.

use std::sync::RwLock;

/// How a host resolves [`message_key`] in the language the person reads;
/// `None` when none of their catalogues holds it.
pub type Resolver = fn(&str) -> Option<String>;

static RESOLVER: RwLock<Option<Resolver>> = RwLock::new(None);

/// Let every kind published from now on be named through `resolver`.
///
/// A host installs its catalogue's lookup once per process; a later call
/// replaces an earlier one.
///
/// # Panics
/// Only if the lock was poisoned by a panic while installing, which is not
/// a state any caller can be handed.
pub fn install(resolver: Resolver) {
    *RESOLVER
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
pub fn localized(kind: &str, source: &str) -> String {
    let resolver = *RESOLVER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match resolver {
        Some(resolve) => resolve(&message_key(kind)).unwrap_or_else(|| source.to_owned()),
        None => source.to_owned(),
    }
}

/// [`localized`] of a resource that is being published under `label`: its
/// declared name when the publisher kept that name, and `label` as it
/// stands when the publisher names the resource itself — a state-dependent
/// title, say — which no catalogue of the kind can know.
#[must_use]
pub fn published(kind: &str, label: &str, declared: &str) -> String {
    if label == declared {
        localized(kind, declared)
    } else {
        label.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn german(key: &str) -> Option<String> {
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
        install(german);
        assert_eq!(
            localized("add-app.question", "Add to apps"),
            "Zu den Apps hinzufügen"
        );
        assert_eq!(localized("lock.screen", "Lock screen"), "Lock screen");
    }

    #[test]
    fn a_resource_named_by_its_publisher_keeps_that_name() {
        install(german);
        assert_eq!(
            published("add-app.question", "Add to apps", "Add to apps"),
            "Zu den Apps hinzufügen"
        );
        assert_eq!(
            published("add-app.question", "Frogs, by name", "Add to apps"),
            "Frogs, by name"
        );
    }
}
