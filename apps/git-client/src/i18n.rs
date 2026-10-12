//! Fluent localization for Git.
//!
//! The catalogues are embedded so a live session, a `--shots` render and a
//! unit test resolve the same copy through the toolkit's [`mod@ui::i18n`]
//! manager. The locale follows the usual POSIX variables and falls back per
//! message to `en-US`.

ui::embedded_catalog!(pub fn, "git-remote-running", ["en-US", "de-DE"]);

/// Select `locale` (BCP-47 or POSIX) on this thread.
pub fn set_locale(locale: &str) {
    CATALOG.set_locale(locale);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::i18n::ArgValue;

    const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
    const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

    /// Every message the source catalogue has, the German one has too, so a
    /// German window never mixes languages.
    #[test]
    fn the_german_catalogue_covers_every_message() {
        let ids = |source: &str| -> Vec<String> {
            source
                .lines()
                .filter(|line| !line.starts_with([' ', '#', '*', '[', '}']) && line.contains(" ="))
                .filter_map(|line| line.split(" =").next().map(str::to_string))
                .collect()
        };
        let english = ids(SOURCE_FTL);
        let german = ids(DE_FTL);
        assert!(english.len() > 50);
        for id in &english {
            assert!(german.contains(id), "de-DE lacks {id}");
        }
        for id in &german {
            assert!(english.contains(id), "de-DE has {id}, which en-US lacks");
        }
    }

    /// Switching to German changes the copy, arguments included.
    #[test]
    fn german_resolves_with_arguments() {
        set_locale("de-DE");
        let title = t_args(
            "git-confirm-delete-branch-title",
            &[("name", ArgValue::from("main"))],
        );
        set_locale("en-US");
        assert!(title.contains("main"));
        assert_ne!(
            title,
            t_args(
                "git-confirm-delete-branch-title",
                &[("name", ArgValue::from("main"))]
            )
        );
    }
}
