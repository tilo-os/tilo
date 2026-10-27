//! The connections and devices a review capture starts from: fixed sample
//! ids, addresses and kinds, so a screenshot of Remote shows the same list
//! every time.

use std::collections::HashMap;

use iroh_base::SecretKey;
use uuid::Uuid;

use ui::tilo_resource::Atom;

use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind, Presence};

/// The time, in milliseconds since the Unix epoch, the sample roster's
/// "last used" reads against.
const SAMPLE_NOW_MS: i64 = 1_790_000_000_000;

/// What a screenshot shows that the document does not hold: the time "last
/// used" reads against, which machines answered, and the Tilo devices
/// nearby.
pub(crate) struct Sample {
    pub(crate) now_ms: i64,
    pub(crate) presence: HashMap<ConnectionId, Presence>,
    pub(super) devices: Vec<TiloDevice>,
}

use super::TiloDevice;

impl Sample {
    pub(crate) fn new() -> Self {
        let presence = with_sample_seeds(|seeds| {
            seeds
                .into_iter()
                .chain([legacy_rdp_seed()])
                .map(|(index, seed)| (sample_id(index), seed.presence))
                .collect()
        });
        Self {
            now_ms: SAMPLE_NOW_MS,
            presence,
            devices: sample_tilo_devices(),
        }
    }
}

/// Readable fixture input for a sample connection. Named fields keep adjacent
/// strings, enums, and booleans from being silently transposed.
struct ConnectionSeed<'a> {
    name: &'a str,
    host: &'a str,
    port: u16,
    user: &'a str,
    kind: ConnectionKind,
    auth: AuthMethod,
    /// How long ago the last session was, in minutes; `None` for never.
    used_minutes_ago: Option<i64>,
    /// Whether the machine answered when this device last tried.
    presence: Presence,
    group: &'a str,
    favorite: bool,
}

/// The stable id of the `index`th sample machine, so a scenario's frame is
/// identical on every run.
pub(super) fn sample_id(index: u128) -> ConnectionId {
    Uuid::from_u128(0x7e40_7e00_0000_0000_0000_0000_0000_0000 | index)
}

impl ConnectionSeed<'_> {
    fn build(self, index: u128) -> Connection {
        Connection {
            id: sample_id(index),
            name: self.name.into(),
            host: Atom(self.host.into()),
            port: self.port,
            user: Atom(self.user.into()),
            kind: self.kind,
            auth: self.auth,
            last_used_ms: self
                .used_minutes_ago
                .map(|minutes| SAMPLE_NOW_MS - minutes * 60_000),
            group: Atom(self.group.into()),
            favorite: self.favorite,
        }
    }
}

/// Hand `read` the lived-in list of saved machines, each with its index.
#[allow(
    clippy::too_many_lines,
    reason = "fixture data: one named record per sample machine"
)]
fn with_sample_seeds<T>(read: impl FnOnce(Vec<(u128, ConnectionSeed<'_>)>) -> T) -> T {
    let studio = SecretKey::from_bytes(&[17; 32]).public().to_string();
    let laptop = SecretKey::from_bytes(&[29; 32]).public().to_string();
    let seeds = [
        ConnectionSeed {
            name: "Build server",
            host: "build.lab.internal",
            port: 22,
            user: "admin",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            used_minutes_ago: Some(12),
            presence: Presence::Online,
            group: "Lab",
            favorite: true,
        },
        ConnectionSeed {
            name: "Studio desktop",
            host: &studio,
            port: 0,
            user: "",
            kind: ConnectionKind::Tilo,
            auth: AuthMethod::DeviceIdentity,
            used_minutes_ago: Some(1),
            presence: Presence::Online,
            group: "Tilo",
            favorite: true,
        },
        ConnectionSeed {
            name: "Design iMac",
            host: "10.0.0.24",
            port: 5900,
            user: "",
            kind: ConnectionKind::Vnc,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(130),
            presence: Presence::Online,
            group: "Studio",
            favorite: false,
        },
        ConnectionSeed {
            name: "Media PC",
            host: "192.168.1.40",
            port: 5900,
            user: "",
            kind: ConnectionKind::Vnc,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(26 * 60),
            presence: Presence::Online,
            group: "Home",
            favorite: false,
        },
        ConnectionSeed {
            name: "Edge gateway",
            host: "edge.example.com",
            port: 2222,
            user: "root",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            used_minutes_ago: Some(3 * 24 * 60),
            presence: Presence::Online,
            group: "Production",
            favorite: false,
        },
        ConnectionSeed {
            name: "Raspberry Pi cluster",
            host: "192.168.4.2",
            port: 22,
            user: "pi",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(8 * 24 * 60),
            presence: Presence::Offline,
            group: "Lab",
            favorite: false,
        },
        ConnectionSeed {
            name: "Workshop laptop",
            host: &laptop,
            port: 0,
            user: "",
            kind: ConnectionKind::Tilo,
            auth: AuthMethod::DeviceIdentity,
            used_minutes_ago: Some(5 * 60),
            presence: Presence::Online,
            group: "Tilo",
            favorite: false,
        },
        ConnectionSeed {
            name: "Backup vault",
            host: "vault.example.com",
            port: 22,
            user: "backup",
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            used_minutes_ago: Some(12 * 24 * 60),
            presence: Presence::Online,
            group: "Production",
            favorite: false,
        },
    ];
    read((1_u128..).zip(seeds).collect())
}

/// A lived-in list of saved machines, with fixed ids so every screenshot is
/// the same document.
pub(crate) fn sample_connections() -> Vec<Connection> {
    with_sample_seeds(|seeds| {
        seeds
            .into_iter()
            .map(|(index, seed)| seed.build(index))
            .collect()
    })
}

/// A remote desktop profile saved before Remote dropped RDP, with its index.
fn legacy_rdp_seed() -> (u128, ConnectionSeed<'static>) {
    (
        100,
        ConnectionSeed {
            name: "Windows workstation",
            host: "10.0.0.31",
            port: 3389,
            user: "quinten",
            kind: ConnectionKind::Rdp,
            auth: AuthMethod::Password,
            used_minutes_ago: Some(40 * 24 * 60),
            presence: Presence::Offline,
            group: "Work",
            favorite: false,
        },
    )
}

/// A remote desktop profile saved before Remote dropped RDP: kept so its
/// owner can switch it to a protocol Remote opens.
pub(crate) fn sample_legacy_rdp() -> Connection {
    let (index, seed) = legacy_rdp_seed();
    seed.build(index)
}

fn sample_tilo_devices() -> Vec<TiloDevice> {
    let device = |name: &str, seed: u8, online: bool| TiloDevice {
        name: name.into(),
        node: SecretKey::from_bytes(&[seed; 32]).public().to_string(),
        online,
    };
    vec![
        device("Studio desktop", 17, true),
        device("Workshop laptop", 29, true),
        device("Living room TV", 41, true),
        device("Garage Pi", 53, false),
    ]
}
