//! The platform's phrases the standard library's frames say (`phrase("tilolang-empty")`), in
//! the person's language, without the toolkit: the UI host's own catalogue, read through the
//! platform's toolkit-free Fluent catalogue (`tilo_history::Catalogue`).

use tilo_history::{Catalogue, FluentArgs, Messages as _};

const SOURCE: (&str, &str) = (
    "en-US",
    include_str!("../../tilo-ui-host/locales/en-US.ftl"),
);
const TRANSLATIONS: &[(&str, &str)] = &[(
    "de-DE",
    include_str!("../../tilo-ui-host/locales/de-DE.ftl"),
)];

/// `locale` as a tag this catalogue compares (`de_DE.UTF-8` is `de-DE`).
fn normalized(locale: &str) -> String {
    let wanted = locale.replace('_', "-");
    wanted.split('.').next().unwrap_or_default().to_owned()
}

/// The phrases of `wanted`, in its own language when there are any, else the source's.
fn choose(wanted: &str) -> &'static (&'static str, &'static str) {
    let language = wanted.split('-').next().unwrap_or_default();
    TRANSLATIONS
        .iter()
        .find(|(candidate, _)| *candidate == wanted)
        .or_else(|| {
            TRANSLATIONS
                .iter()
                .find(|(candidate, _)| candidate.split('-').next() == Some(language))
        })
        .unwrap_or(&SOURCE)
}

/// The phrases of one language.
pub struct Phrases {
    /// The language the host was built for, as its environment named it: the person's.
    preference: String,
    locale: &'static str,
    catalogue: std::cell::RefCell<Catalogue>,
}

impl Phrases {
    /// The phrases of `locale` (`de-DE`, `de_DE.UTF-8`), or of the closest language there are
    /// phrases of: the same language, else the source's. A phrase a translation lacks reads in
    /// the source language.
    #[must_use]
    pub fn new(locale: &str) -> Self {
        let wanted = normalized(locale);
        let chosen = choose(&wanted);
        let mut sources = vec![chosen.1];
        if chosen.0 != SOURCE.0 {
            sources.push(SOURCE.1);
        }
        Self {
            preference: wanted,
            locale: chosen.0,
            catalogue: std::cell::RefCell::new(Catalogue::new(chosen.0, &sources)),
        }
    }

    /// The phrases of the window of a package that ships `shipped`, the languages of its
    /// catalogues: the person's when the package ships it, the package's source language
    /// otherwise ([`tilo_history::window_language`]), so the frames' words read in the language
    /// the window reads in throughout.
    #[must_use]
    pub fn for_window(&self, shipped: &[&str]) -> Self {
        let language = tilo_history::window_language(Some(&self.preference), shipped);
        Self::new(&language)
    }

