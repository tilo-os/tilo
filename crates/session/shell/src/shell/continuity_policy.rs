//! Device-local privacy policy for the personal continuity fabric.
//!
//! This is the single typed boundary between the settings document and shell
//! continuity brokers. Every capability is off unless the document opts it in
//! with a literal `true`. The master switch is on unless the document turns
//! it off: on its own it lets the person's linked devices find each other on
//! the local network and see each other's presence. A malformed section or a
//! non-boolean switch reads as off.

use serde_json::{Map, Value};

use super::device_fabric::PersonalFabricAction;

pub use settings_store::continuity_privacy::ContinuityCapability;

/// An immutable, typed view of the committed device-local policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContinuityPolicySnapshot {
    enabled: bool,
    automatic_display_input: bool,
    /// Each capability's own switch, one bit per [`ContinuityCapability`].
    allowed: u16,
}

/// The switches the section holds, besides `audit`.
const SWITCHES: [&str; 11] = [
    "enabled",
    "clipboard",
    "notifications",
    "wifi_settings",
    "media",
    "display_input",
    "automatic_display_input",
    "file_transfer",
    "realtime_sessions",
    "typed_gamepad",
    "app_continuations",
];

/// Whether the complete section is well formed: only known fields,
/// boolean switches and an audit list of entries. Malformed input cannot
/// enable discovery or release a continuity capability.
fn well_formed(section: &Map<String, Value>) -> bool {
    section.iter().all(|(name, value)| match name.as_str() {
        "audit" => value.as_array().is_some_and(|entries| {
            entries.iter().all(|entry| {
                entry.as_object().is_some_and(|entry| {
                    entry.get("recorded_at_secs").is_some_and(Value::is_u64)
                        && entry.get("enabled").is_some_and(Value::is_boolean)
                        && entry
                            .get("capability")
                            .is_none_or(|capability| capability.is_null() || capability.is_string())
                        && entry.keys().all(|key| {
                            matches!(key.as_str(), "recorded_at_secs" | "capability" | "enabled")
                        })
                })
            })
        }),
        name if SWITCHES.contains(&name) => value.is_boolean(),
        _ => false,
    })
}

impl ContinuityPolicySnapshot {
    /// Parse the settings document. Only literal booleans opt a capability in.
    #[must_use]
    pub fn from_document(document: &Map<String, Value>) -> Self {
        let section = match document.get("continuity") {
            None => {
                return Self {
                    enabled: true,
                    ..Self::default()
                };
            }
            Some(Value::Object(section)) if well_formed(section) => section,
            Some(_) => return Self::default(),
        };
        let flag = |name: &str| section.get(name).and_then(Value::as_bool).unwrap_or(false);
        Self {
            enabled: section
                .get("enabled")
                .is_none_or(|value| value == &Value::Bool(true)),
            automatic_display_input: flag("automatic_display_input"),
            allowed: ContinuityCapability::ALL
                .into_iter()
                .filter(|capability| flag(capability.key()))
                .fold(0, |allowed, capability| allowed | bit(capability)),
        }
    }

    #[must_use]
    pub fn load() -> Self {
        Self::from_document(&settings_store::load())
    }

    /// Automatic leases require a separate literal device-local opt-in.
    #[must_use]
    pub const fn automatic_display_input(self) -> bool {
        self.automatic_display_input
    }

    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn permits(self, capability: ContinuityCapability) -> bool {
        self.enabled && self.allowed & bit(capability) != 0
    }

    /// Whether applying `self` must tear down live state for a capability
    /// that the previously sampled policy allowed.
    #[must_use]
    pub const fn revoked_since(self, previous: Self, capability: ContinuityCapability) -> bool {
        previous.permits(capability) && !self.permits(capability)
    }

