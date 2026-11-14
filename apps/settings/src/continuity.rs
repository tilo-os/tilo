//! What Settings calls the kinds of Continuity and the changes to them; the
//! policy itself is [`settings_sections::continuity`]'s.

pub use settings_sections::continuity::*;

/// Messages are named after the capability's key: `continuity-wifi-settings`
/// and `continuity-wifi-settings-description`.
fn message(capability: Capability, suffix: &str) -> String {
    let key = capability.key().replace('_', "-");
    crate::i18n::t(&format!("continuity-{key}{suffix}"))
}

pub fn label(capability: Capability) -> String {
    message(capability, "")
}

pub fn description(capability: Capability) -> String {
    message(capability, "-description")
}

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
        let cx = crate::area::AreaContext::in_profile(dir.path().to_path_buf());
        let mut policy = ContinuityPrivacy {
            enabled: true,
            ..ContinuityPrivacy::default()
        };
        policy.set_allowed(Capability::FileTransfer, true);
        for second in 0..(AUDIT_LIMIT as u64 + 5) {
            policy.record(Some(Capability::FileTransfer), second % 2 == 0, second);
        }
        cx.storage
            .settings::<ContinuityPrivacy>()
            .set(&policy)
            .unwrap();

        let loaded = cx.storage.settings::<ContinuityPrivacy>().get();
        assert_eq!(loaded, policy);
        assert_eq!(loaded.audit.len(), AUDIT_LIMIT);
        assert_eq!(loaded.audit[0].recorded_at_secs, 5);
    }
}
