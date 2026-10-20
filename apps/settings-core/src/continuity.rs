//! Device-local privacy policy for personal-device Continuity.
//!
//! Certified-device identity and transport stay owned by the existing device
//! fleet and relay.  This module stores only this machine's release policy:
//! which facets may leave or control it, plus a small non-secret audit trail.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

pub const AUDIT_LIMIT: usize = 24;

pub use settings_store::continuity_privacy::ContinuityCapability as Capability;

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

/// One bounded, deliberately low-detail local record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    pub recorded_at_secs: u64,
    pub capability: Option<Capability>,
    pub enabled: bool,
}

impl AuditEntry {
    /// What changed, in words: `Clipboard allowed`, `All Continuity stopped`.
    pub fn summary(&self) -> String {
        let subject = self
            .capability
            .map_or_else(|| crate::i18n::t("continuity-all"), label);
        crate::i18n::t_args(
            if self.enabled {
                "continuity-audit-allowed"
            } else {
                "continuity-audit-stopped"
            },
            &[("subject", subject.into())],
        )
    }
}

/// The release policy, the person's `continuity` settings.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(default, deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently revocable, serialized privacy capability"
)]
pub struct ContinuityPrivacy {
    #[model(readonly)]
    pub enabled: bool,
    #[model(readonly)]
    pub clipboard: bool,
    #[model(readonly)]
    pub notifications: bool,
    #[model(readonly)]
    pub wifi_settings: bool,
    #[model(readonly)]
    pub media: bool,
    #[model(readonly)]
    pub display_input: bool,
    #[model(readonly)]
    pub file_transfer: bool,
    #[model(readonly)]
    pub realtime_sessions: bool,
    #[model(readonly)]
    pub typed_gamepad: bool,
    #[model(readonly)]
    pub app_continuations: bool,
    #[model(readonly)]
    pub audit: Vec<AuditEntry>,
}

impl ui::storage::SettingsSchema for ContinuityPrivacy {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("continuity");
}

#[ui::tilo_resource::resource(kind = "settings.continuity", label = "Continuity")]
impl ContinuityPrivacy {
    /// Allow continuing activities between the person's devices, or stop;
    /// the audit trail notes when.
    #[action(label = "Allow Continuity")]
    pub fn allow_continuity(&mut self, cx: &mut tilo_resource::ActionCx, on: bool) {
        self.enabled = on;
        self.record(None, on, action_secs(cx));
    }

    /// Allow one kind of Continuity (clipboard, notifications, media…), or
    /// stop; the audit trail notes when.
    #[action(label = "Allow a kind of Continuity")]
    pub fn allow_capability(
        &mut self,
        cx: &mut tilo_resource::ActionCx,
        #[param(
            "The kind, by its key: clipboard, notifications, wifi_settings, media, display_input, file_transfer, realtime_sessions, typed_gamepad or app_continuations"
        )]
        capability: String,
        on: bool,
    ) -> Result<(), tilo_resource::Refusal> {
        let capability = Capability::ALL
            .into_iter()
            .find(|known| known.key() == capability)
            .ok_or_else(|| tilo_resource::Refusal::new("unsupported Continuity capability"))?;
        self.set_allowed(capability, on);
        self.record(Some(capability), on, action_secs(cx));
        Ok(())
    }
}

/// When the action was authored, in seconds since the Unix epoch.
fn action_secs(cx: &tilo_resource::ActionCx) -> u64 {
    u64::try_from(cx.now_ms() / 1000).unwrap_or(0)
}

impl ContinuityPrivacy {
    pub const fn allowed(&self, capability: Capability) -> bool {
        match capability {
            Capability::Clipboard => self.clipboard,
            Capability::Notifications => self.notifications,
            Capability::WifiSettings => self.wifi_settings,
            Capability::Media => self.media,
            Capability::DisplayInput => self.display_input,
            Capability::FileTransfer => self.file_transfer,
            Capability::RealtimeSessions => self.realtime_sessions,
            Capability::TypedGamepad => self.typed_gamepad,
            Capability::AppContinuations => self.app_continuations,
        }
    }

    pub fn set_allowed(&mut self, capability: Capability, allowed: bool) {
        let slot = match capability {
            Capability::Clipboard => &mut self.clipboard,
            Capability::Notifications => &mut self.notifications,
            Capability::WifiSettings => &mut self.wifi_settings,
            Capability::Media => &mut self.media,
            Capability::DisplayInput => &mut self.display_input,
            Capability::FileTransfer => &mut self.file_transfer,
            Capability::RealtimeSessions => &mut self.realtime_sessions,
            Capability::TypedGamepad => &mut self.typed_gamepad,
            Capability::AppContinuations => &mut self.app_continuations,
        };
        *slot = allowed;
    }

    pub fn effective(&self, capability: Capability) -> bool {
        self.enabled && self.allowed(capability)
    }

    pub fn record(&mut self, capability: Option<Capability>, enabled: bool, now_secs: u64) {
        self.audit.push(AuditEntry {
            recorded_at_secs: now_secs,
            capability,
            enabled,
        });
        if self.audit.len() > AUDIT_LIMIT {
            self.audit.drain(..self.audit.len() - AUDIT_LIMIT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Privacy facets are opt-in behind a master switch, and audit entries hold only time, capability and state.
    #[test]
    fn continuity_privacy_is_opt_in_and_its_audit_is_payload_free() {
        // Case `privacy_sensitive_facets_are_opt_in_and_master_kill_switch_wins`.
        {
            let mut policy = ContinuityPrivacy::default();
            for capability in Capability::ALL {
                assert!(!policy.allowed(capability));
                assert!(!policy.effective(capability));
            }
            policy.set_allowed(Capability::Clipboard, true);
            assert!(!policy.effective(Capability::Clipboard));
            policy.enabled = true;
            assert!(policy.effective(Capability::Clipboard));
        }

        // Case `audit_schema_cannot_hold_payloads_tokens_or_device_addresses`.
        {
            let entry = AuditEntry {
                recorded_at_secs: 42,
                capability: Some(Capability::Clipboard),
                enabled: true,
            };
            let encoded = serde_json::to_value(entry).unwrap();
            let object = encoded.as_object().unwrap();
            assert_eq!(object.len(), 3);
            assert!(object.contains_key("recorded_at_secs"));
            assert!(object.contains_key("capability"));
            assert!(object.contains_key("enabled"));
        }
    }

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
