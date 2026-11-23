//! The person's devices, as every device of theirs lists, names and revokes them: the `devices`
//! actions [`LIST`], [`RENAME`] and [`REVOKE`], answered the same by a computer's daemon and by
//! its desktop. Their list is the one the person's hosts keep beside their workspaces, read and
//! written through those hosts as this device reaches them ([`Issuer::hosts`]); a revocation is
//! signed by the person's root on the device that holds it.
//!
//! [`LIST`]: tilo_actions::remote::device_tickets::LIST
//! [`RENAME`]: tilo_actions::remote::device_tickets::RENAME
//! [`REVOKE`]: tilo_actions::remote::device_tickets::REVOKE

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::remote::device_tickets::{FleetDevice, KeyKind};
use tilo_server_session::workspaces::{
    KEEP_PERSONAL, LIST_PERSONAL, PERSONAL_DEVICE_NAME, PersonalDevice, PersonalList,
};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationReason};

use super::{PersonHost, TicketServer};

/// How many times the person's list is read while a revocation is waited for to show in it.
const REVOKED_LOOKS: u32 = 80;
/// How long apart those reads are: twenty seconds in all.
const REVOKED_LOOK: Duration = Duration::from_millis(250);

impl TicketServer {
    /// The person's devices, as their hosts list them: `{"devices": [FleetDevice]}`, this one
    /// marked current. This device and the keys it knows itself ([`Issuer::known`]) are listed
    /// whether or not a host has them yet.
    ///
    /// # Errors
    /// This device has no identity yet, or none of the person's hosts answered.
    pub async fn list(&self) -> Result<Value, String> {
        let (cert, _) = self.0.issuer.device()?;
        let this = cert.device_public_key;
        let hosts = self.0.issuer.hosts();
        let mut list = if hosts.is_empty() {
            PersonalList::default()
        } else {
            read(&hosts).await?
        };
        for known in self.0.issuer.known().into_iter().chain([cert]) {
            let key = known.device_public_key;
            if !list
                .devices
                .iter()
                .any(|kept| kept.cert.device_public_key == key)
            {
                list.devices.push(PersonalDevice {
                    cert: known,
                    name: None,
                    revoked: false,
                    browser: None,
                    agent: None,
                    last_active_ms: None,
                    online: key == this,
                });
            }
        }
        Ok(json!({ "devices": listed(&list, this) }))
    }

    /// Call the device `args` names what `name` says, in every list.
    ///
    /// # Errors
    /// The arguments name no device or no name, or no host took the name.
    pub async fn rename(&self, args: &Value) -> Result<Value, String> {
        let device = device_in(args)?.ok_or("name the device to rename by its key")?;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("a device's new name is needed")?;
        let key = hex::encode(device);
        let mut named = Err("none of your hosts took the name".to_owned());
        for host in self.hosts()? {
            let answer = host
                .ask(
                    PERSONAL_DEVICE_NAME,
                    Map::from_iter([
                        ("device".to_owned(), key.clone().into()),
                        ("name".to_owned(), name.into()),
                    ]),
                )
                .await;
            match answer {
                Ok(_) => named = Ok(()),
                Err(refusal) if named.is_err() => named = Err(refusal),
                Err(_) => {}
            }
        }
        named?;
        Ok(json!({ "device": key, "name": name }))
    }

