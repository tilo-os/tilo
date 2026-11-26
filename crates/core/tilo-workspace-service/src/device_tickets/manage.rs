//! The person's devices, as every device of theirs names and revokes them: the `devices` actions
//! [`RENAME`] and [`REVOKE`], answered the same by a computer's daemon and by its desktop. Their
//! devices are the record of the person's own workspace ([`person_workspace::devices`]), which
//! this device holds and every device of theirs reads and writes; a list of them reads that record
//! and joins who is here for online. A revocation is signed by the person's root on the device
//! that holds it and kept in the record's entry of the device it names, root-signed, and every
//! host verifies it there and refuses the device by it.
//!
//! [`RENAME`]: tilo_action_services::device_tickets::RENAME
//! [`REVOKE`]: tilo_action_services::device_tickets::REVOKE

use serde_json::{Value, json};
use tilo_user_identity::{RevocationEntry, RevocationReason};

use super::TicketServer;
use crate::workspaces::person_workspace::devices::{DEVICES, Devices, Listed};
use crate::workspaces::{Workspaces, person_workspace};

impl TicketServer {
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
    /// their own workspace keeps the entry the root signed, and every host that holds the record
    /// refuses the device as the change arrives, ending its links there.
    ///
    /// # Errors
    /// The device is this one, this device does not hold the person's root, or the record
    /// refused the entry.
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
            person_workspace::devices::keep(&workspaces, &known).await?;
        }
        let listed = held(&workspaces, &person, &key)
            .await
            .ok_or_else(|| format!("{key} is no device of yours: `tilo device ls` lists them"))?;
        let mut entry = RevocationEntry {
            user_public_key: person,
            revoked_device_public_key: device,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &*root)
            .map_err(|error| error.to_string())?;
        if !listed.device.revoked(&listed.cert) {
            person_workspace::devices::revoke(&workspaces, &person, &entry).await?;
        }
        // This device refuses the key at once; every other host as the record's change reaches it,
        // and the hosts of the workspaces this device holds as the revocation it announces does:
        // they refuse what the key signs from then on, whichever device sends it.
        workspaces.refresh_blocked().await;
        if let Err(error) = workspaces.announce_revocations(vec![entry.clone()]).await {
            tracing::warn!(%error, "a revocation did not reach every host of this device's workspaces");
        }
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
        .map_err(|_| format!("{text} is not a device's key: `tilo device ls` lists them"))?;
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
