//! Administering the person's devices, whichever action API asks: the `identity.*` actions.
//! Each action API decides who may take a step; what the step writes, and with which arguments,
//! is decided here once.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilo_actions::remote::identity_admin::FleetDevice;
use tilo_device_identity::IdentityPaths;
use tilo_user_identity::{RevocationEntry, RevocationReason};

use crate::device_fleet::DeviceFleetStore;

/// Where the identity an approved device joins lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonPaths {
    /// The device root (`/var/lib/tilo` on an installed system).
    pub root: PathBuf,
    /// The person's identity directory.
    pub dir: PathBuf,
}

impl PersonPaths {
    /// The identity this account acts as: this device's identity root
    /// ([`crate::provisioning::collaboration_identity_root`]) and the
    /// person's identity directory.
    pub fn from_env() -> Result<Self, String> {
        let root = crate::provisioning::collaboration_identity_root();
        let dir = crate::store::identity_dir_from_env()
            .map_err(|error| format!("could not find your identity: {error}"))?;
        Ok(Self { root, dir })
    }
}

/// A device key or relay service node id: 32 bytes as hex, lowercased.
///
/// # Errors
///
/// When `text` is not 64 hexadecimal characters.
pub fn node_id(text: &str, what: &str) -> Result<String, String> {
    let text = text.trim();
    if text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(format!("{what} must be 64 hexadecimal characters"))
    }
}

/// The devices trusted under the identity `identity_id` (whose root key is
/// `user_public_key`), the one `device` holds a certificate for marked
/// current.
pub fn trusted_devices(
    fleets: &DeviceFleetStore,
    device: &IdentityPaths,
    identity_id: &str,
    user_public_key: [u8; 32],
) -> Result<Vec<FleetDevice>, String> {
    let fleet = fleets
        .load(identity_id)
        .map_err(|error| error.to_string())?;
    if fleet
        .certificates
        .iter()
        .any(|cert| cert.user_public_key != user_public_key)
        || fleet
            .revocations
            .iter()
            .any(|entry| entry.user_public_key != user_public_key)
    {
        return Err("the device fleet holds another identity's records".into());
    }
    let current = tilo_device_identity::persist::read_device_cert(
        &device.device_cert_for_user(&user_public_key),
    )
    .ok()
    .map(|cert| cert.device_public_key);
    Ok(fleet
        .certificates
        .iter()
        .map(|cert| {
            listed_device(
                cert,
                current == Some(cert.device_public_key),
                fleet.refuses(cert),
            )
        })
        .collect())
}

/// `cert` as the person's device list shows it: its label, key, and when it
/// was issued and expires, without what a host's record adds.
fn listed_device(
    cert: &tilo_user_identity::DeviceCert,
    current: bool,
    revoked: bool,
) -> FleetDevice {
    FleetDevice {
        label: cert.device_label.clone(),
        key_hex: hex::encode(cert.device_public_key),
        fingerprint: fingerprint(&cert.device_public_key),
        current,
        revoked,
        kind: tilo_actions::remote::identity_admin::KeyKind::of(
            cert.capabilities.agent.is_some(),
            cert.capabilities.server_key,
        ),
        browser: None,
        agent: None,
        issued_ms: Some(ms_since_epoch(cert.issued_at_ns)),
        expires_ms: cert.expires_at_ns.map(ms_since_epoch),
        last_active_ms: None,
        online: false,
        workspace: scope_id(cert.capabilities.agent),
    }
}

/// One key the person's record on a host lists, as that host answered it.
#[derive(Clone, Debug)]
pub struct RecordDevice {
    /// Its certificate, signed by the person's root.
    pub cert: tilo_user_identity::DeviceCert,
    /// The host that keeps the record refuses the key.
    pub refused: bool,
    /// The browser profile it is a tab of, by the profile's id.
    pub browser: Option<String>,
    /// The agent its sessions there named.
    pub agent: Option<String>,
    /// When a session of it there last opened or ended, ms since the epoch.
    pub last_active_ms: Option<i64>,
    /// A session of it there is open now.
    pub online: bool,
}

