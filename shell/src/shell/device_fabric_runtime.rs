//! Native-session bridge between compositor state and the personal fabric.
//!
//! The bridge deliberately samples stable state rather than putting network or
//! hardware discovery on the render path. Remote clipboard entries become
//! history, not the live Wayland selection, until explicitly activated.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};

use crate::compositor::selection::{
    SharedSelection, merge_shared_clipboard_history, shareable_clipboard_history,
};
use crate::shell::device_fabric::{
    ClipboardEntry, ClipboardPayload, DeviceFabricSnapshot, DeviceKind, DevicePresence,
    DisplayPlacement, DisplayRect, FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp,
    MediaSession, RoamingSetting,
};
use crate::shell::network::ShellNetwork;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const PRESENCE_HEARTBEAT: Duration = Duration::from_secs(10);

/// Stable local facts supplied by the compositor without blocking discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalFabricCapabilities {
    pub display: Option<DisplayRect>,
    pub pointer: bool,
    pub keyboard: bool,
    pub audio_output: bool,
}

/// Stateful, low-frequency fabric pump owned by the native compositor loop.
pub struct DeviceFabricRuntime {
    counter: u64,
    last_sample: Option<Instant>,
    last_publish: Option<Instant>,
    local_fingerprint: Option<[u8; 32]>,
    merged_settings_fingerprint: Option<[u8; 32]>,
    applied_remote_settings: BTreeMap<String, Value>,
    settings_origin_initialized: bool,
}

impl DeviceFabricRuntime {
    pub fn new(network: &ShellNetwork) -> Self {
        let counter = network.device_fabric().max_counter();
        Self {
            counter,
            last_sample: None,
            last_publish: None,
            local_fingerprint: None,
            merged_settings_fingerprint: None,
            applied_remote_settings: BTreeMap::new(),
            settings_origin_initialized: false,
        }
    }

