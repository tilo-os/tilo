//! The person's devices, as every device of theirs lists, names and revokes them: the `devices`
//! actions [`LIST`], [`RENAME`] and [`REVOKE`], answered the same by a computer's daemon and by
//! its desktop. Their devices are the record of the person's own workspace
//! ([`person_workspace::devices`]), which this device holds and every device of theirs reads and
//! writes; whether one is online comes from presence, never from the record. A revocation is
//! signed by the person's root on the device that holds it and marked in the record, and the
//! marked record is what every host refuses the device by.
//!
//! [`LIST`]: tilo_actions::remote::device_tickets::LIST
//! [`RENAME`]: tilo_actions::remote::device_tickets::RENAME
//! [`REVOKE`]: tilo_actions::remote::device_tickets::REVOKE

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tilo_actions::remote::device_tickets::{FleetDevice, KeyKind};
use tilo_user_identity::{RevocationEntry, RevocationReason};

use super::TicketServer;
use crate::workspaces::person_workspace::devices::{DEVICES, Device, Devices, Listed};
use crate::workspaces::{Workspaces, person_workspace};

impl TicketServer {
    /// The person's devices, as their own workspace's record lists them: `{"devices":
    /// [FleetDevice]}`, this one marked current. This device and the keys it knows itself
    /// ([`Issuer::known`]) are listed whether or not the record has them yet. A device is online
    /// when it is this one, presence here says so ([`Issuer::present`]), or it holds a live
    /// session with this device's workspace service.
    ///
    /// # Errors
    /// This device has no identity yet.
    ///
    /// [`Issuer::known`]: super::Issuer::known
    /// [`Issuer::present`]: super::Issuer::present
    pub async fn list(&self) -> Result<Value, String> {
        let (cert, _) = self.0.issuer.device()?;
        let person = cert.user_public_key;
        let this = cert.device_public_key;
        let mut present: BTreeSet<[u8; 32]> = self.0.issuer.present().into_iter().collect();
        let (mut devices, browsers, names): (Vec<Listed>, BTreeMap<_, _>, BTreeMap<_, _>) =
            match self.0.issuer.servers() {
                Some(workspaces) => {
                    present.extend(workspaces.present_devices().await);
                    (
                        workspaces.devices_of(&person).await.listed(&person),
                        workspaces.tab_groups(&person).await,
                        workspaces
                            .list()
                            .await
                            .into_iter()
                            .map(|held| (held.workspace, held.name))
                            .collect(),
                    )
                }
                None => (Vec::new(), BTreeMap::new(), BTreeMap::new()),
            };
        for known in self.0.issuer.known().into_iter().chain([cert]) {
            let key = hex::encode(known.device_public_key);
            if !devices.iter().any(|kept| kept.key == key) {
                devices.push(Listed {
                    key,
                    cert: known,
                    device: Device::default(),
                });
            }
        }
        let online = |key: &[u8; 32]| *key == this || present.contains(key);
        Ok(json!({ "devices": listed(&devices, &browsers, &names, this, online) }))
    }

    /// Call the device `args` names what `name` says, in every list: the record of the person's
    /// own workspace takes the name.
    ///
    /// # Errors
    /// The arguments name no device or no name, this device holds no record of the person's
    /// devices, or the record refused the name.
    pub async fn rename(&self, args: &Value) -> Result<Value, String> {
        let device = device_in(args)?.ok_or("name the device to rename by its key")?;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("a device's new name is needed")?;
        let key = hex::encode(device);
        let workspaces = self.record()?;
        person_workspace::apply(
            &workspaces,
            DEVICES,
            Devices::NAME_DEVICE.with((key.clone(), name.to_owned())),
        )
        .await?;
        Ok(json!({ "device": key, "name": name }))
    }

    /// End the login of the device `args` names, signed by the person's root here: the record of
    /// their own workspace marks it taken back, and every host that holds the record refuses it
    /// as the change arrives, ending its links there.
    ///
    /// # Errors
    /// The device is this one, this device does not hold the person's root, or the record
    /// refused the mark.
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
        let person = cert.user_public_key;
        let key = hex::encode(device);
        let workspaces = self.record()?;
        // A key this device's root certified that the record does not hold yet is taken in
        // first, so the record has the device to mark.
        if held(&workspaces, &person, &key).await.is_none()
            && let Some(known) = self
                .0
                .issuer
                .known()
                .into_iter()
                .find(|known| known.device_public_key == device)
        {
            person_workspace::apply(&workspaces, DEVICES, Devices::keeping(&known)?).await?;
        }
        let listed = held(&workspaces, &person, &key)
            .await
            .ok_or_else(|| format!("{key} is no device of yours: `devices#list` lists them"))?;
        let mut entry = RevocationEntry {
            user_public_key: person,
            revoked_device_public_key: device,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &*root)
            .map_err(|error| error.to_string())?;
        self.0.issuer.keep_revocation(&entry);
        if !listed.device.revoked {
            person_workspace::apply(
                &workspaces,
                DEVICES,
                Devices::REVOKE_DEVICE.with(key.clone()),
            )
            .await?;
        }
        // This device refuses the key at once; every other host as the record's change reaches it.
        workspaces.refresh_blocked().await;
        Ok(json!({
            "device": key,
            "revoked": true,
            "revocation": serde_json::to_string(&entry).map_err(|error| error.to_string())?,
            "label": listed.device.called(&listed.cert),
        }))
    }

    /// The workspace service holding the record of the person's devices: why not, with none.
    fn record(&self) -> Result<Workspaces, String> {
        self.0.issuer.servers().ok_or_else(|| {
            "This computer does not hold your devices' record yet: ask again in a moment.".into()
        })
    }
}

/// The device `key` of `person`'s, as the record `workspaces` holds lists it.
async fn held(workspaces: &Workspaces, person: &[u8; 32], key: &str) -> Option<Listed> {
    workspaces
        .devices_of(person)
        .await
        .listed(person)
        .into_iter()
        .find(|listed| listed.key == key)
}

/// `devices` as a list of them shows them: each tab key as its browser's (`browsers`), an
/// agent's grant with the name of the workspace it reaches (`names`), `this` marked current and
/// each that `online` says so of online.
fn listed(
    devices: &[Listed],
    browsers: &BTreeMap<String, String>,
    names: &BTreeMap<String, String>,
    this: [u8; 32],
    online: impl Fn(&[u8; 32]) -> bool,
) -> Vec<FleetDevice> {
    devices
        .iter()
        .map(|listed| {
            let (cert, device) = (&listed.cert, &listed.device);
            let workspace = cert
                .capabilities
                .agent
                .and_then(|scope| scope.workspace_name())
                .map(|id| names.get(id.as_str()).cloned().unwrap_or(id));
            FleetDevice {
                label: device.called(cert),
                key_hex: listed.key.clone(),
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
                browser: browsers.get(&listed.key).cloned(),
                agent: device.agent.clone(),
                issued_ms: Some(ms(cert.issued_at_ns)),
                expires_ms: cert.expires_at_ns.map(ms),
                last_active_ms: device.seen_ms(),
                online: online(&cert.device_public_key),
                workspace,
            }
        })
        .collect()
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