/// What the person's record on their hosts holds, as those hosts answered
/// it: their keys, and their workspaces' names by id.
#[derive(Clone, Debug, Default)]
pub struct Record {
    pub devices: Vec<RecordDevice>,
    pub workspaces: BTreeMap<String, String>,
}

impl Record {
    /// Add `device`, one key a host's record lists, merging what a host before
    /// said of the same key: the newest certificate, whether any host refuses
    /// it, the browser it is a tab of, the agent any of its sessions named,
    /// whether any host has it online now, and when it was last there.
    pub fn add(&mut self, device: RecordDevice) {
        match self
            .devices
            .iter_mut()
            .find(|known| known.cert.device_public_key == device.cert.device_public_key)
        {
            Some(known) => {
                known.refused |= device.refused;
                known.browser = known.browser.take().or(device.browser);
                known.agent = known.agent.take().or(device.agent);
                known.online |= device.online;
                known.last_active_ms = newest(known.last_active_ms, device.last_active_ms);
                if known.cert.issued_at_ns < device.cert.issued_at_ns {
                    known.cert = device.cert;
                }
            }
            None => self.devices.push(device),
        }
    }
}

/// The later of two times, when either is known.
fn newest(one: Option<i64>, other: Option<i64>) -> Option<i64> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.max(other)),
        (one, other) => one.or(other),
    }
}

/// Nanoseconds since the epoch as ms, for a time a list shows.
fn ms_since_epoch(ns: u64) -> i64 {
    i64::try_from(ns / 1_000_000).unwrap_or(i64::MAX)
}

/// The one workspace an agent's grant reaches, by its id; `None` for one that
/// reaches every workspace of the person's, or for a key that is no agent's
/// grant.
fn scope_id(scope: Option<tilo_user_identity::AgentScope>) -> Option<String> {
    scope?.workspace_name()
}

/// The devices `listed`, this device's fleet, and the keys `record` adds: one
/// list of the person's devices, each key once.
///
/// A person's devices are listed by every one of their devices from the same
/// record, the one their hosts keep beside their workspace list, because a
/// key their root certifies need not be certified here: a browser approves
/// one in its own tab, and another of their computers pairs one. `record` is
/// that record, each key with whether the host that keeps it refuses it, the
/// browser it is a tab of, the agent it runs and when it was last there. A key
/// this device's fleet knows is listed as the fleet says it — it is the one
/// that signed a revocation here — and the record can only add that the key is
/// refused, never that it is not.
#[must_use]
pub fn with_record(listed: Vec<FleetDevice>, record: Record) -> Vec<FleetDevice> {
    let named = |workspace: Option<String>| {
        workspace.map(|id| record.workspaces.get(&id).cloned().unwrap_or(id))
    };
    let mut devices = listed;
    // This device's fleet knows the one workspace a grant of its own reaches
    // by id: the record names it where it lists it.
    for device in &mut devices {
        device.workspace = named(device.workspace.take());
    }
    for one in record.devices {
        let workspace = named(scope_id(one.cert.capabilities.agent));
        let running = one.agent.is_some();
        let known = devices
            .iter_mut()
            .find(|known| known.key_hex == hex::encode(one.cert.device_public_key));
        if let Some(known) = known {
            known.revoked |= one.refused;
            known.browser = known.browser.take().or(one.browser);
            known.agent = known.agent.take().or(one.agent);
            known.online |= one.online;
            known.last_active_ms = newest(known.last_active_ms, one.last_active_ms);
            known.workspace = known.workspace.take().or(workspace);
            if running {
                known.kind = tilo_actions::remote::identity_admin::KeyKind::Agent;
            }
        } else {
            let mut device = listed_device(&one.cert, false, one.refused);
            device.agent = one.agent;
            device.browser = one.browser;
            device.last_active_ms = one.last_active_ms;
            device.online = one.online;
            device.workspace = workspace;
            if running {
                device.kind = tilo_actions::remote::identity_admin::KeyKind::Agent;
            }
            devices.push(device);
        }
    }
    devices
}

