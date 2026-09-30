//! Device-local privacy policy for the personal continuity fabric.
//!
//! This is the single typed boundary between the settings document and shell
//! continuity brokers. Missing, malformed, or partially-written settings are
//! intentionally interpreted as disabled.

use serde_json::{Map, Value};

use super::device_fabric::PersonalFabricAction;

/// Independently switchable continuity data planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuityCapability {
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

impl ContinuityCapability {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Notifications => "notification",
            Self::WifiSettings => "Wi-Fi/settings",
            Self::Media => "media",
            Self::DisplayInput => "display/input",
            Self::FileTransfer => "file-transfer",
            Self::RealtimeSessions => "realtime-session",
            Self::TypedGamepad => "typed-gamepad",
            Self::AppContinuations => "app-continuation",
        }
    }
}

/// An immutable, typed view of the committed device-local policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "privacy capabilities are independent opt-in switches"
)]
pub struct ContinuityPolicySnapshot {
    enabled: bool,
    clipboard: bool,
    notifications: bool,
    wifi_settings: bool,
    media: bool,
    display_input: bool,
    file_transfer: bool,
    realtime_sessions: bool,
    typed_gamepad: bool,
    app_continuations: bool,
}

impl ContinuityPolicySnapshot {
    /// Parse the settings document. Only literal booleans opt a capability in.
    #[must_use]
    pub fn from_document(document: &Map<String, Value>) -> Self {
        let Some(Value::Object(section)) = document.get("continuity") else {
            return Self::default();
        };
        let flag = |name: &str| section.get(name).and_then(Value::as_bool).unwrap_or(false);
        Self {
            enabled: flag("enabled"),
            clipboard: flag("clipboard"),
            notifications: flag("notifications"),
            wifi_settings: flag("wifi_settings"),
            media: flag("media"),
            display_input: flag("display_input"),
            file_transfer: flag("file_transfer"),
            realtime_sessions: flag("realtime_sessions"),
            typed_gamepad: flag("typed_gamepad"),
            app_continuations: flag("app_continuations"),
        }
    }

    #[must_use]
    pub fn load() -> Self {
        Self::from_document(&settings_store::load())
    }

    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn permits(self, capability: ContinuityCapability) -> bool {
        self.enabled
            && match capability {
                ContinuityCapability::Clipboard => self.clipboard,
                ContinuityCapability::Notifications => self.notifications,
                ContinuityCapability::WifiSettings => self.wifi_settings,
                ContinuityCapability::Media => self.media,
                ContinuityCapability::DisplayInput => self.display_input,
                ContinuityCapability::FileTransfer => self.file_transfer,
                ContinuityCapability::RealtimeSessions => self.realtime_sessions,
                ContinuityCapability::TypedGamepad => self.typed_gamepad,
                ContinuityCapability::AppContinuations => self.app_continuations,
            }
    }

    /// Whether applying `self` must tear down live state for a capability
    /// that the previously sampled policy allowed.
    #[must_use]
    pub const fn revoked_since(self, previous: Self, capability: ContinuityCapability) -> bool {
        previous.permits(capability) && !self.permits(capability)
    }

