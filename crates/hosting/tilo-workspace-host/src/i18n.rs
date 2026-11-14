//! The host's own words: the one name it gives rather than a person, in the
//! language of the person it founds the workspace for.

use tilo_history::{Catalogue, FluentArgs, Messages as _};

/// The message a workspace its founder did not name is called by.
const NAME: &str = "workspace-default-name";
/// The language this host writes in, which every translation falls back to.
const SOURCE: (&str, &str) = ("en-US", include_str!("../locales/en-US.ftl"));
/// The languages this host speaks beside its source language.
const TRANSLATIONS: &[(&str, &str)] = &[("de-DE", include_str!("../locales/de-DE.ftl"))];

/// The name a person's workspace is given when they do not name it, in
/// `language` (a BCP 47 tag, `de-DE`; the source language for one this host
/// has no catalogue of).
///
/// The founder's language, read once: the name is stored with the workspace,
/// and every member reads it as it was given.
pub(crate) fn workspace_name(language: Option<&str>) -> String {
    let shipped: Vec<&str> = std::iter::once(SOURCE.0)
        .chain(TRANSLATIONS.iter().map(|(tag, _)| *tag))
        .collect();
    let locale = tilo_history::window_language(language, &shipped);
    let chosen = TRANSLATIONS
        .iter()
        .find(|(tag, _)| *tag == locale)
        .unwrap_or(&SOURCE);
    // The source catalogue is read after the chosen one, so a message a
    // translation lacks still reads in the source language.
    let mut sources = vec![chosen.1];
    if chosen.0 != SOURCE.0 {
        sources.push(SOURCE.1);
    }
    let catalogue = Catalogue::new(chosen.0, &sources);
    catalogue
        .format(NAME, &FluentArgs::new())
        .unwrap_or_else(|| NAME.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_name_reads_in_the_founders_language() {
        assert_eq!(workspace_name(Some("de-DE")), "Mein Arbeitsbereich");
        assert_eq!(workspace_name(Some("de-AT")), "Mein Arbeitsbereich");
        assert_eq!(workspace_name(Some("en-US")), "My workspace");
        // A language this host has no catalogue of reads its source language.
        assert_eq!(workspace_name(Some("fr-FR")), "My workspace");
        assert_eq!(workspace_name(None), "My workspace");
    }
}
