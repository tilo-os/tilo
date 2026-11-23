//! The `disk-unlock` resource: what [`tilo_system_resources::disk_unlock`] says it is, published
//! with the actions of [`DiskUnlockAction`].
//!
//! What reads and changes the enrolment is `tilo-auth` behind polkit
//! (os/etc/polkit-1/rules.d/49-tilo-disk-unlock.rules): reading it is granted without a prompt to
//! an administrator at this machine's seat, and changing it asks for authentication. This module
//! translates between its answer and the shared resource, and refuses a volume or network the
//! helper would be handed back before any command runs.

use std::time::Duration;

use bounded_command::Output;
use serde::Deserialize;
use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::offer;
use tilo_system_resources::disk_unlock::{
    DISK_UNLOCK, DiskUnlock, DiskUnlockAction, Sealing, UnlockVolume,
};

use crate::i18n::name;

/// The helper the enrolment is read and changed through, as root.
const HELPER: &str = "/usr/libexec/tilo/tilo-auth";
const PKEXEC: &str = "pkexec";
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const CHANGE_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_LIMIT: usize = 256 * 1024;
const MAX_VOLUMES: usize = 64;
/// The most Wi-Fi networks one volume may name.
pub const MAX_NETWORKS: usize = 16;

/// The `disk-unlock` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.disk-unlock", label = "Disk unlock")]
pub struct DiskUnlockReading;

/// The `disk-unlock` resource reading `disk_unlock`.
#[must_use]
pub fn resource(_disk: &DiskUnlock) -> ResourceSpec {
    offer::<DiskUnlockAction>(tilo_resource::typed_resource::<DiskUnlockReading>(
        DISK_UNLOCK,
        name("so.tilo.system.disk-unlock", "Disk unlock"),
    ))
}

/// Read the enrolled volumes and the Wi-Fi connections saved here.
#[must_use]
pub fn scan() -> DiskUnlock {
    let volumes = read_volumes();
    let saved_networks = saved_wifi();
    let read_error = volumes.as_ref().err().cloned();
    DiskUnlock {
        volumes: volumes.unwrap_or_default(),
        saved_networks: saved_networks.unwrap_or_default(),
        scanned: true,
        read_error,
        ..DiskUnlock::default()
    }
}

/// Choose the whole new list of Wi-Fi connections `volume` may use at boot.
///
/// # Errors
/// The volume is not well formed, a network is not, or the helper refused or failed.
pub fn set_networks(volume: &str, networks: &[String]) -> Result<(), String> {
    if !valid_volume(volume) {
        return Err(format!("{volume} is not a volume's UUID"));
    }
    if networks.len() > MAX_NETWORKS {
        return Err(format!("a volume may use at most {MAX_NETWORKS} networks"));
    }
    if let Some(bad) = networks.iter().find(|name| !valid_network(name)) {
        return Err(format!("`{bad}` is not a Wi-Fi connection's name"));
    }
    let device = format!("/dev/disk/by-uuid/{volume}");
    let mut args: Vec<&str> = vec![
        HELPER,
        "sibling",
        "enrol",
        device.as_str(),
        "--replace-networks",
    ];
    for network in networks {
        args.extend(["--network", network.as_str()]);
    }
    let output = bounded_command::run(PKEXEC, &args, None, OUTPUT_LIMIT, CHANGE_TIMEOUT)
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(helper_failure(&output))
    }
}

fn read_volumes() -> Result<Vec<UnlockVolume>, String> {
    let output = bounded_command::run(
        PKEXEC,
        &[HELPER, "sibling", "volumes"],
        None,
        OUTPUT_LIMIT,
        READ_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(helper_failure(&output));
    }
    parse_volumes(&String::from_utf8_lossy(&output.stdout))
}

fn saved_wifi() -> Result<Vec<String>, String> {
    Ok(os_actions::network::list_saved_network_profiles()?
        .into_iter()
        .filter(|profile| profile.kind == "802-11-wireless")
        .map(|profile| profile.name)
        .collect())
}

/// The helper's last word, or that polkit did not let it run at all.
fn helper_failure(output: &Output) -> String {
    if let Some(126 | 127) = output.status.code() {
        return "Not authorised to change this computer's unlocking".to_owned();
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let said = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .trim_start_matches("Error: ")
        .to_owned();
    if said.is_empty() {
        format!("the unlock helper ended with {}", output.status)
    } else {
        said
    }
}

/// A LUKS UUID: 8-4-4-4-12 hex digits.
#[must_use]
pub fn valid_volume(volume: &str) -> bool {
    volume.len() == 36
        && volume.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// A Wi-Fi connection's name the helper is safe to be handed back.
#[must_use]
pub fn valid_network(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && !name.starts_with('.')
}

#[derive(Deserialize)]
struct WireVolume {
    volume: String,
    device: String,
    keyslot: u32,
    sealing: Sealing,
    #[serde(default)]
    networks: Vec<String>,
    #[serde(default)]
    siblings: Vec<String>,
}

/// The helper's answer as enrolled volumes, refusing anything not well formed: a volume handed
/// back to `pkexec` must be one.
///
/// # Errors
/// The answer is not enrolments, too many, or one of them is malformed.
pub fn parse_volumes(json: &str) -> Result<Vec<UnlockVolume>, String> {
    let volumes: Vec<WireVolume> = serde_json::from_str(json)
        .map_err(|error| format!("the unlock helper's answer does not read: {error}"))?;
    if volumes.len() > MAX_VOLUMES {
        return Err("the unlock helper listed too many volumes".to_owned());
    }
    let mut out = Vec::with_capacity(volumes.len());
    for volume in volumes {
        let device_ok = volume
            .device
            .strip_prefix("/dev/disk/by-uuid/")
            .is_some_and(|rest| rest == volume.volume);
        if !valid_volume(&volume.volume)
            || !device_ok
            || volume.networks.len() > MAX_NETWORKS
            || !volume.networks.iter().all(|name| valid_network(name))
        {
            return Err(format!(
                "the unlock helper answered a malformed volume: {}",
                volume.volume
            ));
        }
        out.push(UnlockVolume {
            volume: volume.volume,
            device: volume.device,
            keyslot: volume.keyslot,
            sealing: volume.sealing,
            networks: volume.networks,
            siblings: volume.siblings,
        });
    }
    Ok(out)
}
