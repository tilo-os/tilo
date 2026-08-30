//! Firmware inventory and upgrades through fwupd.
//!
//! `fwupdmgr` is only a client for the system fwupd daemon.  The daemon
//! validates signed LVFS metadata and firmware and polkit owns authorization;
//! this module never downloads an arbitrary URL or elevates a shell command.

use crate::process::{run, run_capture};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareUpdate {
    pub device: String,
    pub current_version: Option<String>,
    pub new_version: Option<String>,
    pub needs_reboot: bool,
}

/// Query updates already offered by fwupd's configured remotes.
pub fn get_firmware_updates() -> Result<Vec<FirmwareUpdate>, String> {
    let json = run_capture("fwupdmgr", &["get-updates", "--json"])?;
    parse_updates(&json)
}

/// Refresh signed metadata from configured fwupd remotes.
pub fn refresh_firmware_metadata() -> Result<(), String> {
    run("fwupdmgr", &["refresh"])
}

/// Install all firmware updates approved by fwupd.  fwupd/polkit performs
/// authorization; `-y` only answers the client's non-security confirmation.
pub fn install_firmware_updates() -> Result<(), String> {
    run("fwupdmgr", &["update", "-y"])
}

fn parse_updates(json: &str) -> Result<Vec<FirmwareUpdate>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid fwupd json: {e}"))?;
    let mut updates = Vec::new();
    collect_devices(&root, &mut updates);
    updates.sort_by(|a, b| a.device.cmp(&b.device));
    updates.dedup();
    Ok(updates)
}

fn collect_devices(value: &serde_json::Value, out: &mut Vec<FirmwareUpdate>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_devices(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            let releases = map
                .get("Releases")
                .or_else(|| map.get("releases"))
                .and_then(serde_json::Value::as_array);
            if let Some(releases) = releases
                && let Some(release) = releases.first().and_then(serde_json::Value::as_object)
            {
                let string = |key: &str| {
                    map.get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                };
                let new_version = release
                    .get("Version")
                    .or_else(|| release.get("version"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let flags = release
                    .get("Flags")
                    .or_else(|| release.get("flags"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                out.push(FirmwareUpdate {
                    device: string("Name")
                        .or_else(|| string("name"))
                        .unwrap_or_else(|| "Firmware device".into()),
                    current_version: string("Version").or_else(|| string("version")),
                    new_version,
                    // FWUPD_RELEASE_FLAG_NEEDS_REBOOT is bit 2.
                    needs_reboot: flags & (1 << 2) != 0,
                });
            }
            for child in map.values() {
                collect_devices(child, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fwupd_devices_and_releases() {
        let updates = parse_updates(
            r#"{"Devices":[{"Name":"System Firmware","Version":"1.0","Releases":[{"Version":"1.2","Flags":4}]}]}"#,
        )
        .unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].device, "System Firmware");
        assert_eq!(updates[0].new_version.as_deref(), Some("1.2"));
        assert!(updates[0].needs_reboot);
    }
}
