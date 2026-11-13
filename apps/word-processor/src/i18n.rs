//! The word processor's user-facing strings, from Fluent catalogs embedded at build
//! time and resolved through [`t`] and [`t_args`]. A missing key falls back to English,
//! then to the key itself; the language is the person's, else the system's, else `en-US`.

use ui::i18n::ArgValue;

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

pub fn set_locale(locale: &str) {
    CATALOG.set_locale(locale);
}

pub fn current_locale() -> String {
    CATALOG.current_locale()
}

pub fn arg<V: Into<ArgValue>>(name: &str, value: V) -> (&str, ArgValue) {
    (name, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translations_resolve_per_locale_and_fall_back_without_panicking() {
        {
            set_locale("en-US");
            assert_eq!(t("toolbar-styles"), "Styles");
            assert_eq!(t("template-report"), "Report");
        }

        {
            set_locale("de-DE");
            assert_eq!(t("toolbar-styles"), "Stile");
            assert_eq!(t("template-report"), "Bericht");
            set_locale("de_DE.UTF-8");
            assert_eq!(t("template-report"), "Bericht");
            set_locale("en-US");
        }

        {
            set_locale("de-DE");
            assert_eq!(t("this.key.does.not.exist"), "this.key.does.not.exist");
            set_locale("en-US");
            assert_eq!(t("this.key.does.not.exist"), "this.key.does.not.exist");
        }

        {
            set_locale("de-DE");
            let v = t("layout-margins");
            assert_ne!(v, "layout-margins");
            assert!(!v.is_empty());
            set_locale("en-US");
        }

        {
            set_locale("en-US");
            let s = t_args("page-indicator", &[arg("n", 7)]);
            assert_eq!(s, "Page 7");

            set_locale("de-DE");
            let s = t_args("page-indicator", &[arg("n", 9)]);
            assert_eq!(s, "Seite 9");
            set_locale("en-US");
        }

        {
            set_locale("xx-ZZ");
            assert_eq!(t("template-blank"), "Blank");
            set_locale("en-US");
        }

        // A bundle drops an entry that fails to parse, which then renders as the bare key.
        {
            set_locale("en-US");

            assert_eq!(t_args("status-words", &[arg("words", 1)]), "1 word");
            assert_eq!(t_args("status-chars", &[arg("chars", 1)]), "1 character");
            assert_eq!(t_args("status-words", &[arg("words", 2)]), "2 words");
            assert_eq!(t_args("status-chars", &[arg("chars", 12)]), "12 characters");

            assert_eq!(t_args("status-words", &[arg("words", 0)]), "0 words");

            // German has the same one/other split, and `Zeichen` is invariant.
            set_locale("de-DE");
            assert_eq!(t_args("status-words", &[arg("words", 1)]), "1 Wort");
            assert_eq!(t_args("status-chars", &[arg("chars", 1)]), "1 Zeichen");
            assert_eq!(t_args("status-words", &[arg("words", 3)]), "3 Wörter");
            assert_eq!(t_args("status-chars", &[arg("chars", 9)]), "9 Zeichen");

            set_locale("en-US");
        }
    }
}
