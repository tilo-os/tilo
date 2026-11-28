//! The kernel's privacy switches: the radios, and the camera and microphone shutters.

use std::collections::HashMap;
use std::path::Path;

use crate::rfkill::{self, RfkillState};

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
    pub microphone_kill: KillState,
    pub camera_present: bool,
    pub camera_kill: KillState,
    pub wifi_kill: KillState,
    pub bluetooth_kill: KillState,
    pub mobile_kill: KillState,
}

impl Default for PrivacyStatus {
    fn default() -> Self {
        Self {
            microphone_kill: KillState::Unavailable,
            camera_present: false,
            camera_kill: KillState::Unavailable,
            wifi_kill: KillState::Unavailable,
            bluetooth_kill: KillState::Unavailable,
            mobile_kill: KillState::Unavailable,
        }
    }
}

pub fn get_privacy_status() -> PrivacyStatus {
    get_privacy_status_from(Path::new("/sys/class/rfkill"), Path::new("/dev"))
}

fn get_privacy_status_from(class: &Path, dev: &Path) -> PrivacyStatus {
    let mut status = PrivacyStatus {
        camera_present: std::fs::read_dir(dev).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("video"))
        }),
        ..PrivacyStatus::default()
    };
    // Every switch of a kind is folded in, so a machine with more than one (the card's own and the
    // platform's) reports blocked when any of them is.
    let mut radios: HashMap<String, RfkillState> = HashMap::new();
    for (kind, switch) in rfkill::switches(class) {
        let folded = radios.entry(kind).or_default();
        *folded = folded.fold(switch);
    }
    let kill = |kind: &str| KillState::from(radios.get(kind).copied().unwrap_or_default());
    status.wifi_kill = kill("wlan");
    status.bluetooth_kill = kill("bluetooth");
    status.mobile_kill = kill("wwan");
    // Some platforms expose physical camera and microphone shutters through rfkill, though these names
    // are not mandated by Linux.
    status.camera_kill = kill("camera");
    status.microphone_kill = kill("microphone");
    status
}

/// Disconnects an offending client at once. Deliberately an ephemeral stop, not a pretend persistent
/// permission: portal-managed grants stay owned by the portal permission store.
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
