//! Kernel and PipeWire privacy state for shell indicators.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillState {
    Unavailable,
    Allowed,
    SoftwareBlocked,
    HardwareBlocked,
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
        }
    }
}

pub fn get_privacy_status() -> PrivacyStatus {
    get_privacy_status_from(Path::new("/sys/class/rfkill"), Path::new("/dev"))
}

fn get_privacy_status_from(rfkill: &Path, dev: &Path) -> PrivacyStatus {
    let mut status = PrivacyStatus {
        microphone_muted: crate::audio::get_source_state().ok().map(|s| s.muted),
        camera_present: std::fs::read_dir(dev).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("video"))
        }),
        ..PrivacyStatus::default()
    };
    if let Ok(entries) = std::fs::read_dir(rfkill) {
        for entry in entries.flatten() {
            let root = entry.path();
            let ty = read(&root.join("type")).unwrap_or_default();
            let state = kill_state(&root);
            match ty.as_str() {
                "wlan" => status.wifi_kill = state,
                "bluetooth" => status.bluetooth_kill = state,
                "wwan" => status.mobile_kill = state,
                // Some platforms expose physical camera/mic shutters through
                // rfkill even though these names are not mandated by Linux.
                "camera" => status.camera_kill = state,
                "microphone" => status.microphone_kill = state,
                _ => {}
            }
        }
    }
    status
}

fn kill_state(root: &Path) -> KillState {
    if read(root.join("hard").as_path()).as_deref() == Some("1") {
        KillState::HardwareBlocked
    } else if read(root.join("soft").as_path()).as_deref() == Some("1")
        || read(root.join("state").as_path()).as_deref() == Some("0")
    {
        KillState::SoftwareBlocked
    } else {
        KillState::Allowed
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_camera_shutter_wins_over_software_state() {
        let root = tempfile::tempdir().unwrap();
        let dev = tempfile::tempdir().unwrap();
        let camera = root.path().join("rfkill0");
        std::fs::create_dir(&camera).unwrap();
        std::fs::write(camera.join("type"), "camera\n").unwrap();
        std::fs::write(camera.join("soft"), "0\n").unwrap();
        std::fs::write(camera.join("hard"), "1\n").unwrap();

        let status = get_privacy_status_from(root.path(), dev.path());
        assert_eq!(status.camera_kill, KillState::HardwareBlocked);
    }
}