    /// End the login of the device `args` names, signed by the person's root here, and wait for
    /// a host to list it as revoked: from then on every device of theirs refuses it.
    ///
    /// # Errors
    /// The device is this one, this device does not hold the person's root, or no host took the
    /// revocation.
    pub async fn revoke(&self, args: &Value) -> Result<Value, String> {
        let device = device_in(args)?.ok_or("name the device to revoke by its key")?;
        let (cert, _) = self.0.issuer.device()?;
        if device == cert.device_public_key {
            return Err("That is this computer: revoke it from another of your devices.".into());
        }
        let reason = reason_in(args)?;
        let root = self.0.issuer.root()?.ok_or(
            "This computer does not hold your identity, so it cannot revoke a device: revoke it \
             on the one that does.",
        )?;
        if root.public_key() != cert.user_public_key {
            return Err(
                "This computer is a device of another person than the one whose identity it holds."
                    .into(),
            );
        }
        let mut entry = RevocationEntry {
            user_public_key: cert.user_public_key,
            revoked_device_public_key: device,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &*root)
            .map_err(|error| error.to_string())?;
        self.0.issuer.keep_revocation(&entry);
        let revocation = serde_json::to_string(&entry).map_err(|error| error.to_string())?;
        let hosts = self.0.issuer.hosts();
        // With none of the person's hosts, the revocation is kept where this device keeps what
        // its root signed, and nowhere else.
        if hosts.is_empty() {
            return Ok(json!({
                "device": hex::encode(device),
                "revoked": true,
                "revocation": revocation,
            }));
        }
        for host in &hosts {
            if let Err(error) = host.revoked(&entry) {
                tracing::info!(host = host.label(), %error, "a host did not hear that the person took a device back");
            }
        }
        for _ in 0..REVOKED_LOOKS {
            if let Some(revoked) = read(&hosts)
                .await?
                .devices
                .iter()
                .find(|kept| kept.cert.device_public_key == device && kept.revoked)
            {
                return Ok(json!({
                    "device": hex::encode(device),
                    "label": named(revoked),
                    "revoked": true,
                    "revocation": revocation,
                }));
            }
            tokio::time::sleep(REVOKED_LOOK).await;
        }
        Err("Your hosts did not take the revocation: run it again.".into())
    }

    /// Keep `cert`, a computer a ticket of this device's just added, in the person's list, so
    /// their other devices list it at once. A device with none of their hosts has no list to keep
    /// it in: a host lists the computer as it enters.
    pub async fn keep_listed(&self, cert: &DeviceCert) {
        let hosts = self.0.issuer.hosts();
        let Some(host) = hosts.first() else {
            return;
        };
        let kept = async {
            let mut list = personal(host.as_ref()).await?;
            list.devices
                .retain(|kept| kept.cert.device_public_key != cert.device_public_key);
            list.devices.push(PersonalDevice {
                cert: cert.clone(),
                name: None,
                revoked: false,
                browser: None,
                agent: None,
                last_active_ms: None,
                online: false,
            });
            let list = serde_json::to_value(&list).map_err(|error| error.to_string())?;
            host.ask(KEEP_PERSONAL, Map::from_iter([("list".to_owned(), list)]))
                .await
                .map(drop)
        };
        if let Err(error) = kept.await {
            tracing::warn!(%error, "the added computer is listed once it enters");
        }
    }

    /// The person's hosts as this device reaches them: why not, with none.
    fn hosts(&self) -> Result<Vec<Arc<dyn PersonHost>>, String> {
        let hosts = self.0.issuer.hosts();
        if hosts.is_empty() {
            return Err(
                "This computer is with none of your hosts yet, which keep your list of devices: \
                 ask again in a moment."
                    .into(),
            );
        }
        Ok(hosts)
    }
}

/// The person's list as `hosts` keep it, each device once: the newest certificate any host has of
/// it, revoked where any host refuses it, online where any has it online.
async fn read(hosts: &[Arc<dyn PersonHost>]) -> Result<PersonalList, String> {
    let mut merged: Option<PersonalList> = None;
    let mut refused = None;
    for host in hosts {
        let list = match personal(host.as_ref()).await {
            Ok(list) => list,
            Err(refusal) => {
                refused = Some(refusal);
                continue;
            }
        };
        let Some(into) = &mut merged else {
            merged = Some(list);
            continue;
        };
        for workspace in list.workspaces {
            if !into
                .workspaces
                .iter()
                .any(|known| known.workspace == workspace.workspace)
            {
                into.workspaces.push(workspace);
            }
        }
        for device in list.devices {
            match into
                .devices
                .iter_mut()
                .find(|known| known.cert.device_public_key == device.cert.device_public_key)
            {
                Some(known) => {
                    known.revoked |= device.revoked;
                    known.online |= device.online;
                    known.name = known.name.take().or(device.name);
                    known.browser = known.browser.take().or(device.browser);
                    known.agent = known.agent.take().or(device.agent);
                    known.last_active_ms = known.last_active_ms.max(device.last_active_ms);
                    if known.cert.issued_at_ns < device.cert.issued_at_ns {
                        known.cert = device.cert;
                    }
                }
                None => into.devices.push(device),
            }
        }
    }
    merged.ok_or_else(|| refused.unwrap_or_else(|| "none of your hosts answered".into()))
}

