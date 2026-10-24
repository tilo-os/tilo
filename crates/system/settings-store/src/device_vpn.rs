//! Your devices as a VPN: whether this device lets the person's other
//! devices route their traffic through it, and which of their devices this
//! one routes through (docs/device-vpn.md).
//!
//! Settings and quick settings write these keys; the session's controller
//! follows them and publishes what it is doing as a [`DeviceVpnStatus`] in
//! the private runtime directory, which Settings reads back. Every key is
//! this device's own: another device is elsewhere, on another network.

use serde::{Deserialize, Serialize};

use crate::Key;
use crate::hardware::publisher_live_in;

/// The section holding this device's VPN choices.
pub const SECTION: &str = "device_vpn";

/// Whether the person's other devices may route their traffic through this
/// one. Off until the person turns it on.
pub const EXIT: Key<bool> = Key::new(SECTION, "exit", || false);

/// Which of the person's devices this one routes through, and how; `None`
/// routes nothing.
pub const THROUGH: Key<Option<Through>> = Key::new(SECTION, "through", || None);

/// The last request to stop one of the person's devices routing through
/// this one. A new request has a new id; the session acts on each id once.
pub const STOP: Key<Option<StopRequest>> = Key::new(SECTION, "stop", || None);

/// The status file holds a handful of devices; anything larger is not one the
/// session wrote.
const MAX_STATUS_BYTES: u64 = 262_144;

/// What this device routes through another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Routes {
    /// Everything this device sends.
    #[default]
    All,
    /// Only what is for the networks the other device is attached to: the
    /// files and printers at home, the rest goes out as usual.
    HomeNetwork,
}

/// Routing through one of the person's devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Through {
    /// The other device's endpoint id, hex.
    pub device: String,
    pub routes: Routes,
    /// Whether names are looked up through the other device too, so names
    /// on its network resolve.
    pub dns: bool,
    /// Whether this device's traffic is blocked while the tunnel is down,
    /// instead of going out directly.
    pub kill_switch: bool,
}

impl Default for Through {
    fn default() -> Self {
        Self {
            device: String::new(),
            routes: Routes::All,
            dns: true,
            kill_switch: false,
        }
    }
}

impl Through {
    /// Routing everything through `device`, names included, the way most
    /// people mean it.
    #[must_use]
    pub fn device(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            ..Self::default()
        }
    }
}

/// A request to stop `device` routing through this one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopRequest {
    pub id: u64,
    /// The device's endpoint id, hex.
    pub device: String,
}

impl StopRequest {
    /// A request to stop `device`, after `previous`.
    #[must_use]
    pub fn after(previous: Option<&Self>, device: impl Into<String>) -> Self {
        Self {
            id: previous.map_or(1, |previous| previous.id.wrapping_add(1)),
            device: device.into(),
        }
    }
}

/// Where this device's route through another stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ClientState {
    #[default]
    Connecting,
    Connected {
        /// When the tunnel came up, Unix milliseconds.
        since_unix_ms: u64,
    },
    /// The tunnel is down; with the kill switch on, nothing goes out until
    /// it is back.
    Reconnecting { blocking: bool },
    /// The other device turned this one away.
    Refused { reason: Refusal },
    /// This device could not set the route up.
    Failed { problem: Problem },
}

/// Why this device could not set its route up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// The system part that routes traffic is not installed here.
    NotInstalled,
    /// The system refused the route.
    Setup,
    /// Only the other device's home network was asked for, and it is on
    /// none this device can reach through it.
    NoHomeNetwork,
}

/// Why another device turned this one away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// It does not count this device as the person's.
    NotYours,
    /// Its switch to share its connection is off.
    ExitOff,
    /// It has no room for another device right now.
    Full,
}

/// This device routing through another, as Settings shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientStatus {
    /// The other device's endpoint id, hex.
    pub device: String,
    /// The other device's name, as the person named it.
    pub label: String,
    pub state: ClientState,
    pub routes: Routes,
    pub kill_switch: bool,
}

/// One of the person's devices routing through this one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExitUser {
    pub device: String,
    pub label: String,
    pub since_unix_ms: u64,
}

/// One of the person's devices this one could route through: it shares its
/// connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OfferedExit {
    pub device: String,
    pub label: String,
}

/// What the session's controller is doing, published for Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceVpnStatus {
    pub publisher_pid: u32,
    /// With the PID it names the publisher's lease, so a file a crashed
    /// session left is never shown as current.
    pub publisher_start_ticks: u64,
    /// Whether this device can be an exit at all: the helper is installed.
    pub exit_available: bool,
    /// The person's switch, as the session follows it.
    pub exit_on: bool,
    /// The person's devices routing through this one now.
    pub exit_users: Vec<ExitUser>,
    /// The person's devices this one could route through.
    pub offered: Vec<OfferedExit>,
    /// This device's route through another, while it has one.
    pub client: Option<ClientStatus>,
}

impl DeviceVpnStatus {
    /// The live session's status, or `None` when no running session
    /// published one.
    #[must_use]
    pub fn load() -> Option<Self> {
        Self::load_at(&status_path()?)
    }

