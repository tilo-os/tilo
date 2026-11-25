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
//! language each time the action is published ([`Word::read`]), its
//! placeables filled with the [`Argument`]s it carries — so an action bound
//! to what a person is looking at ("Rename “report.pdf”") names it in every
//! language too.
//!
//! A host that keeps a catalogue installs the lookup with
//! [`install`]; until it does, every kind is named by its declaration text.
//! This crate has no rendering or catalogue dependency, so the one path a
//! translated name is read through stays the host's.

use std::sync::RwLock;

/// How a host resolves a key in the language the person reads, its
/// placeables filled with `arguments`, joining the [`Catalogue`] that holds
/// it first when one is named; `None` when none of their catalogues holds it
/// as [`Fill`] asks.
pub type Resolver =
    fn(Option<&'static Catalogue>, &str, &[(&str, Argument)], Fill) -> Option<String>;

/// What a resolution takes of a message whose placeables `arguments` leave
/// unfilled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// A declared [`Word`], whose arguments its build checked against the
    /// source language: a catalogue that still names a value it lacks is
    /// shown as best it can be, and the host reports it.
    AsGiven,
    /// A key derived by convention (a parameter's title, a kind's name):
    /// another message can share it — a history sentence for the field a
    /// parameter sets ("Named the workbook { $new }") — and one that takes a
    /// value this lookup cannot give is not this key's word.
    Whole,
}

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
#[derive(Clone, Debug)]
pub struct Word {
    pub key: &'static str,
    /// What the source language says: the text where no catalogue of the
    /// person's holds the key.
    pub source: &'static str,
    pub catalogue: Option<&'static Catalogue>,
    /// What the placeables of the message show (`name = …`), by the names
    /// the message calls them: a word naming what a person looks at
    /// ("Rename “report.pdf”") carries it here rather than in text no
    /// catalogue can translate.
    pub arguments: Vec<(&'static str, Argument)>,
}

impl Word {
    /// The word in the person's language, its placeables filled — and its
    /// declared text, its placeables filled too, where no catalogue of theirs
    /// holds the key, or where no process installed one ([`install`]): a server or
    /// an agent applying an action reads the same words as a window without the
    /// catalogue syntax.
    #[must_use]
    pub fn read(&self) -> String {
        resolve(self.catalogue, self.key, &self.arguments, Fill::AsGiven)
            .unwrap_or_else(|| filled(self.source, &self.arguments))
    }
}

/// `source` with its placeables filled from `arguments`: a simple one shows its
/// argument, a select block the form its count names (else the `*` form), and
/// a placeable no argument names stays as the catalogue wrote it.
fn filled(source: &str, arguments: &[(&'static str, Argument)]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or_default());
        let after = rest.get(open.saturating_add(1)..).unwrap_or_default();
        let Some(close) = matching_close(after) else {
            out.push_str(rest.get(open..).unwrap_or_default());
            return out;
        };
        let placeable = after.get(..close).unwrap_or_default().trim();
        rest = after.get(close.saturating_add(1)..).unwrap_or_default();
        out.push_str(
            &placeable_text(placeable, arguments).unwrap_or_else(|| format!("{{{placeable}}}")),
        );
    }
    out.push_str(rest);
    out
}

/// The offset of the `}` closing a placeable whose text starts at `after`:
/// a select block holds placeables of its own, so braces pair up.
fn matching_close(after: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (at, character) in after.char_indices() {
        match character {
            '{' => depth = depth.saturating_add(1),
            '}' if depth == 0 => return Some(at),
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// What the placeable `placeable` (the text between its braces) shows, or
/// `None` when no argument fills it.
fn placeable_text(placeable: &str, arguments: &[(&'static str, Argument)]) -> Option<String> {
    let (name, body) = match placeable.split_once("->") {
        Some((name, body)) => (name.trim(), Some(body)),
        None => (placeable, None),
    };
    let name = name.strip_prefix('$')?.trim();
    let (_, value) = arguments.iter().find(|(called, _)| *called == name)?;
    match body {
        None => Some(shown(value)),
        Some(body) => {
            let wanted = match value {
                Argument::Number(count) if (*count - 1.0).abs() < f64::EPSILON => "one",
                _ => "other",
            };
            let forms: Vec<(&str, bool, &str)> = body
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let starred = line.starts_with('*');
                    let line = line.strip_prefix('*').unwrap_or(line).trim_start();
                    let (key, text) = line.strip_prefix('[')?.split_once(']')?;
                    Some((key, starred, text.trim()))
                })
                .collect();
            let (_, _, text) = forms
                .iter()
                .find(|(key, _, _)| *key == wanted)
                .or_else(|| forms.iter().find(|(_, starred, _)| *starred))?;
            Some(filled(text, arguments))
        }
    }
}

/// What one argument shows.
fn shown(value: &Argument) -> String {
    match value {
        Argument::Text(text) => text.clone(),
        Argument::Number(count) if count.fract() == 0.0 => format!("{count:.0}"),
        Argument::Number(count) => count.to_string(),
    }
}

/// One argument of a [`Word`]: what one of its placeables (`{ $name }`)
/// shows in every language.
#[derive(Clone, Debug, PartialEq)]
pub enum Argument {
    /// Words as they are, in no catalogue: the name of a file, a marker.
    Text(String),
    /// A number, which a message with count forms (`{ $count -> [one] … }`)
    /// selects its form by.
    Number(f64),
}

impl From<&str> for Argument {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<&String> for Argument {
    fn from(text: &String) -> Self {
        Self::Text(text.clone())
    }
}

impl From<String> for Argument {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<std::borrow::Cow<'_, str>> for Argument {
    fn from(text: std::borrow::Cow<'_, str>) -> Self {
        Self::Text(text.into_owned())
    }
}

/// A count a message shows, as the number its forms select by: a selection of
/// one entry, or of several, that is never near what `f64` holds exactly.
macro_rules! argument_from_count {
    ($($t:ty),*) => {$(
        impl From<$t> for Argument {
            fn from(count: $t) -> Self {
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_lossless,
                    reason = "a count a message shows is far below what f64 holds exactly"
                )]
                let count = count as f64;
                Self::Number(count)
            }
        }
    )*};
}

argument_from_count!(usize, u32, i64, f64);

/// `params`, each called what the catalogue of `word` calls it where it
/// holds a word for it: the action's own (`<action key>-<param>`), else the
/// one every action of `scope` (an operation or a kind) shares
/// (`<scope>-<param>`). A parameter neither names keeps its schema as it is.
#[must_use]
pub fn titled(
    word: &Word,
    scope: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> Vec<tilo_actions::ActionParam> {
    let scope = message_key(scope);
    params
        .into_iter()
        .map(|param| {
            let name = message_key(param.name.as_ref());
            let title = resolve(
                word.catalogue,
                &format!("{}-{name}", word.key),
                &[],
                Fill::Whole,
            )
            .or_else(|| resolve(word.catalogue, &format!("{scope}-{name}"), &[], Fill::Whole));
            match title {
                Some(title) => param.described(title),
                None => param,
            }
        })
        .collect()
}

/// `key` through the installed resolver, `None` without one.
fn resolve(
    catalogue: Option<&'static Catalogue>,
    key: &str,
    arguments: &[(&str, Argument)],
    fill: Fill,
) -> Option<String> {
    let resolver = *RESOLVER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    resolver.and_then(|(resolve, _)| resolve(catalogue, key, arguments, fill))
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

pub use tilo_actions::{message_key, noun_key};

/// The catalogue beside the crate being built, as a `static`:
/// `declared_catalogue!(CATALOGUE);`, its source language embedded and its
/// other languages read from the language packs as a declaration's words are.
///
/// A declaration names its words through its own catalogue; a crate that
/// publishes a resource of a kind no declaration of its owns — a service's
/// rows, read from what the machine holds — names it through this one
/// ([`localized`] of the kind and the name it declares).
#[macro_export]
macro_rules! declared_catalogue {
    ($vis:vis $name:ident) => {
        $vis static $name: $crate::label::Catalogue = $crate::label::Catalogue {
            id: ::core::env!("CARGO_PKG_NAME"),
            source: ::core::include_str!(::core::concat!(
                ::core::env!("CARGO_MANIFEST_DIR"),
                "/locales/en-US.ftl"
            )),
            dir: ::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/locales"),
            locales: &["de-DE", "en-US"],
        };
    };
}

/// What a resource of a kind this crate publishes from outside a document is
/// called, in the person's language: the kind's entry in the catalogue beside
/// this crate, read the way a declaration's words are.
///
/// Declares a module `i18n` holding the crate's `CATALOGUE` (through
/// `declared_catalogue!`) and `pub(crate) fn name(kind, source)` (through
/// [`localized`]).
#[macro_export]
macro_rules! declared_names {
    () => {
        mod i18n {
            $crate::declared_catalogue!(CATALOGUE);

            pub(crate) fn name(kind: &str, source: &str) -> String {
                $crate::label::localized(kind, source, Some(&CATALOGUE))
            }
        }
    };
}

/// `source`, the name the resource declares, in the person's language: the
/// catalogue entry for `kind` when one of theirs holds it, `source` itself
/// otherwise.
#[must_use]
pub fn localized(kind: &str, source: &str, catalogue: Option<&'static Catalogue>) -> String {
    resolve(catalogue, &message_key(kind), &[], Fill::Whole).unwrap_or_else(|| source.to_owned())
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

    /// A word read where no process installed a lookup — a server applying an
    /// action, an agent reading one — fills its placeables from the source
    /// text rather than showing the catalogue syntax.
    #[test]
    fn a_word_without_a_lookup_fills_its_placeables() {
        let pair = Word {
            key: "merge-title-pair",
            source: "Merge of { $ours } and { $theirs }",
            catalogue: None,
            arguments: vec![
                ("ours", Argument::from("a.txt")),
                ("theirs", Argument::from("b.txt")),
            ],
        };
        assert_eq!(pair.read(), "Merge of a.txt and b.txt");
        let rules = Word {
            key: "zone-rules",
            source: "{ $count ->\n    [one] 1 rule\n   *[other] { $count } rules\n}",
            catalogue: None,
            arguments: vec![("count", Argument::from(2_usize))],
        };
        assert_eq!(rules.read(), "2 rules");
        let one = Word {
            arguments: vec![("count", Argument::from(1_usize))],
            ..rules
        };
        assert_eq!(one.read(), "1 rule");
    }

    fn german(
        _: Option<&'static Catalogue>,
        key: &str,
        _: &[(&str, Argument)],
        _: Fill,
    ) -> Option<String> {
        (key == "add-app-question").then(|| "Zu den Apps hinzufügen".to_owned())
    }

    tilo_test_env::scenario! {
        fn a_kind_and_its_words_read_in_the_persons_language;

        phase a_kind_reads_under_its_own_key;
        {
            assert_eq!(message_key("add-app.question"), "add-app-question");
            assert_eq!(message_key("lock.screen"), "lock-screen");
            assert_eq!(message_key("example.todo_list"), "example-todo-list");
        }

        phase a_kind_is_named_by_its_entry_and_by_its_declaration_without_one;
        {
            // The lookup is the process's, so every test here stands on a host
            // that installed one.
            install(german, || 0);
            assert_eq!(
                localized("add-app.question", "Add to apps", None),
                "Zu den Apps hinzufügen"
            );
            assert_eq!(localized("lock.screen", "Lock screen", None), "Lock screen");
        }

        phase a_words_placeables_are_filled_in_the_language_it_is_read_in;
        {
            fn naming(
                _: Option<&'static Catalogue>,
                key: &str,
                arguments: &[(&str, Argument)],
                _: Fill,
            ) -> Option<String> {
                if key != "files-folder-rename-selected" {
                    return None;
                }
                let named = arguments.iter().find_map(|(name, value)| match value {
                    Argument::Text(text) if *name == "name" => Some(text.as_str()),
                    _ => None,
                });
                Some(format!("„{}“ umbenennen", named.unwrap_or_default()))
            }
            install(naming, || 0);
            let word = Word {
                key: "files-folder-rename-selected",
                source: "Rename \u{201c}{ $name }\u{201d}",
                catalogue: None,
                arguments: vec![("name", Argument::from("Bericht.pdf"))],
            };
            assert_eq!(word.read(), "„Bericht.pdf“ umbenennen");
        }

        phase a_resource_named_by_its_publisher_keeps_that_name;
        {
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

        phase a_kind_refines_its_noun_from_its_entry_and_keeps_the_derived_one_without_it;
        {
            install_noun(|key, count| match (key, count) {
                ("notes-note-noun", 1) => Some("notebook".to_owned()),
                ("notes-note-noun", 2) => Some("notebooks".to_owned()),
                _ => None,
            });
            assert_eq!(noun_key("notes.note"), "notes-note-noun");
            assert_eq!(
                localized_noun("notes.note", tilo_actions::Noun::of_type("Note")),
                tilo_actions::Noun::new("notebook", "notebooks"),
                "the catalogue says what the app calls it"
            );
            assert_eq!(
                localized_noun("lock.screen", tilo_actions::Noun::of_type("Screen")),
                tilo_actions::Noun::new("screen", "screens"),
                "a noun no catalogue holds is the derived one"
            );
        }
    }
}
