//! Mail's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.

use ui::i18n::EmbeddedCatalog;

ui::embedded_catalog!(pub fn, "mail-setup-heading-add", ["en-US"]);

/// The catalogue, which every host mounting Mail loads before its view.
pub fn catalog() -> &'static EmbeddedCatalog {
    &CATALOG
}

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn catalogue_resolves_every_used_key_and_keeps_interpolated_sentences() {
        // Phase `every_key_the_app_uses_resolves_to_a_real_string`.
        {
            // A key that resolves to itself is a key with no catalogue entry — the
            // failure mode that ships an untranslatable literal to a screen.
            for key in [
                "mail-setup-heading-add",
                "mail-setup-heading-edit",
                "mail-setup-label-display-name",
                "mail-setup-label-email",
                "mail-setup-label-username",
                "mail-setup-label-password",
                "mail-setup-label-imap-host",
                "mail-setup-label-smtp-host",
                "mail-setup-label-sign-in",
                "mail-setup-auth-password",
                "mail-setup-auth-oauth",
                "mail-setup-tls",
                "mail-setup-advanced",
                "mail-setup-save",
                "mail-setup-cancel",
                "mail-setup-remove",
                "mail-compose-label-to",
                "mail-compose-label-cc",
                "mail-compose-label-bcc",
                "mail-compose-label-subject",
                "mail-compose-placeholder-to",
                "mail-compose-cc-bcc",
                "mail-compose-send",
                "mail-compose-save-draft",
                "mail-compose-discard",
                "mail-compose-prompt-link",
                "mail-compose-prompt-add",
                "mail-compose-prompt-cancel",
                "mail-setup-oauth-provider-fallback",
                "mail-setup-oauth-note-unavailable",
            ] {
                let s = t(key);
                assert_ne!(s, key, "{key} has no entry in the catalogue");
                assert!(!s.trim().is_empty(), "{key} resolves to nothing");
            }
        }

        // Phase `interpolated_keys_keep_the_sentence_in_the_catalogue`.
        {
            let s = t_args(
                "mail-setup-oauth-sign-in",
                &[("provider", ui::i18n::ArgValue::from("Gmail"))],
            );
            assert!(s.contains("Gmail"), "{s}");
            assert_ne!(s, "mail-setup-oauth-sign-in");
        }
    }
}