    /// Classify every fabric action at one exhaustive boundary. Adding a new
    /// wire action therefore requires an explicit privacy decision. `None`
    /// is device administration between the person's enrolled devices,
    /// which no optional continuity switch turns off.
    #[must_use]
    pub const fn capability_for_action(
        action: &PersonalFabricAction,
    ) -> Option<ContinuityCapability> {
        Some(match action {
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
            | PersonalFabricAction::BluetoothHandoffCancel { .. }
            // A media handoff's local file travels on the transfer lane.
            | PersonalFabricAction::NearbyTransferOffer {
                kind: crate::shell::nearby_transfer::TransferKind::MediaHandoff,
                ..
            } => ContinuityCapability::Media,
            PersonalFabricAction::SetDisplayPlacement { .. }
            | PersonalFabricAction::SetDisplayPlacementChecked { .. }
            | PersonalFabricAction::DeviceHealthProbe { .. }
            | PersonalFabricAction::DeviceHealthReport { .. }
            | PersonalFabricAction::InputLeaseGranted { .. }
            | PersonalFabricAction::InputLeaseRevoked { .. }
            | PersonalFabricAction::Input { .. }
            | PersonalFabricAction::SpatialInputProtocol { .. }
            | PersonalFabricAction::SpatialInputSequenced { .. } => ContinuityCapability::DisplayInput,
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
            // A legacy app keeps running on the device that hosts it; the
            // person's other devices show it through a stream of its window.
            PersonalFabricAction::WindowViews { .. } => ContinuityCapability::AppContinuations,
            // Waking a sleeping device is administration of the person's own
            // machines, available like presence itself.
            PersonalFabricAction::WakeDevice { .. }
            | PersonalFabricAction::WakeReport { .. }
            | PersonalFabricAction::WakeProbe { .. }
            | PersonalFabricAction::WakeProbeReply { .. } => return None,
        })
    }

    /// Whether `action` may be processed now. Transfer continuations carry
    /// no kind, so they pass while either transfer switch is on; the transfer
    /// state machine then enforces the switch of the transfer's own kind.
    #[must_use]
    pub const fn permits_action(self, action: &PersonalFabricAction) -> bool {
        if matches!(
            action,
            PersonalFabricAction::NearbyTransferReady { .. }
                | PersonalFabricAction::NearbyTransferCancel { .. }
                | PersonalFabricAction::NearbyTransferRestart { .. }
                | PersonalFabricAction::NearbyTransferResult { .. }
        ) {
            return self.permits(ContinuityCapability::FileTransfer)
                || self.permits(ContinuityCapability::Media);
        }
        match Self::capability_for_action(action) {
            Some(capability) => self.permits(capability),
            None => true,
        }
    }
}

/// `capability`'s bit in [`ContinuityPolicySnapshot::allowed`].
const fn bit(capability: ContinuityCapability) -> u16 {
    1 << capability as u16
}

/// The policy in a live settings document, parsed once per settings
/// generation — for a per-tick reader that must not read the file.
pub struct ContinuityPolicyCache {
    settings: settings_store::live::LiveSettings,
    seen: u64,
    policy: ContinuityPolicySnapshot,
}

impl ContinuityPolicyCache {
    #[must_use]
    pub fn new(settings: &settings_store::live::LiveSettings) -> Self {
        let snapshot = settings.snapshot();
        Self {
            settings: settings.clone(),
            seen: snapshot.generation,
            policy: ContinuityPolicySnapshot::from_document(&snapshot.document),
        }
    }

