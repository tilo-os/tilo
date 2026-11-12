//! The platform's phrases the standard library's frames say (`phrase("tilolang-empty")`), in
//! the person's language, without the toolkit: the UI host's own catalogue and the toolkit's
//! locale catalogues — the words dates and clocks are written in — read through the platform's
//! toolkit-free Fluent catalogue (`tilo_history::Catalogue`).

use tilo_history::{Catalogue, FluentArgs, Messages as _};

const SOURCE: (&str, &str) = (
    "en-US",
    include_str!("../../tilo-ui-host/locales/en-US.ftl"),
);
const TRANSLATIONS: &[(&str, &str)] = &[(
    "de-DE",
    include_str!("../../tilo-ui-host/locales/de-DE.ftl"),
)];

/// The words the toolkit writes dates, clocks and numbers with (`ui::format`), which a host
/// without the toolkit says too: the same catalogues the toolkit reads, so a date reads the same
/// in a window and in what its host wrote beside it.
const TOOLKIT: (&str, &str) = (
    "en-US",
    include_str!("../../../sdk/ui-core/locales/en-US.ftl"),
);
const TOOLKIT_TRANSLATIONS: &[(&str, &str)] = &[(
    "de-DE",
    include_str!("../../../sdk/ui-core/locales/de-DE.ftl"),
)];

/// `locale` as a tag this catalogue compares (`de_DE.UTF-8` is `de-DE`).
fn normalized(locale: &str) -> String {
    let wanted = locale.replace('_', "-");
    wanted.split('.').next().unwrap_or_default().to_owned()
}

/// `translations`' phrases of `wanted`, in its own language when there are any, else `source`.
fn choose_in(
    translations: &'static [(&'static str, &'static str)],
    wanted: &str,
    source: &'static (&'static str, &'static str),
) -> &'static (&'static str, &'static str) {
    let language = wanted.split('-').next().unwrap_or_default();
    translations
        .iter()
        .find(|(candidate, _)| *candidate == wanted)
        .or_else(|| {
            translations
                .iter()
                .find(|(candidate, _)| candidate.split('-').next() == Some(language))
        })
        .unwrap_or(source)
}

/// The phrases of one language.
pub struct Phrases {
    /// The language the phrases were asked for, normalized (`de-DE`): the person's, as their
    /// session named it, else the one this process runs in. A window takes it as the language the
    /// person prefers (`de`), whatever language the window itself reads in.
    preference: String,
    /// The language the phrases are in (`de-DE`): the window's, chosen among the languages the
    /// package it shows ships. It is not the language of the host's own catalogue, which need not
    /// have a translation in it: an app written in a language the host's chrome has none of still
    /// reads its own words in it.
    locale: String,
    catalogue: std::cell::RefCell<Catalogue>,
}

impl Phrases {
    /// The phrases of `locale` (`de-DE`, `de_DE.UTF-8`): the host's own words and the toolkit's,
    /// in that language or in the closest the catalogues have (the same language, else the
    /// source's), and every package's words added later ([`Self::add_package`]) in it. A phrase a
    /// translation lacks reads in the source language.
    #[must_use]
    pub fn new(locale: &str) -> Self {
        let wanted = normalized(locale);
        let mut sources = Vec::new();
        // The host's own words, and the toolkit's beside them: a key both hold reads as the
        // host's. Each falls back to its source language on its own, so a language the host's
        // chrome has no translation for still reads its source rather than nothing at all.
        for (translations, source) in [(TRANSLATIONS, &SOURCE), (TOOLKIT_TRANSLATIONS, &TOOLKIT)] {
            let chosen = choose_in(translations, &wanted, source);
            sources.push(chosen.1);
            if chosen.0 != source.0 {
                sources.push(source.1);
            }
        }
        let catalogue = std::cell::RefCell::new(Catalogue::new(&wanted, &sources));
        Self {
            preference: wanted.clone(),
            locale: wanted,
            catalogue,
        }
    }

    /// The phrases of the window of a package that ships `shipped`, the languages of its
    /// catalogues, in the language the window speaks: `language`, the person's as their session
    /// named it, when the package ships it, the package's source language otherwise
    /// ([`tilo_history::window_language`]); where the session named none, the language this
    /// process runs in. The frames' words, the app's own and the history's sentences all read in
    /// it, so the window reads in one language throughout. What the person types dates and times
    /// in stays their own language, whatever language the window reads in ([`Self::language`]).
    #[must_use]
    pub fn for_window(&self, language: Option<&str>, shipped: &[&str]) -> Self {
        let preference = language
            .map(str::trim)
            .filter(|language| !language.is_empty())
            .unwrap_or_else(|| self.preference.as_str());
        Self {
            preference: preference.to_owned(),
            ..Self::new(&tilo_history::window_language(Some(preference), shipped))
        }
    }

    /// The language the phrases are in (`en-US`).
    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale
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

impl tilo_formats::Words for Phrases {
    fn word(&self, key: &str, args: &[(&str, tilo_formats::Word)]) -> String {
        let args: Vec<(String, String)> = args
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    // A number stays a number here: this catalogue selects a plural by it.
                    tilo_formats::Word::Number(n) => n.to_string(),
                    tilo_formats::Word::Text(text) => text.clone(),
                };
                ((*name).to_owned(), value)
            })
            .collect();
        self.say(key, &args)
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
        let english_only = german_host.for_window(None, &["en-US"]);
        assert_eq!(english_only.locale(), "en-US");
        assert_eq!(english_only.say("tilolang-empty", &[]), "Nothing here yet");
        let both = german_host.for_window(None, &["en-US", "de-DE"]);
        assert_eq!(both.locale(), "de-DE", "the person's, when it ships it");
    }

    /// The window reads in the language the person's session names, whatever language this host's
    /// own process runs in: the host that draws a German window may well have started in the
    /// source language, and the app's words still come from its German catalogue.
    #[test]
    fn a_windows_language_is_the_one_the_session_names() {
        let english_host = Phrases::new("en-US");
        let german = english_host.for_window(Some("de-DE"), &["en-US", "de-DE"]);
        assert_eq!(german.locale(), "de-DE");
        german.add_package(&[("de-DE".to_owned(), "d-clean = Morgenroutine\n".to_owned())]);
        assert_eq!(german.say("d-clean", &[]), "Morgenroutine");
        assert_ne!(
            german.say("tilolang-empty", &[]),
            "Nothing here yet",
            "the frames' words follow the window too"
        );
        // A window whose package ships no German still reads the German the person types their
        // dates in: the language they prefer is theirs, whatever the window speaks.
        let english_only = english_host.for_window(Some("de-DE"), &["en-US"]);
        assert_eq!(english_only.locale(), "en-US");
        assert_eq!(english_only.language(), "de");
        // A language the host's own catalogue has no translation for is the app's to speak: the
        // window's language is the one chosen, never the host chrome's nearest one.
        let portuguese = english_host.for_window(Some("pt-BR"), &["en-US", "pt-BR"]);
        assert_eq!(portuguese.locale(), "pt-BR");
        portuguese.add_package(&[("pt-BR".to_owned(), "d-clean = Limpeza matinal\n".to_owned())]);
        assert_eq!(portuguese.say("d-clean", &[]), "Limpeza matinal");
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
