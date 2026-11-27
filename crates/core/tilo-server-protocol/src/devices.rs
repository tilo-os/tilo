//! The person's devices: the rule a device's name follows, and the list of them every device of
//! theirs shows ([`fleet`]), joined from the records of their own workspace and who is here.

use crate::device_tickets::{FleetDevice, KeyKind};
use tilo_actions::remote::Here;

/// Whether a device's name is safe to render as one line.
pub fn valid_device_label(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label.len() <= 80
        && label.chars().all(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
}

/// The record of the person's keys in their own workspace: each key's certificate, what it is
/// (a device, an agent's grant, a script's key), the name the person gave it, the agent it runs
/// and whether their root took it back.
pub const DEVICES: (&str, &str) = ("so.tilo.devices", "devices");

/// The record of which browser each tab key is a tab of, in the person's own workspace.
pub const TAB_GROUPS: (&str, &str) = ("so.tilo.shell", "tab-groups");

/// The fact under which a device's entry keeps when a session of it was last with a host: ms
/// since the epoch, little-endian, in hex.
const SEEN_FACT: &str = "seen";

/// The value of the one record a record read (`{"records":[{"value"}]}`) answered: `null` when
/// it is absent or still arriving.
#[must_use]
pub fn record_value(readings: &serde_json::Value) -> serde_json::Value {
    readings
        .get("records")
        .and_then(|records| records.get(0))
        .and_then(|record| record.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// The name each of the person's devices reads by, by its key, as their devices record keeps
/// it: the name they gave it, else the label its certificate was made with.
#[must_use]
pub fn names(records: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    let Some(entries) = records
        .get("devices")
        .and_then(serde_json::Value::as_object)
    else {
        return std::collections::BTreeMap::new();
    };
    entries
        .iter()
        .filter_map(|(key, entry)| {
            // A collection keeps each item under its id, with its place beside it.
            let device = entry.get("v").unwrap_or(entry);
            let certificate = device.get("certificate")?.as_str()?;
            let cert: tilo_user_identity::DeviceCert =
                postcard::from_bytes(&hex::decode(certificate).ok()?).ok()?;
            let name = device
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map_or_else(|| cert.device_label.clone(), str::to_owned);
            Some((key.clone(), name))
        })
        .collect()
}

/// The values of the records [`fleet`] joins, as the record API reads them from the person's
/// own workspace.
#[derive(Clone, Debug, Default)]
pub struct FleetRecords {
    /// Their devices record ([`DEVICES`]).
    pub devices: serde_json::Value,
    /// Their tab groups record ([`TAB_GROUPS`]), `null` for none.
    pub tab_groups: serde_json::Value,
    /// Their list of workspaces, `null` for none: it names the workspace an agent's key reaches.
    pub workspaces: serde_json::Value,
}

/// The device kept under `key` in `records`, the value of the person's devices record
/// ([`DEVICES`]), when `person`'s root signed its certificate: the certificate, and the revocation
/// that root signed for it when the entry the record keeps verifies. `None` for an entry another
/// root signed, one kept under another key, or one whose certificate does not read.
#[must_use]
pub fn kept(
    records: &serde_json::Value,
    key: &str,
    person: &[u8; 32],
) -> Option<(
    tilo_user_identity::DeviceCert,
    Option<tilo_user_identity::RevocationEntry>,
)> {
    let entry = records.get("devices")?.as_object()?.get(key)?.clone();
    // A collection keeps each item under its id, with its place beside it.
    let device = entry.get("v").unwrap_or(&entry);
    let cert: tilo_user_identity::DeviceCert =
        postcard::from_bytes(&hex::decode(device.get("certificate")?.as_str()?).ok()?).ok()?;
    // Each device is kept under its own key, which its certificate names.
    if cert.user_public_key != *person
        || key != hex::encode(cert.device_public_key)
        || tilo_user_identity::verify_device_cert(&cert).is_err()
    {
        return None;
    }
    let revocation = device
        .get("revocation")
        .and_then(serde_json::Value::as_str)
        .and_then(|entry| tilo_user_identity::revocation_of(entry, &cert));
    Some((cert, revocation))
}

/// The person's devices as every list of them shows them, joined from `records` and from who is
/// here now (`here`), whose devices are online. `this` is the device the list is shown on.
///
/// A device is listed when `person`'s root signed its certificate: an entry another root signed,
/// kept under another key, or whose certificate does not read, is left out. One is revoked when
/// the entry the record keeps beside its certificate is that root's own signature.
#[must_use]
pub fn fleet(
    person: &[u8; 32],
    this: &[u8; 32],
    records: &FleetRecords,
    here: &[Here],
) -> Vec<FleetDevice> {
    let online: std::collections::BTreeSet<&str> = here
        .iter()
        .flat_map(|entry| entry.devices.iter().map(String::as_str))
        .collect();
    let Some(entries) = records
        .devices
        .get("devices")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(key, entry)| {
            // A collection keeps each item under its id, with its place beside it.
            let device = entry.get("v").unwrap_or(entry);
            let text = |field: &str| {
                device
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            let (cert, revocation) = kept(&records.devices, key, person)?;
            let agent = text("agent");
            let seen = device
                .get("facts")
                .and_then(|facts| facts.get(SEEN_FACT))
                .and_then(serde_json::Value::as_str)
                .and_then(|seen| hex::decode(seen).ok())
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                .map(i64::from_le_bytes);
            let workspace = cert
                .capabilities
                .agent
                .as_ref()
                .and_then(tilo_user_identity::AgentScope::workspace_name)
                .map(|id| {
                    // The list keeps each workspace under its id, its fields under `v`.
                    records
                        .workspaces
                        .get("workspaces")
                        .and_then(|listed| listed.get(&id))
                        .and_then(|listed| listed.get("v"))
                        .and_then(|listed| listed.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map_or(id, str::to_owned)
                });
            let current = cert.device_public_key == *this;
            Some(FleetDevice {
                label: text("name")
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| cert.device_label.clone()),
                key_hex: key.clone(),
                fingerprint: cert
                    .device_public_key
                    .iter()
                    .take(6)
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                current,
                revoked: revocation.is_some(),
                kind: device
                    .get("kind")
                    .and_then(|kind| serde_json::from_value::<KeyKind>(kind.clone()).ok())
                    .unwrap_or_else(|| {
                        KeyKind::of(
                            cert.capabilities.placing.is_some(),
                            cert.capabilities.server_key,
                            cert.capabilities.agent.is_some(),
                        )
                    }),
                browser: records
                    .tab_groups
                    .get(format!("tab:{key}"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                agent,
                issued_ms: Some(ms(cert.issued_at_ns)),
                expires_ms: cert.expires_at_ns.map(ms),
                last_active_ms: seen,
                online: current || online.contains(key.as_str()),
                workspace,
            })
        })
        .collect()
}

/// Nanoseconds since the epoch as ms, for a time a list shows.
fn ms(ns: u64) -> i64 {
    i64::try_from(ns / 1_000_000).unwrap_or(i64::MAX)
}
