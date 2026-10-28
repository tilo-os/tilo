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

/// The phrases of one language.
pub struct Phrases {
    catalogue: Catalogue,
}

impl Phrases {
    /// The phrases of `locale` (`de-DE`, `de_DE.UTF-8`), or of the closest language there are
    /// phrases of: the same language, else the source's. A phrase a translation lacks reads in
    /// the source language.
    #[must_use]
    pub fn new(locale: &str) -> Self {
        let wanted = locale.replace('_', "-");
        let wanted = wanted.split('.').next().unwrap_or_default();
        let language = wanted.split('-').next().unwrap_or_default();
        let chosen = TRANSLATIONS
            .iter()
            .find(|(candidate, _)| *candidate == wanted)
            .or_else(|| {
                TRANSLATIONS
                    .iter()
                    .find(|(candidate, _)| candidate.split('-').next() == Some(language))
            })
            .unwrap_or(&SOURCE);
        let mut sources = vec![chosen.1];
        if chosen.0 != SOURCE.0 {
            sources.push(SOURCE.1);
        }
        Self {
            catalogue: Catalogue::new(chosen.0, &sources),
        }
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
            fluent.set(name.as_str(), value.as_str());
        }
        self.catalogue
            .format(id, &fluent)
            .unwrap_or_else(|| id.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_librarys_phrases_read_in_the_persons_language() {
        let english = Phrases::new("en-US");
        for id in [
            "tilolang-all",
            "tilolang-empty",
            "tilolang-more",
            "tilolang-none-selected",
            "tilolang-sort",
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
        assert_eq!(english.say("no-such-phrase", &[]), "no-such-phrase");
    }
}