/// A key's short form people compare between screens: its first six bytes.
fn fingerprint(key: &[u8; 32]) -> String {
    key.iter()
        .take(6)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The revocation reason `text` names.
pub fn revocation_reason(text: &str) -> Result<RevocationReason, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "lost" => Ok(RevocationReason::Lost),
        "stolen" => Ok(RevocationReason::Stolen),
        "retired" => Ok(RevocationReason::Retired),
        "compromised" => Ok(RevocationReason::Compromised),
        _ => Err("reason must be lost, stolen, retired, or compromised".into()),
    }
}

/// Root-sign the permanent revocation of `device` (64 hex characters) under
/// the identity `identity_id`, for `reason`, and answer the entry the root
/// signed: what a device of the person's announces to their hosts, which
/// refuse the key from then on.
pub fn revoke(
    fleets: &DeviceFleetStore,
    identity_id: &str,
    device: &str,
    reason: &str,
) -> Result<RevocationEntry, String> {
    let device = node_id(device, "device")?;
    let mut key = [0; 32];
    hex::decode_to_slice(&device, &mut key).map_err(|error| error.to_string())?;
    let reason = revocation_reason(reason)?;
    fleets
        .revoke(identity_id, key, reason, tilo_clock::unix_now_ns())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of the person's devices, as the record on a host keeps it.
    fn certified(label: &str, key: u8) -> tilo_user_identity::DeviceCert {
        tilo_user_identity::DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [key; 32],
            device_label: label.into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn listed(label: &str, key: u8, current: bool, revoked: bool) -> FleetDevice {
        listed_device(&certified(label, key), current, revoked)
    }

    /// A key the person's record on a host lists, with nothing said of it but
    /// what is given.
    fn recorded(cert: tilo_user_identity::DeviceCert) -> RecordDevice {
        RecordDevice {
            cert,
            refused: false,
            browser: None,
            agent: None,
            last_active_ms: None,
            online: false,
        }
    }

    tilo_test_env::scenario! {
        fn the_device_list;

        phase the_devices_of_the_record_join_the_fleets_own;
        // A device another of the person's devices paired, or a browser
        // certified in its own tab, is in the record and not in this device's
        // fleet: the list Settings shows is both, each key once.
        {
            let devices = with_record(
                vec![listed("This desktop", 2, true, false)],
                Record {
                    devices: vec![
                        recorded(certified("This desktop", 2)),
                        RecordDevice {
                            browser: Some("0f".repeat(16)),
                            ..recorded(certified("Browser", 3))
                        },
                    ],
                    workspaces: BTreeMap::new(),
                },
            );
            assert_eq!(
                devices
                    .iter()
                    .map(|device| (device.label.as_str(), device.current, device.revoked))
                    .collect::<Vec<_>>(),
                [("This desktop", true, false), ("Browser", false, false)]
            );
            assert_eq!(devices[1].key_hex, hex::encode([3; 32]));
            assert_eq!(
                devices[1].browser.as_deref(),
                Some("0f".repeat(16).as_str()),
                "a browser's tab key says which browser it is of"
            );
        }

        phase a_revocation_marks_a_device_from_either_list;
        // A revocation reaches this device's list from either side: one its own
        // fleet signed, and one the host keeping the record keeps. The record
        // never says a key the fleet refused is fine again.
        {
            let devices = with_record(
                vec![
                    listed("Lost laptop", 4, false, true),
                    listed("Phone", 5, false, false),
                ],
                Record {
                    devices: vec![
                        recorded(certified("Lost laptop", 4)),
                        RecordDevice {
                            refused: true,
                            ..recorded(certified("Phone", 5))
                        },
                    ],
                    workspaces: BTreeMap::new(),
                },
            );
            assert_eq!(
                devices
                    .iter()
                    .map(|device| (device.label.as_str(), device.revoked))
                    .collect::<Vec<_>>(),
                [("Lost laptop", true), ("Phone", true)]
            );
        }

        phase what_each_host_says_of_a_key_is_one_line_in_the_list;
        // What the hosts of the person's workspaces said of one key is said once:
        // the newest time it was there, online when any host has it now, the agent
        // any of its sessions named, refused where any host refuses it, and the
        // one workspace an agent's grant reaches by the name the record gives it.
        {
            let workspace = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
            let mut grants = certified("Claude Code", 6);
            grants.capabilities = tilo_user_identity::DeviceCapabilities::agent_grant(
                tilo_user_identity::AgentScope::parse(workspace).unwrap(),
            );
            let mut record = Record {
                workspaces: BTreeMap::from([(workspace.to_owned(), "Studio".to_owned())]),
                ..Record::default()
            };
            record.add(RecordDevice {
                last_active_ms: Some(9_000),
                agent: Some("Claude Code".to_owned()),
                ..recorded(grants.clone())
            });
            record.add(RecordDevice {
                last_active_ms: Some(7_000),
                refused: true,
                // A host that has it open now, which said nothing newer.
                online: true,
                ..recorded(grants)
            });
            let devices = with_record(Vec::new(), record);
            assert_eq!(devices.len(), 1, "one key, one line");
            let grant = &devices[0];
            assert_eq!(grant.agent.as_deref(), Some("Claude Code"));
            assert_eq!(grant.last_active_ms, Some(9_000), "the newest time wins");
            assert!(grant.online, "a host that has it open now says so");
            assert!(grant.revoked, "a host that refuses it says so");
            assert_eq!(
                grant.workspace.as_deref(),
                Some("Studio"),
                "the one workspace it reaches, by its name"
            );
            assert_eq!(
                grant.kind,
                tilo_actions::remote::identity_admin::KeyKind::Agent
            );
            assert_eq!(grant.issued_ms, Some(0), "issued at 1 ns, said in ms");
        }

        phase a_key_an_agent_runs_on_is_listed_with_its_agent;
        // A computer an agent works on is a key acting for the person, and the
        // list says the agent and when the key was last here: what a `tilo login`
        // computer's sessions told the person's host.
        {
            let computer = certified("Claude Code on laptop", 7);
            let devices = with_record(
                vec![listed("Claude Code on laptop", 7, true, false)],
                Record {
                    devices: vec![RecordDevice {
                        agent: Some("Claude Code".to_owned()),
                        last_active_ms: Some(6_000),
                        online: true,
                        ..recorded(computer)
                    }],
                    workspaces: BTreeMap::new(),
                },
            );
            let device = &devices[0];
            assert_eq!(device.agent.as_deref(), Some("Claude Code"));
            assert_eq!(device.last_active_ms, Some(6_000));
            assert!(device.online);
            assert_eq!(
                device.kind,
                tilo_actions::remote::identity_admin::KeyKind::Agent,
                "a key an agent runs on acts for the person"
            );
            assert!(device.workspace.is_none());
            assert!(device.current, "the fleet's own word stands");
        }
    }

    tilo_test_env::scenario! {
        fn a_device_key_is_checked_before_anything_runs;

        phase a_device_key_is_thirty_two_bytes_as_hex;
        {
            let key = "aa00000000000000000000000000000000000000000000000000000000000001";
            assert_eq!(
                node_id(&key.to_ascii_uppercase(), "device"),
                Ok(key.to_owned())
            );
            assert!(node_id("abc", "device").is_err());
            assert!(node_id(&"g".repeat(64), "device").is_err());
            assert_eq!(revocation_reason(" Lost "), Ok(RevocationReason::Lost));
            assert!(revocation_reason("misplaced").is_err());
        }
    }
}
