//! Chat's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.

ui::embedded_catalog!(pub fn, ["en-US"]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

/// The emoji catalogue in the desktop's language: names, group labels and
/// search follow it, with English kept for anything it does not translate.
pub fn emoji_catalogue() -> &'static emoji::Catalogue {
    emoji::Catalogue::for_locale(&ui::i18n::I18nManager::current_locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_KEYS: &[&str] = &[
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
        "chat-agent-work-show-details",
        "chat-agent-work-hide-details",
        "chat-agent-kind",
        "chat-agents-section",
        "chat-agent-message-placeholder",
        "chat-agent-empty-queue-title",
        "chat-agent-empty-queue-message",
        "chat-agent-empty-steer-title",
        "chat-agent-empty-steer-message",
        "chat-agent-empty-unavailable-title",
        "chat-agent-float",
        "chat-agent-return",
        "chat-agent-drag",
        "chat-agent-moving-title",
        "chat-agent-moving-message",
        "chat-agent-detached-title",
        "chat-agent-detached-message",
        "chat-agent-mode-queue",
        "chat-agent-mode-steer",
        "chat-agent-mode-steer-unavailable",
        "chat-agent-task-placeholder",
        "chat-agent-steer-placeholder",
        "chat-agent-send-queue",
        "chat-agent-send-steer",
        "chat-conversation-read-only",
        "chat-agent-unavailable",
        "chat-agent-task-loading",
        "chat-agent-task-none",
        "chat-agent-task-queued",
        "chat-agent-task-saved",
        "chat-agent-task-unavailable",
        "chat-agent-task-choose",
        "chat-agent-task-more",
        "chat-agent-task-next",
        "chat-agent-task-first",
        "chat-agent-task-phase-unknown",
        "chat-agent-task-accepted",
        "chat-agent-task-planning",
        "chat-agent-task-executing",
        "chat-agent-task-started",
        "chat-agent-task-stopped",
        "chat-agent-task-attention",
        "chat-agent-direction-waiting",
        "chat-agent-direction-applied",
        "chat-agent-direction-refused",
        "chat-agent-direction-outcome-unknown",
        "chat-agent-task-status",
        "chat-agent-task-output",
        "chat-agent-task-author",
        "chat-agent-open-task",
        "chat-agent-open-current-task",
        "chat-agent-open-task-center",
        "chat-agent-start",
        "chat-agent-start-unavailable",
        "chat-agent-stop",
        "chat-agent-select-task",
        "chat-agent-stop-current",
        "chat-thread-title",
        "chat-thread-back",
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
