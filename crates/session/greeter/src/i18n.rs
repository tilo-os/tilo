//! The sign-in screen's Fluent catalogue.
//!
//! Every string the greeter shows is a key here, resolved in the machine's
//! locale through the toolkit's [`mod@ui::i18n`] manager. The catalogues are
//! embedded, so a `--shots` render and a unit test resolve exactly what the
//! login screen does.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

/// Every key the greeter shows, in every shipped language.
#[cfg(test)]
pub(crate) const KEYS: &[&str] = &[
    "greeter-title",
    "greeter-username",
    "greeter-username-placeholder",
    "greeter-passcode",
    "greeter-passcode-placeholder",
    "greeter-sign-in",
    "greeter-signing-in",
    "greeter-starting",
    "greeter-use-fingerprint",
    "greeter-caps-lock",
    "greeter-session",
    "greeter-restart",
    "greeter-shut-down",
    "greeter-hint-username",
    "greeter-hint-passcode",
    "greeter-hint-no-fingerprint",
    "greeter-error-passcode",
    "greeter-error-credentials",
    "greeter-error-locked",
    "greeter-error-account-expired",
    "greeter-error-passcode-expired",
    "greeter-error-not-permitted",
    "greeter-error-unavailable",
    "greeter-error-session",
    "greeter-error-service",
    "greeter-error-no-session",
    "greeter-error-passcode-again",
    "greeter-suspend",
    "greeter-power",
    "greeter-larger-text",
    "greeter-high-contrast",
    "greeter-screen-reader",
    "greeter-accounts",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_resolves_in_every_language() {
        for (locale, source) in [
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ] {
            for key in KEYS {
                assert!(
                    source.lines().any(|line| line
                        .split_once(" = ")
                        .is_some_and(|(k, v)| k == *key && !v.trim().is_empty())),
                    "{key} has no {locale} text"
                );
            }
        }
        for key in KEYS {
            assert_ne!(t(key), *key, "{key} does not resolve");
        }
    }
}
