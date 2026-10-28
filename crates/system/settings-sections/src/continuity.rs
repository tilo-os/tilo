//! Device-local privacy policy for personal-device Continuity.
//!
//! Certified-device identity and transport stay owned by the existing device
//! fleet and relay.  This module stores only this machine's release policy:
//! which facets may leave or control it, plus a small non-secret audit trail.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

pub const AUDIT_LIMIT: usize = 24;

pub use settings_store::continuity_privacy::ContinuityCapability as Capability;

/// One bounded, deliberately low-detail local record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    pub recorded_at_secs: u64,
    pub capability: Option<Capability>,
    pub enabled: bool,
}

/// The release policy, the person's `continuity` settings. Each switch is on
/// until the person turns it off ([`settings_store::continuity_privacy::ON_UNLESS_SET`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
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

impl Default for ContinuityPrivacy {
    fn default() -> Self {
        let on = settings_store::continuity_privacy::ON_UNLESS_SET;
        Self {
            enabled: on,
            clipboard: on,
            notifications: on,
            wifi_settings: on,
            media: on,
            display_input: on,
            file_transfer: on,
            realtime_sessions: on,
            typed_gamepad: on,
            app_continuations: on,
            audit: Vec::new(),
        }
    }
}

impl ui::storage::SettingsSchema for ContinuityPrivacy {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("continuity");
    ui::settings_codec!();
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
        let excess = self.audit.len().saturating_sub(AUDIT_LIMIT);
        self.audit.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every facet is on until turned off, the master switch stops them all,
    /// and audit entries hold only time, capability and state.
    #[test]
    fn continuity_privacy_is_on_unless_turned_off_and_its_audit_is_payload_free() {
        {
            let mut policy = ContinuityPrivacy::default();
            for capability in Capability::ALL {
                assert!(policy.effective(capability));
            }
            policy.set_allowed(Capability::Clipboard, false);
            assert!(!policy.effective(Capability::Clipboard));
            assert!(policy.effective(Capability::Media));
            policy.enabled = false;
            policy.set_allowed(Capability::Clipboard, true);
            assert!(!policy.effective(Capability::Clipboard));
            assert!(!policy.effective(Capability::Media));
        }
        // A settings document written before a facet existed keeps it on.
        {
            let read: ContinuityPrivacy =
                serde_json::from_value(serde_json::json!({"enabled": true, "media": false}))
                    .unwrap();
            assert!(!read.effective(Capability::Media));
            assert!(read.effective(Capability::AppContinuations));
        }

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
}