/// The person's list as `host` keeps it.
async fn personal(host: &dyn PersonHost) -> Result<PersonalList, String> {
    let listed = host.ask(LIST_PERSONAL, Map::new()).await?;
    serde_json::from_value(listed)
        .map_err(|error| format!("{} answered no list of your devices: {error}", host.label()))
}

/// `list`'s devices as a list of them shows them, `this` marked current.
fn listed(list: &PersonalList, this: [u8; 32]) -> Vec<FleetDevice> {
    let names: BTreeMap<&str, &str> = list
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace.as_str(), workspace.name.as_str()))
        .collect();
    list.devices
        .iter()
        .map(|device| {
            let cert = &device.cert;
            let workspace = cert
                .capabilities
                .agent
                .and_then(|scope| scope.workspace_name())
                .map(|id| {
                    names
                        .get(id.as_str())
                        .map_or_else(|| id.clone(), |name| (*name).to_owned())
                });
            FleetDevice {
                label: named(device),
                key_hex: hex::encode(cert.device_public_key),
                fingerprint: fingerprint(&cert.device_public_key),
                current: cert.device_public_key == this,
                revoked: device.revoked,
                kind: if device.agent.is_some() {
                    KeyKind::Agent
                } else {
                    KeyKind::of(
                        cert.capabilities.agent.is_some(),
                        cert.capabilities.server_key,
                    )
                },
                browser: device.browser.clone(),
                agent: device.agent.clone(),
                issued_ms: Some(ms(cert.issued_at_ns)),
                expires_ms: cert.expires_at_ns.map(ms),
                last_active_ms: device.last_active_ms,
                online: device.online,
                workspace,
            }
        })
        .collect()
}

/// What a list calls `device`: the name the person gave it, else the label it was certified with.
fn named(device: &PersonalDevice) -> String {
    device
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| device.cert.device_label.clone())
}

/// A key's short form people compare between screens: its first six bytes.
fn fingerprint(key: &[u8; 32]) -> String {
    key.iter()
        .take(6)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Nanoseconds since the epoch as ms, for a time a list shows.
fn ms(ns: u64) -> i64 {
    i64::try_from(ns / 1_000_000).unwrap_or(i64::MAX)
}

/// The device `args` names by its key, when it names one.
fn device_in(args: &Value) -> Result<Option<[u8; 32]>, String> {
    let Some(text) = args
        .get("device")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return Ok(None);
    };
    let mut key = [0; 32];
    hex::decode_to_slice(text, &mut key)
        .map_err(|_| format!("{text} is not a device's key: `devices#list` lists them"))?;
    Ok(Some(key))
}

/// Why `args` says a device is revoked: retired when it says nothing.
fn reason_in(args: &Value) -> Result<RevocationReason, String> {
    match args
        .get("reason")
        .and_then(Value::as_str)
        .map(|reason| reason.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("" | "retired") => Ok(RevocationReason::Retired),
        Some("lost") => Ok(RevocationReason::Lost),
        Some("stolen") => Ok(RevocationReason::Stolen),
        Some("compromised") => Ok(RevocationReason::Compromised),
        Some(other) => Err(format!(
            "{other} is no reason a device is revoked: lost, stolen, retired or compromised"
        )),
    }
}
