//! What Settings calls the kinds of Continuity and the changes to them;
//! the policy itself is [`settings_sections::continuity`]'s.

pub use settings_sections::continuity::*;

/// What `capability` is called in Settings.
pub fn label(capability: Capability) -> String {
    crate::i18n::t(match capability {
        Capability::Clipboard => "continuity-clipboard",
        Capability::Notifications => "continuity-notifications",
        Capability::WifiSettings => "continuity-wifi-settings",
        Capability::Media => "continuity-media",
        Capability::DisplayInput => "continuity-display-input",
        Capability::FileTransfer => "continuity-file-transfer",
        Capability::RealtimeSessions => "continuity-realtime-sessions",
        Capability::TypedGamepad => "continuity-typed-gamepad",
        Capability::AppContinuations => "continuity-app-continuations",
    })
}

/// What allowing `capability` lets the person's other devices do.
pub fn description(capability: Capability) -> String {
    crate::i18n::t(match capability {
        Capability::Clipboard => "continuity-clipboard-description",
        Capability::Notifications => "continuity-notifications-description",
        Capability::WifiSettings => "continuity-wifi-settings-description",
        Capability::Media => "continuity-media-description",
        Capability::DisplayInput => "continuity-display-input-description",
        Capability::FileTransfer => "continuity-file-transfer-description",
        Capability::RealtimeSessions => "continuity-realtime-sessions-description",
        Capability::TypedGamepad => "continuity-typed-gamepad-description",
        Capability::AppContinuations => "continuity-app-continuations-description",
    })
}

/// What `entry` changed, in words: `Clipboard allowed`, `All Continuity stopped`.
pub fn summary(entry: &AuditEntry) -> String {
    let subject = entry
        .capability
        .map_or_else(|| crate::i18n::t("continuity-all"), label);
    crate::i18n::t_args(
        if entry.enabled {
            "continuity-audit-allowed"
        } else {
            "continuity-audit-stopped"
        },
        &[("subject", subject.into())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_is_bounded_and_round_trips_through_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let cx = crate::host::PaneCx::in_file(dir.path().join("settings.json"));
        let mut policy = ContinuityPrivacy {
            enabled: true,
            ..ContinuityPrivacy::default()
        };
        policy.set_allowed(Capability::FileTransfer, true);
        for second in 0..(AUDIT_LIMIT as u64 + 5) {
            policy.record(Some(Capability::FileTransfer), second % 2 == 0, second);
        }
        cx.settings::<ContinuityPrivacy>().set(&policy).unwrap();

        let loaded = cx.settings::<ContinuityPrivacy>().get();
        assert_eq!(loaded, policy);
        assert_eq!(loaded.audit.len(), AUDIT_LIMIT);
        assert_eq!(loaded.audit[0].recorded_at_secs, 5);
    }
}
