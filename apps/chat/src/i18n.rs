//! Chat's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
pub(crate) static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", &[("en-US", include_str!("../locales/en-US.ftl"))]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

/// Resolve a Chat string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a Chat string key that interpolates variables.
///
/// The values are the app's own words — an operation's label, a participant's
/// name — so the *sentence* around them stays in the catalogue and only the
/// nouns are substituted. Building the line with `format!` instead would put
/// English word order in the source and leave nothing to translate.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// The emoji catalogue in the desktop's language: names, group labels and
/// search follow it, with English kept for anything it does not translate.
pub fn emoji_catalogue() -> &'static emoji::Catalogue {
    emoji::Catalogue::for_locale(&ui::i18n::I18nManager::current_locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_app_uses_resolves_to_a_real_string() {
        // A key that resolves to itself is a key with no catalogue entry — the
        // failure mode that ships an untranslatable literal to a screen.
        for key in [
            "chat-voice-unavailable",
            "chat-attachment-import-unavailable",
            "chat-mention-agent-tag",
            "chat-person-unnamed",
            "chat-person-relative-name",
            "chat-identity-available",
            "chat-identity-in-voice",
            "chat-context-sharing",
            "chat-context-following",
            "chat-context-stop",
            "chat-header-call-open",
            "chat-header-voice-call",
            "chat-attachment-no-access",
            "chat-call-connected",
            "chat-call-reconnecting",
            "chat-call-joining",
            "chat-call-unavailable",
            "chat-call-voice-unavailable",
            "chat-call-open",
            "chat-call-join",
            "chat-call-leave-action",
            "chat-call-mute-action",
            "chat-call-unmute-action",
            "chat-call-status",
            "chat-call-select-input",
            "chat-call-input",
            "chat-call-input-hint",
            "chat-call-mute",
            "chat-call-unmute",
            "chat-call-deafen",
            "chat-call-undeafen",
            "chat-call-audio-only",
            "chat-call-collapse",
            "chat-call-leave",
            "chat-call-issue-unavailable",
            "chat-call-issue-denied",
            "chat-call-issue-permission-denied",
            "chat-call-issue-source-lost",
            "chat-call-issue-capture-failed",
            "chat-call-issue-transport-failed",
            "chat-call-issue-session-ended",
            "chat-call-issue-stale",
            "chat-emoji-insert",
            "chat-emoji-choose",
            "chat-emoji-group",
            "chat-reaction-add",
            "chat-reaction-remove",
            "chat-agent-work-taken",
            "chat-agent-work-waiting",
            "chat-agent-work-intent",
            "chat-agent-work-intent-about",
            "chat-agent-work-done",
            "chat-agent-work-refused",
            "chat-agent-work-model-rationale",
            "chat-agent-work-rule-match",
            "chat-agent-work-model-choice",
            "chat-agent-work-rule-choice",
            "chat-agent-work-rationale",
            "chat-agent-work-step-requested",
            "chat-agent-work-step-applied",
            "chat-agent-work-step-no-change",
            "chat-agent-work-step-refused",
            "chat-agent-work-step-refused-why",
            "chat-thread-title",
            "chat-thread-back",
            "chat-thread-close",
            "chat-thread-replies",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
    }
}

#[cfg(test)]
mod call_title_tests {
    #[test]
    fn call_title_interpolates_the_channel() {
        let title = super::t_args("chat-call-title", &[("channel", "standup".into())]);
        assert!(title.contains("standup"));
        assert!(!title.contains("chat-call-title"));
        assert!(!title.contains("$channel"));
    }
}
