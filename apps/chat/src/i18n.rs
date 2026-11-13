//! Chat's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

/// The emoji catalogue in the desktop's language, for Chat's own use: the
/// name of an emoji and of a group it paints in the accessibility tree,
/// with English kept for anything it does not translate.
///
/// The shared emoji picker and reaction strip do not read it: the toolkit
/// provides the catalogue they offer.
pub fn emoji_catalogue() -> &'static emoji::Catalogue {
    emoji::Catalogue::for_locale(&ui::i18n::I18nManager::current_locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_KEYS: &[&str] = &[
        "chat-voice-unavailable",
        "chat-call-not-joined",
        "chat-mention-agent-tag",
        "chat-person-unnamed",
        "chat-person-relative-name",
        "chat-identity-available",
        "chat-identity-in-voice",
        "chat-context-sharing",
        "chat-context-following",
        "chat-context-stop",
        "chat-header-call-open",
        "chat-attachment-no-access",
        "chat-call-connected",
        "chat-call-reconnecting",
        "chat-call-joining",
        "chat-call-unavailable",
        "chat-call-voice-unavailable",
        "chat-call-open",
        "chat-call-join",
        "chat-call-leave-action",
        "chat-call-status",
        "chat-call-people-two",
        "chat-call-people-many",
        "chat-call-only-you",
        "chat-call-open-window",
        "chat-call-untitled",
        "chat-call-camera-on",
        "chat-call-camera-off",
        "chat-call-mute",
        "chat-call-unmute",
        "chat-call-no-microphone",
        "chat-call-deafen",
        "chat-call-undeafen",
        "chat-call-audio-only",
        "chat-call-collapse",
        "chat-call-leave",
        "chat-call-issue-unavailable",
        "chat-call-issue-denied",
        "chat-call-issue-permission-denied",
        "chat-call-issue-camera-denied",
        "chat-call-issue-source-lost",
        "chat-call-issue-capture-failed",
        "chat-call-issue-transport-failed",
        "chat-call-issue-session-ended",
        "chat-call-issue-stale",
        "chat-call-issue-unsupported",
        "chat-emoji-choose",
        "chat-emoji-group",
        "chat-agent-work-waiting",
        "chat-agent-work-did",
        "chat-agent-work-refused",
        "chat-agent-work-step-requested",
        "chat-agent-work-step-applied",
        "chat-agent-work-step-no-change",
        "chat-agent-work-step-refused",
        "chat-agent-work-step-refused-why",
        "chat-agent-work-show-details",
        "chat-agent-work-hide-details",
        "chat-agent-kind",
        "chat-send",
        "chat-voice-channel",
        "chat-quote-caption",
        "chat-quote-earlier",
        "chat-failure-details",
        "chat-conversation-read-only",
        "chat-direct-unavailable",
        "chat-person-unknown",
        "chat-agent-author",
        "chat-thread-title",
        "chat-thread-close",
        "chat-thread-replies",
    ];

    #[test]
    fn every_key_the_app_uses_resolves_to_a_real_string() {
        // A key that resolves to itself is a key with no catalogue entry — the
        // failure mode that ships an untranslatable literal to a screen.
        for &key in APP_KEYS {
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
