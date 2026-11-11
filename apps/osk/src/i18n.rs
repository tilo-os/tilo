//! The keyboard's Fluent catalogue, embedded so a `--shots` render, a unit
//! test and a live session resolve the same strings.

ui::embedded_catalog!(pub fn, ["de-DE", "en-US"]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_resolves_to_a_real_string() {
        for key in [
            "osk-emoji-key",
            "osk-emoji-panel",
            "osk-emoji-search",
            "osk-emoji-recent",
            "osk-emoji-no-results",
            "osk-emoji-letters",
            "osk-emoji-letters-name",
            "osk-emoji-delete",
            "osk-emoji-end-search",
            "osk-emoji-tones-hint",
            "osk-emoji-tone-light",
            "osk-emoji-tone-medium-light",
            "osk-emoji-tone-medium",
            "osk-emoji-tone-medium-dark",
            "osk-emoji-tone-dark",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
        let insert = t_args("osk-emoji-insert", &[("name", "pizza".into())]);
        assert!(insert.contains("pizza"), "{insert}");
    }
}

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-'Bildschirmtastatur'.ftl`, never from the English source
/// catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("osk-name"), "Bildschirmtastatur");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("osk-name"), "On-screen keyboard");
    }
}