    /// Classify every fabric action at one exhaustive boundary. Adding a new
    /// wire action therefore requires an explicit privacy decision.
    #[must_use]
    pub const fn capability_for_action(action: &PersonalFabricAction) -> ContinuityCapability {
        match action {
            PersonalFabricAction::ResumeMedia { .. }
            | PersonalFabricAction::MediaAccepted { .. }
            | PersonalFabricAction::MediaRejected { .. }
            | PersonalFabricAction::MediaCancelled { .. }
            | PersonalFabricAction::RemoteAudioOffer { .. }
            | PersonalFabricAction::RemoteAudioDecision { .. }
            | PersonalFabricAction::RemoteAudioFrame { .. }
            | PersonalFabricAction::RemoteAudioStop { .. }
            | PersonalFabricAction::RemoteAudioKeepalive { .. }
            | PersonalFabricAction::RemoteAudioCancel { .. }
            | PersonalFabricAction::SpeakerGroupInvite { .. }
            | PersonalFabricAction::SpeakerGroupDecision { .. }
            | PersonalFabricAction::SpeakerGroupLeave { .. }
            | PersonalFabricAction::SpeakerGroupSessionOffer { .. }
            | PersonalFabricAction::SpeakerGroupSessionDecision { .. }
            | PersonalFabricAction::SpeakerGroupSessionStop { .. }
            | PersonalFabricAction::SpeakerIdentify { .. }
            | PersonalFabricAction::SpeakerSweep { .. }
            // Speaker groups and remote audio play against the fabric clock,
            // so its probes are part of Media, not of display health.
            | PersonalFabricAction::FabricClockProbe { .. }
            | PersonalFabricAction::FabricClockReport { .. }
            | PersonalFabricAction::SpeakerListenInvite { .. }
            | PersonalFabricAction::SpeakerListenDecision { .. }
            | PersonalFabricAction::SpeakerListen { .. }
            | PersonalFabricAction::SpeakerHeard { .. }
            | PersonalFabricAction::RemoteCaptureSignal { .. }
            | PersonalFabricAction::RemoteCaptureChunk { .. }
            | PersonalFabricAction::BluetoothHandoffPrepare { .. }
            | PersonalFabricAction::BluetoothHandoffPrepared { .. }
            | PersonalFabricAction::BluetoothHandoffCommit { .. }
            | PersonalFabricAction::BluetoothHandoffFinished { .. }
            | PersonalFabricAction::BluetoothHandoffAccepted { .. }
            | PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
            | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }
            | PersonalFabricAction::BluetoothHandoffCancel { .. } => ContinuityCapability::Media,
            PersonalFabricAction::DisplayMirrorRequest { .. }
            | PersonalFabricAction::DisplayMirrorDecision { .. }
            | PersonalFabricAction::DisplayMirrorStop { .. }
            | PersonalFabricAction::SetDisplayPlacement { .. }
            | PersonalFabricAction::DeviceHealthProbe { .. }
            | PersonalFabricAction::DeviceHealthReport { .. }
            | PersonalFabricAction::InputLeaseGranted { .. }
            | PersonalFabricAction::InputLeaseRevoked { .. }
            | PersonalFabricAction::Input { .. } => ContinuityCapability::DisplayInput,
            PersonalFabricAction::NearbyDrop { .. }
            | PersonalFabricAction::NearbyDropResult { .. }
            | PersonalFabricAction::NearbyTransferOffer { .. }
            | PersonalFabricAction::NearbyTransferReady { .. }
            | PersonalFabricAction::NearbyTransferCancel { .. }
            | PersonalFabricAction::NearbyTransferRestart { .. }
            | PersonalFabricAction::NearbyTransferResult { .. } => {
                ContinuityCapability::FileTransfer
            }
            PersonalFabricAction::NotificationProject { .. }
            | PersonalFabricAction::NotificationRetire { .. }
            | PersonalFabricAction::NotificationDismiss { .. }
            | PersonalFabricAction::NotificationInvoke { .. } => {
                ContinuityCapability::Notifications
            }
            PersonalFabricAction::RealtimeOffer { .. }
            | PersonalFabricAction::RealtimeAccept { .. }
            | PersonalFabricAction::RealtimeMediaReady { .. }
            | PersonalFabricAction::RealtimeStopSource { .. }
            | PersonalFabricAction::RealtimeCancel { .. } => ContinuityCapability::RealtimeSessions,
            PersonalFabricAction::GamepadForwardRequest { .. }
            | PersonalFabricAction::GamepadForwardGrant { .. }
            | PersonalFabricAction::GamepadForwardEvent { .. }
            | PersonalFabricAction::GamepadForwardStop { .. } => ContinuityCapability::TypedGamepad,
            PersonalFabricAction::Tethering { .. } => ContinuityCapability::WifiSettings,
        }
    }

    #[must_use]
    pub const fn permits_action(self, action: &PersonalFabricAction) -> bool {
        self.permits(Self::capability_for_action(action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuity_policy_fails_closed_and_opts_in_each_capability_under_the_master_switch() {
        fn absent_malformed_and_partial_policy_fail_closed() {
            for value in [
                json!({}),
                json!({"continuity": true}),
                json!({"continuity": {"enabled": true}}),
            ] {
                let policy = ContinuityPolicySnapshot::from_document(&document(&value));
                for capability in [
                    ContinuityCapability::Clipboard,
                    ContinuityCapability::Notifications,
                    ContinuityCapability::WifiSettings,
                    ContinuityCapability::Media,
                    ContinuityCapability::DisplayInput,
                    ContinuityCapability::FileTransfer,
                    ContinuityCapability::RealtimeSessions,
                    ContinuityCapability::TypedGamepad,
                    ContinuityCapability::AppContinuations,
                ] {
                    assert!(!policy.permits(capability), "{capability:?} leaked");
                }
            }
        }

        fn master_switch_overrides_the_full_capability_matrix() {
            let all = json!({"continuity": {
                "enabled": false,
                "clipboard": true,
                "notifications": true,
                "wifi_settings": true,
                "media": true,
                "display_input": true,
                "file_transfer": true,
                "realtime_sessions": true,
                "typed_gamepad": true,
                "app_continuations": true
            }});
            let policy = ContinuityPolicySnapshot::from_document(&document(&all));
            assert!(!policy.enabled());
            assert!(!policy.permits(ContinuityCapability::Clipboard));
            assert!(!policy.permits(ContinuityCapability::TypedGamepad));
            assert!(!policy.permits(ContinuityCapability::AppContinuations));
        }

        fn each_capability_is_independently_opted_in() {
            let names = [
                ("clipboard", ContinuityCapability::Clipboard),
                ("notifications", ContinuityCapability::Notifications),
                ("wifi_settings", ContinuityCapability::WifiSettings),
                ("media", ContinuityCapability::Media),
                ("display_input", ContinuityCapability::DisplayInput),
                ("file_transfer", ContinuityCapability::FileTransfer),
                ("realtime_sessions", ContinuityCapability::RealtimeSessions),
                ("typed_gamepad", ContinuityCapability::TypedGamepad),
                ("app_continuations", ContinuityCapability::AppContinuations),
            ];
            for (enabled_name, enabled_capability) in names {
                let mut section = Map::new();
                section.insert("enabled".into(), Value::Bool(true));
                section.insert(enabled_name.into(), Value::Bool(true));
                let mut root = Map::new();
                root.insert("continuity".into(), Value::Object(section));
                let policy = ContinuityPolicySnapshot::from_document(&root);
                for (_, capability) in names {
                    assert_eq!(policy.permits(capability), capability == enabled_capability);
                }
            }
        }

        fn per_capability_and_master_kill_switches_report_revocation() {
            let all = document(&json!({"continuity": {
                "enabled": true,
                "clipboard": true,
                "notifications": true,
                "wifi_settings": true,
                "media": true,
                "display_input": true,
                "file_transfer": true,
                "realtime_sessions": true,
                "typed_gamepad": true,
                "app_continuations": true
            }}));
            let old = ContinuityPolicySnapshot::from_document(&all);
            let capabilities = [
                ("clipboard", ContinuityCapability::Clipboard),
                ("notifications", ContinuityCapability::Notifications),
                ("wifi_settings", ContinuityCapability::WifiSettings),
                ("media", ContinuityCapability::Media),
                ("display_input", ContinuityCapability::DisplayInput),
                ("file_transfer", ContinuityCapability::FileTransfer),
                ("realtime_sessions", ContinuityCapability::RealtimeSessions),
                ("typed_gamepad", ContinuityCapability::TypedGamepad),
                ("app_continuations", ContinuityCapability::AppContinuations),
            ];
            for (name, capability) in capabilities {
                let mut next = all.clone();
                next.get_mut("continuity")
                    .and_then(Value::as_object_mut)
                    .unwrap()
                    .insert(name.into(), Value::Bool(false));
                let next = ContinuityPolicySnapshot::from_document(&next);
                for (_, candidate) in capabilities {
                    assert_eq!(next.revoked_since(old, candidate), candidate == capability);
                }
            }

            let mut disabled = all;
            disabled
                .get_mut("continuity")
                .and_then(Value::as_object_mut)
                .unwrap()
                .insert("enabled".into(), Value::Bool(false));
            let disabled = ContinuityPolicySnapshot::from_document(&disabled);
            for (_, capability) in capabilities {
                assert!(disabled.revoked_since(old, capability));
            }
        }

        absent_malformed_and_partial_policy_fail_closed();
        master_switch_overrides_the_full_capability_matrix();
        each_capability_is_independently_opted_in();
        per_capability_and_master_kill_switches_report_revocation();
    }

    fn policy(media: bool, display_input: bool) -> ContinuityPolicySnapshot {
        ContinuityPolicySnapshot::from_document(&document(&json!({
            "continuity": {"enabled": true, "media": media, "display_input": display_input}
        })))
    }

    #[test]
    fn the_fabric_clock_is_media_and_device_health_stays_display_input() {
        let clock_probe = PersonalFabricAction::FabricClockProbe {
            nonce: [1; 16],
            sent_us: 10,
        };
        let clock_report = PersonalFabricAction::FabricClockReport {
            report: crate::shell::fabric_clock::ClockReport {
                probe_nonce: [1; 16],
                probe_sent_us: 10,
                received_us: 20,
                replied_us: 21,
            },
        };
        let health_probe = PersonalFabricAction::DeviceHealthProbe { nonce: [2; 16] };
        let health_report = PersonalFabricAction::DeviceHealthReport {
            report: crate::shell::spatial_calibration::DeviceHealthReport {
                probe_nonce: [2; 16],
                power: crate::shell::spatial_calibration::DevicePower::Battery(50),
                energy_saver: false,
            },
        };
        for clock in [&clock_probe, &clock_report] {
            assert_eq!(
                ContinuityPolicySnapshot::capability_for_action(clock),
                ContinuityCapability::Media
            );
        }
        for health in [&health_probe, &health_report] {
            assert_eq!(
                ContinuityPolicySnapshot::capability_for_action(health),
                ContinuityCapability::DisplayInput
            );
        }

        let media_only = policy(true, false);
        assert!(media_only.permits_action(&clock_probe));
        assert!(media_only.permits_action(&clock_report));
        assert!(!media_only.permits_action(&health_probe));
        assert!(!media_only.permits_action(&health_report));

        let display_only = policy(false, true);
        assert!(!display_only.permits_action(&clock_probe));
        assert!(!display_only.permits_action(&clock_report));
        assert!(display_only.permits_action(&health_probe));
        assert!(display_only.permits_action(&health_report));
    }
}