    /// The language the phrases are in (`en-US`).
    #[must_use]
    pub fn locale(&self) -> &'static str {
        self.locale
    }

    /// The language the person prefers (`de` for `de-DE`), the one they type dates and times
    /// in, whatever language the window's phrases are in.
    #[must_use]
    pub fn language(&self) -> String {
        let preference = normalized(&self.preference);
        preference
            .split('-')
            .next()
            .filter(|language| !language.is_empty())
            .unwrap_or("en")
            .to_owned()
    }

    /// The phrases of the language the process's environment names.
    #[must_use]
    pub fn from_env() -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| SOURCE.0.to_owned());
        Self::new(&locale)
    }

    /// The phrase `id` with `args`; the id itself when there is no such phrase.
    #[must_use]
    pub fn say(&self, id: &str, args: &[(String, String)]) -> String {
        let mut fluent = FluentArgs::new();
        for (name, value) in args {
            // A number stays a number, so a plural rule selects by it (`[one]` for 1).
            match value.parse::<f64>() {
                Ok(number) => fluent.set(name.as_str(), number),
                Err(_) => fluent.set(name.as_str(), value.as_str()),
            }
        }
        self.catalogue
            .borrow()
            .format(id, &fluent)
            .unwrap_or_else(|| id.to_owned())
    }

    /// Takes in a package's catalogues, `(locale, Fluent text)` (i18n.md): the one in these
    /// phrases' language first, then the source language's, so a translation wins and a message
    /// it lacks reads in the source language.
    pub fn add_package(&self, catalogues: &[(String, String)]) {
        let language = self.locale.split('-').next().unwrap_or_default();
        let mut catalogue = self.catalogue.borrow_mut();
        let translation = catalogues
            .iter()
            .find(|(locale, _)| locale == self.locale)
            .or_else(|| {
                catalogues
                    .iter()
                    .find(|(locale, _)| locale.split('-').next() == Some(language))
            });
        if let Some((_, text)) = translation {
            catalogue.add(text);
        }
        if let Some((_, text)) = catalogues.iter().find(|(locale, _)| locale == SOURCE.0) {
            catalogue.add(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A package's words read in the person's language (i18n.md): its translation where it has
    /// one, its source language where it lacks one, a type's noun by count.
    #[test]
    fn a_packages_words_read_in_the_persons_language() {
        let catalogues = [
            (
                "en-US".to_owned(),
                "d-new = New note\nd-pin = Pin\nd-note = Note\n    .noun = { $count ->\n        [one] note\n       *[other] notes\n    }\n".to_owned(),
            ),
            (
                "de-DE".to_owned(),
                "d-new = Neue Notiz\nd-note = Notiz\n    .noun = { $count ->\n        [one] Notiz\n       *[other] Notizen\n    }\n".to_owned(),
            ),
        ];
        let count = |n: &str| vec![("count".to_owned(), n.to_owned())];
        let german = Phrases::new("de_DE.UTF-8");
        german.add_package(&catalogues);
        assert_eq!(german.say("d-new", &[]), "Neue Notiz");
        assert_eq!(
            german.say("d-pin", &[]),
            "Pin",
            "a word the translation lacks is the source's"
        );
        assert_eq!(german.say("d-note.noun", &count("1")), "Notiz");
        assert_eq!(german.say("d-note.noun", &count("2")), "Notizen");
        let english = Phrases::new("en-US");
        english.add_package(&catalogues);
        assert_eq!(english.say("d-new", &[]), "New note");
        assert_eq!(english.say("d-note.noun", &count("2")), "notes");
        assert_eq!(
            english.say("d-gone", &[]),
            "d-gone",
            "no catalogue: the key, as a phrase is"
        );
    }

    /// A window speaks the language its package ships: an English-only app reads English even
    /// where the person's own language is German.
    #[test]
    fn a_windows_phrases_are_in_the_language_its_package_ships() {
        let german_host = Phrases::new("de_DE.UTF-8");
        assert_ne!(
            german_host.say("tilolang-empty", &[]),
            "Nothing here yet",
            "the host's own language is German"
        );
        let english_only = german_host.for_window(&["en-US"]);
        assert_eq!(english_only.locale(), "en-US");
        assert_eq!(english_only.say("tilolang-empty", &[]), "Nothing here yet");
        let both = german_host.for_window(&["en-US", "de-DE"]);
        assert_eq!(both.locale(), "de-DE", "the person's, when it ships it");
    }

    #[test]
    fn the_standard_librarys_phrases_read_in_the_persons_language() {
        let english = Phrases::new("en-US");
        for id in [
            "tilolang-all",
            "tilolang-empty",
            "tilolang-empty-kind",
            "tilolang-none-selected",
            "tilolang-sort",
            "tilolang-done",
            "tilolang-no-date",
            "tilolang-all-done",
            "tilolang-all-caught-up",
            "tilolang-add",
        ] {
            let kind = [("kind".to_owned(), "notes".to_owned())];
            assert_ne!(english.say(id, &kind), id, "{id} has a phrase");
        }
        let german = Phrases::new("de_DE.UTF-8");
        assert_ne!(
            german.say("tilolang-empty", &[]),
            english.say("tilolang-empty", &[]),
            "German has its own"
        );
        assert_eq!(
            german.say(
                "tilolang-empty-kind",
                &[("kind".to_owned(), "Notizen".to_owned())]
            ),
            "Noch keine Notizen",
            "a list that finds nothing reads in the person's language"
        );
        assert_eq!(english.say("no-such-phrase", &[]), "no-such-phrase");
    }
}