    /// The current policy: one atomic load unless the settings changed.
    pub fn current(&mut self) -> ContinuityPolicySnapshot {
        if self.settings.generation() != self.seen {
            let snapshot = self.settings.snapshot();
            self.seen = snapshot.generation;
            self.policy = ContinuityPolicySnapshot::from_document(&snapshot.document);
        }
        self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_cached_policy_follows_each_settings_generation() {
        use settings_store::SettingsSource as _;
        let source = std::sync::Arc::new(settings_store::HostDocument::new(Map::new(), |_| Ok(())));
        let settings = settings_store::live::LiveSettings::start_from(source.clone(), || {});
        let mut cache = ContinuityPolicyCache::new(&settings);
        assert!(!cache.current().permits(ContinuityCapability::TypedGamepad));

        let seen = settings.generation();
        source
            .update_section(
                "continuity",
                document(&json!({"enabled": true, "typed_gamepad": true})),
            )
            .unwrap();
        let _ = settings.wait_for_change(seen, tilo_test_env::hang_bound());
        assert!(cache.current().permits(ContinuityCapability::TypedGamepad));
    }

    fn document(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn automatic_display_input_requires_a_literal_separate_opt_in() {
        for automatic in [false, true] {
            let policy = ContinuityPolicySnapshot::from_document(&document(
                &json!({"continuity": {"enabled":true, "display_input":true, "automatic_display_input":automatic}}),
            ));
            assert_eq!(policy.automatic_display_input(), automatic);
            assert!(
                policy.permits(ContinuityCapability::DisplayInput),
                "the known device-local switch must not invalidate the capability policy"
            );
        }
        for value in [
            json!(null),
            json!(false),
            json!(1),
            json!("true"),
            json!({}),
        ] {
            let policy = ContinuityPolicySnapshot::from_document(&document(
                &json!({"continuity": {"enabled":true, "display_input":true, "automatic_display_input":value}}),
            ));
            assert!(!policy.automatic_display_input());
        }
        let absent = ContinuityPolicySnapshot::from_document(&document(
            &json!({"continuity": {"enabled":true,"display_input":true}}),
        ));
        assert!(!absent.automatic_display_input());
        let explicit = ContinuityPolicySnapshot::from_document(&document(
            &json!({"continuity": {"automatic_display_input":true}}),
        ));
        assert!(explicit.automatic_display_input());
        assert!(
            !explicit.permits(ContinuityCapability::DisplayInput),
            "the separate choice cannot enable its master capability"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuity_policy_fails_closed_and_opts_in_each_capability_under_the_master_switch() {
        fn absent_malformed_and_partial_policy_fail_closed() {
            for (value, master) in [
                (json!({}), true),
                (json!({"continuity": {}}), true),
                (json!({"continuity": true}), false),
                (json!({"continuity": {"enabled": "yes"}}), false),
                (
                    json!({"continuity": {"enabled": true, "unknown": 1}}),
                    false,
                ),
                (json!({"continuity": {"clipboard": "yes"}}), false),
                (json!({"continuity": {"audit": [{"enabled": true}]}}), false),
                (
                    json!({"continuity": {"audit": [
                        {"recorded_at_secs": 4, "capability": "clipboard", "enabled": true},
                        {"recorded_at_secs": 5, "capability": null, "enabled": false}
                    ]}}),
                    true,
                ),
                (json!({"continuity": {"enabled": true}}), true),
            ] {
                let policy = ContinuityPolicySnapshot::from_document(&document(&value));
                assert_eq!(policy.enabled(), master, "{value}");
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
                Some(ContinuityCapability::Media)
            );
        }
        for health in [&health_probe, &health_report] {
            assert_eq!(
                ContinuityPolicySnapshot::capability_for_action(health),
                Some(ContinuityCapability::DisplayInput)
            );
        }

        // Waking one of the person's devices needs no optional switch.
        let wake = PersonalFabricAction::WakeDevice {
            target: crate::shell::personal_continuity::DeviceId::new("desk").unwrap(),
        };
        assert_eq!(ContinuityPolicySnapshot::capability_for_action(&wake), None);
        assert!(policy(false, false).permits_action(&wake));
        assert!(ContinuityPolicySnapshot::default().permits_action(&wake));

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

    #[test]
    fn the_transfer_lane_answers_to_the_switch_of_each_transfer_kind() {
        use crate::shell::nearby_transfer::TransferKind;

        let offer = |kind| PersonalFabricAction::NearbyTransferOffer {
            transfer_id: [1; 16],
            name: "film.mkv".into(),
            kind,
            total_bytes: 4,
            content_hash: [2; 32],
        };
        let media_offer = offer(TransferKind::MediaHandoff);
        let file_offer = offer(TransferKind::File);
        let ready = PersonalFabricAction::NearbyTransferReady {
            request_id: [3; 16],
            transfer_id: [1; 16],
            committed_offset: 0,
            prefix_hash: [4; 32],
        };
        assert_eq!(
            ContinuityPolicySnapshot::capability_for_action(&media_offer),
            Some(ContinuityCapability::Media)
        );
        assert_eq!(
            ContinuityPolicySnapshot::capability_for_action(&file_offer),
            Some(ContinuityCapability::FileTransfer)
        );

        let media_only = policy(true, false);
        assert!(media_only.permits_action(&media_offer));
        assert!(!media_only.permits_action(&file_offer));
        assert!(media_only.permits_action(&ready));

        let file_only = ContinuityPolicySnapshot::from_document(&document(&json!({
            "continuity": {"enabled": true, "file_transfer": true}
        })));
        assert!(!file_only.permits_action(&media_offer));
        assert!(file_only.permits_action(&file_offer));
        assert!(file_only.permits_action(&ready));

        let neither = policy(false, true);
        assert!(!neither.permits_action(&media_offer));
        assert!(!neither.permits_action(&ready));
    }
}