    /// Sample local state and project authenticated remote state into the OS.
    pub fn pump(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        local: LocalFabricCapabilities,
    ) {
        let now = Instant::now();
        if self
            .last_sample
            .is_some_and(|sample| now.duration_since(sample) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.last_sample = Some(now);
        let Some((device, label)) = network.local_device_fabric_identity() else {
            return;
        };
        if !self.settings_origin_initialized {
            self.seed_remote_setting_origins(network, &device);
            self.settings_origin_initialized = true;
        }
        self.counter = self.counter.max(network.device_fabric().max_counter());
        let previous = network.device_fabric().snapshots().get(&device).cloned();
        let tentative_stamp = FabricStamp {
            counter: self.counter.saturating_add(1).max(1),
            device: device.clone(),
        };
        let mut candidate = build_local_snapshot(
            &device,
            label,
            local,
            selection,
            previous.as_ref(),
            &self.applied_remote_settings,
            &tentative_stamp,
        );
        let neutral_stamp = FabricStamp {
            counter: 0,
            device: device.clone(),
        };
        stamp_snapshot(&mut candidate, &neutral_stamp, 0);
        let fingerprint = postcard::to_allocvec(&candidate)
            .ok()
            .map(|bytes| *blake3::hash(&bytes).as_bytes());
        let heartbeat_due = self
            .last_publish
            .is_none_or(|published| now.duration_since(published) >= PRESENCE_HEARTBEAT);
        if fingerprint != self.local_fingerprint || heartbeat_due {
            self.counter = tentative_stamp.counter;
            if fingerprint == self.local_fingerprint {
                if let Some(mut heartbeat) = previous {
                    heartbeat.presence.stamp = tentative_stamp;
                    heartbeat.presence.last_seen_unix_ms = unix_ms();
                    candidate = heartbeat;
                }
            } else {
                candidate.presence.stamp = tentative_stamp;
                candidate.presence.last_seen_unix_ms = unix_ms();
            }
            if network.upsert_local_device_fabric(candidate).is_ok() {
                self.local_fingerprint = fingerprint;
                self.last_publish = Some(now);
            }
        }
        self.merge_remote_state(network, selection, &device);
        if let Err(error) = network.refresh_wifi_credential_recipients() {
            tracing::warn!(%error, "could not refresh per-device Wi-Fi credential envelopes");
        }
    }

    fn seed_remote_setting_origins(
        &mut self,
        network: &ShellNetwork,
        local_device: &crate::shell::personal_continuity::DeviceId,
    ) {
        let current = roaming_setting_values();
        self.applied_remote_settings = network
            .device_fabric()
            .merged_setting_records()
            .into_iter()
            .filter(|(key, item)| {
                item.stamp.device != *local_device && current.get(key) == Some(&item.value)
            })
            .map(|(key, item)| (key, item.value))
            .collect();
    }

    fn merge_remote_state(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        local_device: &crate::shell::personal_continuity::DeviceId,
    ) {
        let fabric = network.device_fabric();
        let history = fabric.merged_clipboard();
        let entries = history.entries().cloned().collect::<Vec<_>>();
        merge_shared_clipboard_history(selection, &entries);

        let records = fabric.merged_setting_records();
        let fingerprint = serde_json::to_vec(&records)
            .ok()
            .map(|bytes| *blake3::hash(&bytes).as_bytes());
        if fingerprint != self.merged_settings_fingerprint {
            let merged = records
                .iter()
                .map(|(key, item)| (key.clone(), item.value.clone()))
                .collect();
            apply_roaming_settings(&merged);
            self.merged_settings_fingerprint = fingerprint;
        }
        self.applied_remote_settings = records
            .into_iter()
            .filter(|(_, item)| item.stamp.device != *local_device)
            .map(|(key, item)| (key, item.value))
            .collect();
    }
}

fn build_local_snapshot(
    device: &crate::shell::personal_continuity::DeviceId,
    label: String,
    local: LocalFabricCapabilities,
    selection: &SharedSelection,
    previous: Option<&DeviceFabricSnapshot>,
    applied_remote_settings: &BTreeMap<String, Value>,
    stamp: &FabricStamp,
) -> DeviceFabricSnapshot {
    let mut snapshot = DeviceFabricSnapshot {
        schema_version: FABRIC_SCHEMA_VERSION,
        owner: device.clone(),
        presence: DevicePresence {
            device: device.clone(),
            label,
            kind: local_device_kind(),
            capabilities: FabricCapabilities {
                display: local.display.is_some(),
                pointer: local.pointer,
                keyboard: local.keyboard,
                audio_output: local.audio_output,
                video_output: local.display.is_some(),
                clipboard: true,
                roaming_settings: true,
                secret_store: true,
                app_continuation: true,
            },
            online: true,
            last_seen_unix_ms: 0,
            stamp: stamp.clone(),
        },
        media: media::handoff::playback_sessions()
            .into_iter()
            .map(|session| MediaSession {
                id: session.id,
                app_id: session.app_id,
                title: session.title,
                content_id: session.content_id,
                position_ms: session.position_ms,
                duration_ms: session.duration_ms,
                playing: session.playing,
                has_video: session.has_video,
                output_device: device.clone(),
                stamp: stamp.clone(),
            })
            .collect(),
        clipboard: shareable_clipboard(device, selection, stamp.counter),
        settings: reconcile_roaming_settings(
            roaming_setting_values(),
            previous,
            applied_remote_settings,
            stamp,
        ),
        wifi: previous.map_or_else(Vec::new, |state| state.wifi.clone()),
        display: local.display.map(|rect| DisplayPlacement {
            device: device.clone(),
            rect,
            scale_milli: 1000,
            stamp: stamp.clone(),
        }),
    };
    preserve_unchanged_stamps(&mut snapshot, previous);
    snapshot
}

fn preserve_unchanged_stamps(
    candidate: &mut DeviceFabricSnapshot,
    previous: Option<&DeviceFabricSnapshot>,
) {
    let Some(previous) = previous else {
        return;
    };
    for item in &mut candidate.media {
        if let Some(old) = previous.media.iter().find(|old| old.id == item.id) {
            let mut comparable = item.clone();
            comparable.stamp = old.stamp.clone();
            if comparable == *old {
                item.stamp = old.stamp.clone();
            }
        }
    }
    for item in &mut candidate.clipboard {
        if let Some(old) = previous.clipboard.iter().find(|old| old.id == item.id) {
            let mut comparable = item.clone();
            comparable.stamp = old.stamp.clone();
            if comparable == *old {
                item.stamp = old.stamp.clone();
            }
        }
    }
    if let (Some(display), Some(old)) = (&mut candidate.display, &previous.display) {
        let mut comparable = display.clone();
        comparable.stamp = old.stamp.clone();
        if comparable == *old {
            display.stamp = old.stamp.clone();
        }
    }
}

fn stamp_snapshot(snapshot: &mut DeviceFabricSnapshot, stamp: &FabricStamp, now_unix_ms: u64) {
    snapshot.presence.stamp = stamp.clone();
    snapshot.presence.last_seen_unix_ms = now_unix_ms;
    for item in &mut snapshot.media {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.clipboard {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.settings {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.wifi {
        item.stamp = stamp.clone();
    }
    if let Some(display) = snapshot.display.as_mut() {
        display.stamp = stamp.clone();
    }
}

fn shareable_clipboard(
    device: &crate::shell::personal_continuity::DeviceId,
    selection: &SharedSelection,
    counter: u64,
) -> Vec<ClipboardEntry> {
    shareable_clipboard_history(selection)
        .into_iter()
        .filter_map(|item| {
            let payload = if item.mime == "text/plain;charset=utf-8" {
                ClipboardPayload::Text(String::from_utf8(item.bytes).ok()?)
            } else {
                ClipboardPayload::Binary {
                    mime: item.mime,
                    bytes: item.bytes,
                }
            };
            let mut entry = ClipboardEntry::shareable(
                payload,
                device.clone(),
                item.created_unix_ms,
                FabricStamp {
                    counter,
                    device: device.clone(),
                },
            )
            .ok()?;
            entry.pinned = item.pinned;
            Some(entry)
        })
        .collect()
}

fn roaming_setting_values() -> BTreeMap<String, Value> {
    let document = settings_store::load();
    let mut out = BTreeMap::new();
    for section in [
        "accessibility",
        "appearance",
        "region",
        "notifications",
        "input",
        "audio",
    ] {
        let Some(Value::Object(values)) = document.get(section) else {
            continue;
        };
        for (key, value) in values {
            if section == "appearance" && matches!(key.as_str(), "wallpaper" | "wallpaper_path") {
                continue;
            }
            if section == "audio" && !matches!(key.as_str(), "balance" | "mono_output") {
                continue;
            }
            out.insert(format!("qos.{section}.{key}"), value.clone());
        }
    }
    out
}

fn reconcile_roaming_settings(
    current: BTreeMap<String, Value>,
    previous: Option<&DeviceFabricSnapshot>,
    applied_remote: &BTreeMap<String, Value>,
    stamp: &FabricStamp,
) -> Vec<RoamingSetting> {
    current
        .into_iter()
        .filter_map(|(key, value)| {
            let old = previous
                .and_then(|snapshot| snapshot.settings.iter().find(|setting| setting.key == key));
            if let Some(remote_value) = applied_remote.get(&key) {
                if remote_value == &value {
                    return old.cloned();
                }
                // A value diverging from what this runtime last applied is an
                // explicit local edit, even when it happens to equal stale
                // local state from before the remote winner arrived.
                return Some(RoamingSetting {
                    key,
                    value,
                    stamp: stamp.clone(),
                });
            }
            if let Some(old) = old
                && old.value == value
            {
                return Some(old.clone());
            }
            Some(RoamingSetting {
                key,
                value,
                stamp: stamp.clone(),
            })
        })
        .collect()
}

fn apply_roaming_settings(settings: &BTreeMap<String, Value>) {
    let mut sections = BTreeMap::<String, Map<String, Value>>::new();
    for (key, value) in settings {
        let Some(rest) = key.strip_prefix("qos.") else {
            continue;
        };
        let Some((section, item)) = rest.split_once('.') else {
            continue;
        };
        sections
            .entry(section.to_string())
            .or_default()
            .insert(item.to_string(), value.clone());
    }
    for (section, values) in sections {
        if let Err(error) = settings_store::update_section(&section, values) {
            tracing::warn!(%error, section, "could not apply roaming device setting");
        }
    }
}

fn local_device_kind() -> DeviceKind {
    let chassis = std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok());
    match chassis {
        Some(8 | 9 | 10 | 14 | 30 | 31 | 32) => DeviceKind::Laptop,
        Some(11 | 28 | 29) => DeviceKind::Tablet,
        _ => DeviceKind::Desktop,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::personal_continuity::DeviceId;

    #[test]
    fn snapshot_stamping_updates_every_authored_record() {
        let device = DeviceId::new("laptop").unwrap();
        let mut snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: device.clone(),
            presence: DevicePresence {
                device: device.clone(),
                label: "Laptop".into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities::default(),
                online: true,
                last_seen_unix_ms: 0,
                stamp: FabricStamp {
                    counter: 0,
                    device: device.clone(),
                },
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
        };
        let stamp = FabricStamp { counter: 9, device };
        stamp_snapshot(&mut snapshot, &stamp, 42);
        assert_eq!(snapshot.presence.stamp, stamp);
        assert_eq!(snapshot.presence.last_seen_unix_ms, 42);
    }

    #[test]
    fn remotely_applied_setting_is_not_reauthored_but_a_user_edit_is() {
        let device = DeviceId::new("laptop").unwrap();
        let old_stamp = FabricStamp {
            counter: 3,
            device: device.clone(),
        };
        let mut previous = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: device.clone(),
            presence: DevicePresence {
                device: device.clone(),
                label: "Laptop".into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities::default(),
                online: true,
                last_seen_unix_ms: 0,
                stamp: old_stamp.clone(),
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: vec![RoamingSetting {
                key: "qos.appearance.dark".into(),
                value: Value::Bool(false),
                stamp: old_stamp,
            }],
            wifi: Vec::new(),
            display: None,
        };
        let next_stamp = FabricStamp { counter: 9, device };
        let applied_remote = BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(true))]);

        let received = reconcile_roaming_settings(
            BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(true))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert_eq!(received, previous.settings);

        let edited = reconcile_roaming_settings(
            BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(false))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert_eq!(edited[0].stamp, next_stamp);
        assert_eq!(edited[0].value, Value::Bool(false));

        previous.settings.clear();
        let bootstrap = reconcile_roaming_settings(
            BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(true))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert!(bootstrap.is_empty());
    }
}
