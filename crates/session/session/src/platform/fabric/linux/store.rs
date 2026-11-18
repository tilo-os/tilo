//! What the person's devices keep of themselves in the person's own store.
//!
//! Each device writes what lasts of it in its own part of the store
//! ([`tilo_peer_state::DEVICE_FACTS`]), which no other device may write: what it is
//! ([`ABOUT`]), the speaker groups it leads and joined and the outputs it offers them
//! ([`SPEAKERS`]), and its disk-unlock enrolments with the answers it gave ([`UNLOCK`]). The Wi-Fi
//! passphrases any of them learned are the person's, one per network
//! ([`tilo_peer_state::WIFI_KIND`]), each sealed to the devices that may open it. The store carries
//! all of it between the person's devices as it carries their settings and their workspace list:
//! signed by the device that wrote it, admitted only from a device their root certified, kept, and
//! handed to a device that comes up later by any sibling.
//!
//! Every value is the postcard bytes of its type under its key, written only when it changed; a
//! value that does not read, or does not hold for the device it is under, is left out.

use std::time::SystemTime;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _, ScalarValue};
use collab_types::CommitNamed as _;
use collab_types::DocumentMutation;
use collab_types::signer::DeviceSigner;
use neighbour_wire::NeighbourMsg;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tilo_peer_state::{DEVICE_FACTS, PersonalStore, PersonalSync, WIFI_KIND};

use super::{DeviceAbout, PersonDevice, SpeakerFabricState, UnlockFabricState, WifiCredential};
use crate::platform::fabric::DeviceId;

/// What a device is: its [`DeviceAbout`].
pub(crate) const ABOUT: &str = "about";
/// Its speaker groups, memberships, outputs and microphones: its [`SpeakerFabricState`].
pub(crate) const SPEAKERS: &str = "speakers";
/// Its disk unlock: its [`UnlockFabricState`].
pub(crate) const UNLOCK: &str = "unlock";

/// The kind the changes of a device's own part and of the person's Wi-Fi are named as.
const CHANGE_KIND: &str = "device";

/// Keep `value` under `key` in this device's own part of the person's store. Returns whether it
/// changed.
///
/// # Errors
///
/// The value does not encode, or the store refused the write.
pub(crate) fn keep_own<T: Serialize>(
    personal: &PersonalSync,
    key: &str,
    value: &T,
) -> Result<bool, String> {
    let held = personal.read(|store| {
        let device = store.device().device_public_key;
        store.device_part(&device, DEVICE_FACTS)
    });
    keep(
        personal,
        held.as_deref(),
        key,
        value,
        |store, mutation, signer, now| {
            store.publish_device_part(DEVICE_FACTS, mutation, signer, now)
        },
    )
}

/// Keep `credential` among the Wi-Fi passphrases the person's devices share, replacing what was
/// kept for its network. Returns whether it changed.
///
/// # Errors
///
/// The credential does not encode, or the store refused the write.
pub(crate) fn keep_wifi(
    personal: &PersonalSync,
    credential: &WifiCredential,
) -> Result<bool, String> {
    let held = personal.read(|store| store.person_resource(WIFI_KIND));
    keep(
        personal,
        held.as_deref(),
        &hex::encode(credential.network_id),
        credential,
        |store, mutation, signer, now| {
            store.publish_person_resource(WIFI_KIND, mutation, signer, now)
        },
    )
}

/// What the person's devices keep of themselves: each device the store holds a certificate of,
/// with what it wrote in its own part, and the Wi-Fi passphrases they share.
pub(crate) fn kept(personal: &PersonalSync) -> (Vec<PersonDevice>, Vec<WifiCredential>) {
    personal.read(|store| {
        let now = tilo_clock::wall();
        let devices = store
            .devices()
            .into_iter()
            .filter(|cert| store.admits_device(&cert.device_public_key, now).is_ok())
            .filter_map(|cert| {
                let device = DeviceId::new(hex::encode(cert.device_public_key)).ok()?;
                let part = store
                    .device_part(&cert.device_public_key, DEVICE_FACTS)
                    .and_then(|bytes| AutoCommit::load(&bytes).ok());
                let about = part
                    .as_ref()
                    .and_then(|part| value::<DeviceAbout>(part, ABOUT))
                    .filter(|about| about.validate().is_ok())
                    .unwrap_or_else(|| {
                        DeviceAbout::named(
                            tilo_user_identity::device_name(&cert.device_label).unwrap_or_default(),
                        )
                    });
                let mut kept = PersonDevice::new(device.clone(), about);
                if let Some(part) = &part {
                    kept.speakers = value::<SpeakerFabricState>(part, SPEAKERS)
                        .filter(|speakers| speakers.validate(&device).is_ok())
                        .unwrap_or_default();
                    kept.unlock = value::<UnlockFabricState>(part, UNLOCK)
                        .filter(|unlock| unlock.validate(&device).is_ok())
                        .unwrap_or_default();
                }
                Some(kept)
            })
            .collect();
        let wifi = store
            .person_resource(WIFI_KIND)
            .and_then(|bytes| AutoCommit::load(&bytes).ok())
            .map(|doc| {
                doc.keys(automerge::ROOT)
                    .filter_map(|key| value::<WifiCredential>(&doc, &key))
                    .filter(|credential| credential.validate().is_ok())
                    .collect()
            })
            .unwrap_or_default();
        (devices, wifi)
    })
}

/// Write `value` under `key` of the document `held` (none yet when `None`) and publish the change
/// with `publish`, unless the document holds that value already.
fn keep<T: Serialize>(
    personal: &PersonalSync,
    held: Option<&[u8]>,
    key: &str,
    value: &T,
    publish: impl FnOnce(
        &mut PersonalStore,
        &DocumentMutation,
        &dyn DeviceSigner,
        SystemTime,
    ) -> Result<NeighbourMsg, String>,
) -> Result<bool, String> {
    let bytes = postcard::to_allocvec(value).map_err(|error| error.to_string())?;
    let mut doc = match held.map(AutoCommit::load) {
        Some(Ok(doc)) => doc,
        Some(Err(error)) => return Err(error.to_string()),
        None => AutoCommit::new(),
    };
    if raw(&doc, key).as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    let base = doc.get_heads();
    doc.put(automerge::ROOT, key, ScalarValue::Bytes(bytes))
        .map_err(|error| error.to_string())?;
    doc.commit_edit(CHANGE_KIND, key);
    let mutation = if held.is_some() {
        DocumentMutation::Incremental(doc.save_after(&base))
    } else {
        DocumentMutation::Snapshot(doc.save())
    };
    personal.publish(|store, signer, now| publish(store, &mutation, signer, now))?;
    Ok(true)
}

/// The value under `key` of `doc`, when it reads as a `T`.
fn value<T: DeserializeOwned>(doc: &AutoCommit, key: &str) -> Option<T> {
    tilo_failure::ignored!(
        "a value no build of this person's wrote is not one of their devices' to read",
        postcard::from_bytes(&raw(doc, key)?).ok()
    )
}

/// The bytes under `key` of `doc`.
fn raw(doc: &AutoCommit, key: &str) -> Option<Vec<u8>> {
    let (value, _) = doc.get(automerge::ROOT, key).ok()??;
    match value {
        automerge::Value::Scalar(scalar) => match scalar.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        },
        automerge::Value::Object(_) => None,
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