    #[must_use]
    pub fn load_at(path: &std::path::Path) -> Option<Self> {
        crate::published::note_file_read();
        let bytes = tilo_files::native::bounded::read(path, MAX_STATUS_BYTES).ok()??;
        let status: Self = serde_json::from_slice(&bytes).ok()?;
        publisher_live_in(
            path.parent()?,
            status.publisher_pid,
            status.publisher_start_ticks,
        )
        .then_some(status)
    }

    /// # Errors
    ///
    /// `XDG_RUNTIME_DIR` is unset or relative, or the file can't be written.
    pub fn save(&self) -> std::io::Result<()> {
        let path =
            status_path().ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
        self.save_at(&path)
    }

    /// # Errors
    ///
    /// `self` can't be serialised or written to `path`.
    pub fn save_at(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        body.push(b'\n');
        tilo_files::native::atomic::write_creating_parent(path, &body)
    }

    /// The rows Settings → Network → VPN lists for this feature, after the
    /// system's own VPN connections: one per device this one routes or could
    /// route through, the current one first.
    #[must_use]
    pub fn rows(&self) -> Vec<DeviceVpnRow> {
        let mut rows = Vec::new();
        if let Some(client) = &self.client {
            rows.push(DeviceVpnRow {
                device: client.device.clone(),
                label: client.label.clone(),
                state: Some(client.state.clone()),
            });
        }
        rows.extend(
            self.offered
                .iter()
                .filter(|offered| {
                    self.client
                        .as_ref()
                        .is_none_or(|client| client.device != offered.device)
                })
                .map(|offered| DeviceVpnRow {
                    device: offered.device.clone(),
                    label: offered.label.clone(),
                    state: None,
                }),
        );
        rows
    }
}

/// One entry of the VPN list: routing through one of the person's devices.
/// Its title is "Through" and the device's name; turning it on writes
/// [`THROUGH`], off clears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceVpnRow {
    pub device: String,
    pub label: String,
    /// `None` while this device does not route through it.
    pub state: Option<ClientState>,
}

impl DeviceVpnRow {
    /// Whether this device routes, or is trying to route, through it.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.state.is_some()
    }
}

/// Where the session publishes its status: the private runtime directory,
/// never a shared fallback another user could write.
#[must_use]
pub fn status_path() -> Option<std::path::PathBuf> {
    tilo_paths::xdg::runtime_dir().map(|path| path.join("tilo/device-vpn.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_is_off_and_nothing_is_routed_until_the_person_chooses() {
        let document = serde_json::Map::new();
        assert!(!EXIT.from_document(&document));
        assert_eq!(THROUGH.from_document(&document), None);
        assert_eq!(STOP.from_document(&document), None);
    }

    #[test]
    fn routing_through_a_device_reads_back_with_its_choices() {
        let through = Through {
            routes: Routes::HomeNetwork,
            kill_switch: true,
            ..Through::device("ab12")
        };
        let mut section = serde_json::Map::new();
        section.insert("through".into(), serde_json::to_value(&through).unwrap());
        assert_eq!(THROUGH.from_section(&section), Some(through));
        // A choice a later version added does not unset the rest.
        let partial = serde_json::json!({ "device": "cd34" });
        section.insert("through".into(), partial);
        assert_eq!(
            THROUGH.from_section(&section),
            Some(Through::device("cd34"))
        );
    }

    #[test]
    fn each_stop_request_has_a_new_id() {
        let first = StopRequest::after(None, "ab");
        let second = StopRequest::after(Some(&first), "ab");
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn rows_list_the_current_route_first_and_each_device_once() {
        let status = DeviceVpnStatus {
            offered: vec![
                OfferedExit {
                    device: "a".into(),
                    label: "Desk".into(),
                },
                OfferedExit {
                    device: "b".into(),
                    label: "Home server".into(),
                },
            ],
            client: Some(ClientStatus {
                device: "b".into(),
                label: "Home server".into(),
                state: ClientState::Connected { since_unix_ms: 5 },
                ..ClientStatus::default()
            }),
            ..DeviceVpnStatus::default()
        };
        let rows = status.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].device, "b");
        assert!(rows[0].is_on());
        assert_eq!(rows[1].device, "a");
        assert!(!rows[1].is_on());
    }

    #[test]
    fn a_status_without_a_live_publisher_is_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-vpn.json");
        let status = DeviceVpnStatus {
            publisher_pid: std::process::id(),
            publisher_start_ticks: 1,
            exit_on: true,
            ..DeviceVpnStatus::default()
        };
        status.save_at(&path).unwrap();
        assert_eq!(DeviceVpnStatus::load_at(&path), None);
        let (pid, ticks) = crate::hardware::publisher_identity_in(dir.path()).unwrap();
        let live = DeviceVpnStatus {
            publisher_pid: pid,
            publisher_start_ticks: ticks,
            ..status
        };
        live.save_at(&path).unwrap();
        assert_eq!(DeviceVpnStatus::load_at(&path), Some(live));
    }
}
