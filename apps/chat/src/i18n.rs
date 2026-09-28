//! Chat's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "chat-mention-agent-tag",
);

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

/// A word substituted into a catalogue sentence.
pub fn text(value: impl Into<String>) -> ArgValue {
    ArgValue::Text(value.into())
}

/// A count substituted into a catalogue sentence; plural forms select on it.
#[expect(
    clippy::cast_precision_loss,
    reason = "on-screen counts are far below 2^52, where f64 is exact"
)]
pub fn number(value: usize) -> ArgValue {
    ArgValue::Number(value as f64)
}

/// A yes/no switch for a catalogue `select`.
pub fn flag(value: bool) -> ArgValue {
    ArgValue::Text(if value { "yes" } else { "no" }.to_owned())
}

/// The emoji catalogue in the desktop's language: names, group labels and
/// search follow it, with English kept for anything it does not translate.
pub fn emoji_catalogue() -> &'static emoji::Catalogue {
    emoji::Catalogue::for_locale(&ui::i18n::I18nManager::current_locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key the app resolves.
    const APP_KEYS: &[&str] = &[
        "chat-mention-agent-tag",
        "chat-identity-available",
        "chat-identity-in-voice",
        "chat-call-mute",
        "chat-call-unmute",
        "chat-call-deafen",
        "chat-call-undeafen",
        "chat-call-audio-only",
        "chat-call-collapse",
        "chat-call-leave",
        "chat-emoji-insert",
        "chat-emoji-choose",
        "chat-emoji-group",
        "chat-reaction-add",
        "chat-reaction-remove",
        "chat-you",
        "chat-someone",
        "chat-workspace-fallback",
        "chat-workspace-current-fallback",
        "chat-presence-online",
        "chat-presence-idle",
        "chat-presence-dnd",
        "chat-presence-offline",
        "chat-member-you",
        "chat-member-agent",
        "chat-member-in-voice",
        "chat-roster-people",
        "chat-roster-collaborators",
        "chat-roster-subtitle",
        "chat-menu-rename",
        "chat-menu-delete",
        "chat-menu-move-up",
        "chat-menu-move-down",
        "chat-menu-new-text-channel",
        "chat-menu-new-voice-channel",
        "chat-menu-new-category",
        "chat-menu-toggle-collapse",
        "chat-menu-archive",
        "chat-menu-edit-message",
        "chat-menu-pin-message",
        "chat-menu-delete-message",
        "chat-default-category",
        "chat-default-voice-channel",
        "chat-default-new-voice-channel",
        "chat-inbox-title",
        "chat-inbox-subtitle",
        "chat-sidebar-title",
        "chat-online-count",
        "chat-header-from",
        "chat-header-source-topic",
        "chat-header-in-call",
        "chat-header-call",
        "chat-jump-latest",
        "chat-channel-start",
        "chat-channel-empty",
        "chat-day-today",
        "chat-day-yesterday",
        "chat-day-ago",
        "chat-edited-view-history",
        "chat-replies",
        "chat-last-reply",
        "chat-image-loading",
        "chat-image-pending",
        "chat-image-unavailable",
        "chat-image-decoding",
        "chat-video-meta",
        "chat-access-not-granted",
        "chat-view-only",
        "chat-edit-placeholder",
        "chat-edit-hint",
        "chat-edit-save",
        "chat-edit-cancel",
        "chat-composer-placeholder",
        "chat-thread-composer-placeholder",
        "chat-thread-title",
        "chat-thread-back",
        "chat-attach-title",
        "chat-attach-empty",
        "chat-attach-more",
        "chat-attach-loading",
        "chat-attach-preparing",
        "chat-attach-shared",
        "chat-attach-picker-exhausted",
        "chat-attach-picker-cancelled",
        "chat-attach-picker-failed",
        "chat-attach-import-exhausted",
        "chat-attach-unsupported-no-transport",
        "chat-attach-import-stale",
        "chat-attach-import-failed",
        "chat-attach-import-permission",
        "chat-attach-reason-workspace-unavailable",
        "chat-attach-reason-file-unavailable",
        "chat-attach-reason-not-shareable",
        "chat-attach-reason-unreadable",
        "chat-attach-reason-empty",
        "chat-attach-reason-too-large",
        "chat-attach-reason-store-refused",
        "chat-attach-reason-verification",
        "chat-attach-default-name",
        "chat-call-title-fallback",
        "chat-call-active",
        "chat-call-expand",
        "chat-call-camera-off",
        "chat-call-no-video",
        "chat-call-you",
        "chat-call-you-host",
        "chat-call-session-peer",
        "chat-call-session-host",
        "chat-call-panel-streams",
        "chat-call-audio-stream",
        "chat-call-audio-stream-name",
        "chat-call-headcount",
        "chat-call-headcount-sharing",
        "chat-call-sharing-you",
        "chat-call-sharing-other",
        "chat-call-sharing-you-sub",
        "chat-call-sharing-other-sub",
        "chat-call-live",
        "chat-call-action-join",
        "chat-call-action-leave",
        "chat-call-action-speak",
        "chat-call-action-camera",
        "chat-call-action-share",
        "chat-emoji-add-reaction",
        "chat-emoji-title",
        "chat-history-title",
        "chat-history-now",
        "chat-history-edited",
        "chat-a11y-inbox",
        "chat-a11y-current-context",
        "chat-a11y-channel",
        "chat-a11y-category",
        "chat-a11y-category-fallback",
        "chat-a11y-add-channel",
        "chat-a11y-add-category",
        "chat-a11y-rename-field",
        "chat-a11y-edit-message",
        "chat-a11y-composer",
        "chat-a11y-thread-composer",
        "chat-a11y-send",
        "chat-a11y-jump-latest",
        "chat-a11y-attach",
        "chat-a11y-attach-pick",
        "chat-a11y-start-call",
        "chat-a11y-show-navigation",
        "chat-a11y-show-detail",
        "chat-a11y-open-thread",
        "chat-a11y-close-thread",
        "chat-a11y-app",
        "chat-a11y-app-inbox",
        "chat-a11y-app-channels",
        "chat-a11y-messages",
        "chat-a11y-message",
        "chat-a11y-unknown-person",
        "chat-a11y-edit-history",
        "chat-a11y-edit-save",
        "chat-a11y-edit-cancel",
        "chat-a11y-call-mute",
        "chat-a11y-call-share",
        "chat-a11y-call-participants",
        "chat-a11y-call-expand",
        "chat-a11y-call-leave",
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

    #[test]
    fn plural_and_select_forms_read_as_english_sentences() {
        let channel = |inbox: bool, unread: usize| {
            t_args(
                "chat-a11y-channel",
                &[
                    ("glyph", text("#")),
                    ("name", text("design")),
                    ("inbox", flag(inbox)),
                    ("source", text("Studio")),
                    ("unread", number(unread)),
                ],
            )
        };
        assert_eq!(channel(false, 0), "# design");
        assert_eq!(channel(true, 3), "# design, from Studio, 3 unread");
        assert_eq!(t_args("chat-a11y-inbox", &[("unread", number(0))]), "Inbox");
        assert_eq!(
            t_args("chat-a11y-inbox", &[("unread", number(2))]),
            "Inbox, 2 unread"
        );
        let online = |agents: bool, count: usize| {
            t_args(
                "chat-online-count",
                &[("agents", flag(agents)), ("count", number(count))],
            )
        };
        assert_eq!(online(false, 1), "1 person online");
        assert_eq!(online(false, 4), "4 people online");
        assert_eq!(online(true, 2), "2 collaborators online");
        assert_eq!(t_args("chat-day-ago", &[("days", number(3))]), "3 days ago");
        assert_eq!(t_args("chat-replies", &[("count", number(1))]), "1 reply");
        assert_eq!(
            t_args(
                "chat-call-live",
                &[("me", flag(false)), ("name", text("Ada"))]
            ),
            "Live · Ada's screen"
        );
    }
}
