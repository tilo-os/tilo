//! Kernel and `PipeWire` privacy state for shell indicators.

use std::collections::HashMap;
use std::path::Path;

use crate::rfkill::{self, RfkillState};

pub use media::audio::capture::MediaKind;

/// One actively-running `PipeWire` capture stream. `label` is derived from the
/// executable behind `PipeWire`'s server-assigned security PID, never from the
/// client-supplied `application.name`; an app cannot rename itself to another
/// app in the indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveMediaUse {
    pub client_id: u32,
    pub pid: u32,
    pub app_id: String,
    pub label: String,
    pub binary: std::path::PathBuf,
    pub kind: MediaKind,
}

impl From<media::audio::capture::MediaUse> for ActiveMediaUse {
    fn from(use_: media::audio::capture::MediaUse) -> Self {
        Self {
            client_id: use_.client_id,
            pid: use_.pid,
            app_id: use_.app_id,
            label: use_.label,
            binary: use_.binary,
            kind: use_.kind,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillState {
    Unavailable,
    Allowed,
    SoftwareBlocked,
    HardwareBlocked,
}

impl From<RfkillState> for KillState {
    fn from(state: RfkillState) -> Self {
        if !state.present {
            Self::Unavailable
        } else if state.hard_blocked {
            Self::HardwareBlocked
        } else if state.soft_blocked {
            Self::SoftwareBlocked
        } else {
            Self::Allowed
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyStatus {
    pub microphone_muted: Option<bool>,
    pub microphone_kill: KillState,
    pub camera_present: bool,
    pub camera_kill: KillState,
    pub wifi_kill: KillState,
    pub bluetooth_kill: KillState,
    pub mobile_kill: KillState,
    pub active_media: Vec<ActiveMediaUse>,
}

impl Default for PrivacyStatus {
    fn default() -> Self {
        Self {
            microphone_muted: None,
            microphone_kill: KillState::Unavailable,
            camera_present: false,
            camera_kill: KillState::Unavailable,
            wifi_kill: KillState::Unavailable,
            bluetooth_kill: KillState::Unavailable,
            mobile_kill: KillState::Unavailable,
            active_media: Vec::new(),
        }
    }
}

pub fn get_privacy_status() -> PrivacyStatus {
    let mut status = get_privacy_status_from(Path::new("/sys/class/rfkill"), Path::new("/dev"));
    status.active_media = active_media_uses().unwrap_or_default();
    status
}

fn get_privacy_status_from(class: &Path, dev: &Path) -> PrivacyStatus {
    let mut status = PrivacyStatus {
        microphone_muted: media::audio::default_node::volume_state(
            media::audio::default_node::DefaultNode::Source,
        )
        .ok()
        .map(|s| s.muted),
        camera_present: std::fs::read_dir(dev).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("video"))
        }),
        ..PrivacyStatus::default()
    };
    // Every switch of a kind is folded in, so a machine with more than one
    // (the card's own and the platform's) reports blocked when any of them is.
    let mut radios: HashMap<String, RfkillState> = HashMap::new();
    for (kind, switch) in rfkill::switches(class) {
        let folded = radios.entry(kind).or_default();
        *folded = folded.fold(switch);
    }
    let kill = |kind: &str| KillState::from(radios.get(kind).copied().unwrap_or_default());
    status.wifi_kill = kill("wlan");
    status.bluetooth_kill = kill("bluetooth");
    status.mobile_kill = kill("wwan");
    // Some platforms expose physical camera/mic shutters through rfkill even
    // though these names are not mandated by Linux.
    status.camera_kill = kill("camera");
    status.microphone_kill = kill("microphone");
    status
}

/// Snapshot active capture streams through `PipeWire`'s own graph, each
/// attributed from `/proc`: see [`media::audio::capture`].
pub fn active_media_uses() -> Result<Vec<ActiveMediaUse>, String> {
    Ok(media::audio::capture::media_uses()?
        .into_iter()
        .map(ActiveMediaUse::from)
        .collect())
}

/// Immediately disconnect an offending `PipeWire` client. This is
/// deliberately an ephemeral stop, not a pretend persistent permission:
/// portal-managed grants remain owned by the portal permission store.
pub fn revoke_media_client(client_id: u32) -> Result<(), String> {
    crate::process::run("pw-cli", &["destroy", &client_id.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_blocked_switch_of_a_kind_blocks_it_whichever_order_it_reads_in() {
        for blocked_first in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let dev = tempfile::tempdir().unwrap();
            for (name, blocked) in [("rfkill0", blocked_first), ("rfkill1", !blocked_first)] {
                let switch = root.path().join(name);
                std::fs::create_dir(&switch).unwrap();
                std::fs::write(switch.join("type"), "wlan\n").unwrap();
                std::fs::write(switch.join("soft"), u8::from(blocked).to_string()).unwrap();
                std::fs::write(switch.join("hard"), "0\n").unwrap();
            }
            let status = get_privacy_status_from(root.path(), dev.path());
            assert_eq!(
                status.wifi_kill,
                KillState::SoftwareBlocked,
                "one blocked switch keeps the radio off (blocked first: {blocked_first})"
            );
        }
    }
}
