//! Device-local privacy policy for personal-device Continuity.
//!
//! Certified-device identity and transport stay owned by the existing device
//! fleet and relay.  This module stores only this machine's release policy:
//! which facets may leave or control it, plus a small non-secret audit trail.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

pub(crate) const AUDIT_LIMIT: usize = 24;

/// A separately revocable Continuity facet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Capability {
    Clipboard,
    Notifications,
    WifiSettings,
    Media,
    DisplayInput,
    FileTransfer,
    RealtimeSessions,
    TypedGamepad,
    AppContinuations,
}

impl Capability {
    pub(crate) const ALL: [Self; 9] = [
        Self::Clipboard,
        Self::Notifications,
        Self::WifiSettings,
        Self::Media,
        Self::DisplayInput,
        Self::FileTransfer,
        Self::RealtimeSessions,
        Self::TypedGamepad,
        Self::AppContinuations,
    ];

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Notifications => "notifications",
            Self::WifiSettings => "wifi_settings",
            Self::Media => "media",
            Self::DisplayInput => "display_input",
            Self::FileTransfer => "file_transfer",
            Self::RealtimeSessions => "realtime_sessions",
            Self::TypedGamepad => "typed_gamepad",
            Self::AppContinuations => "app_continuations",
        }
    }

    pub(crate) fn label(self) -> String {
        crate::i18n::t(match self {
            Self::Clipboard => "continuity-clipboard",
            Self::Notifications => "continuity-notifications",
            Self::WifiSettings => "continuity-wifi-settings",
            Self::Media => "continuity-media",
            Self::DisplayInput => "continuity-display-input",
            Self::FileTransfer => "continuity-file-transfer",
            Self::RealtimeSessions => "continuity-realtime-sessions",
            Self::TypedGamepad => "continuity-typed-gamepad",
            Self::AppContinuations => "continuity-app-continuations",
        })
    }

    pub(crate) fn description(self) -> String {
        crate::i18n::t(match self {
            Self::Clipboard => "continuity-clipboard-description",
            Self::Notifications => "continuity-notifications-description",
            Self::WifiSettings => "continuity-wifi-settings-description",
            Self::Media => "continuity-media-description",
            Self::DisplayInput => "continuity-display-input-description",
            Self::FileTransfer => "continuity-file-transfer-description",
            Self::RealtimeSessions => "continuity-realtime-sessions-description",
            Self::TypedGamepad => "continuity-typed-gamepad-description",
            Self::AppContinuations => "continuity-app-continuations-description",
        })
    }
}

/// One bounded, deliberately low-detail local record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditEntry {
    pub(crate) recorded_at_secs: u64,
    pub(crate) capability: Option<Capability>,
    pub(crate) enabled: bool,
}

impl AuditEntry {
    /// What changed, in words: `Clipboard allowed`, `All Continuity stopped`.
    pub(crate) fn summary(&self) -> String {
        let subject = self
            .capability
            .map_or_else(|| crate::i18n::t("continuity-all"), Capability::label);
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
    pub(crate) enabled: bool,
    #[model(readonly)]
    pub(crate) clipboard: bool,
    #[model(readonly)]
    pub(crate) notifications: bool,
    #[model(readonly)]
    pub(crate) wifi_settings: bool,
    #[model(readonly)]
    pub(crate) media: bool,
    #[model(readonly)]
    pub(crate) display_input: bool,
    #[model(readonly)]
    pub(crate) file_transfer: bool,
    #[model(readonly)]
    pub(crate) realtime_sessions: bool,
    #[model(readonly)]
    pub(crate) typed_gamepad: bool,
    #[model(readonly)]
    pub(crate) app_continuations: bool,
    #[model(readonly)]
    pub(crate) audit: Vec<AuditEntry>,
}

impl ui::storage::SettingsSchema for ContinuityPrivacy {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("continuity");
}

#[ui::tilo_resource::resource(kind = "settings.continuity", label = "Continuity")]
impl ContinuityPrivacy {}

impl ContinuityPrivacy {
    pub(crate) const fn allowed(&self, capability: Capability) -> bool {
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

    pub(crate) fn set_allowed(&mut self, capability: Capability, allowed: bool) {
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

    pub(crate) fn effective(&self, capability: Capability) -> bool {
        self.enabled && self.allowed(capability)
    }

    pub(crate) fn record(&mut self, capability: Option<Capability>, enabled: bool, now_secs: u64) {
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
