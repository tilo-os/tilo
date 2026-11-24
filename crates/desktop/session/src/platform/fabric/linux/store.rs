//! What the person's devices keep of themselves in the person's own workspace.
//!
//! Each device writes what lasts of it under its own key
//! ([`tilo_workspace_service::workspaces::person_workspace::devices`]): what it is ([`ABOUT`])
//! and its disk-unlock enrolments, which the session's disk unlock keeps and reads
//! ([`crate::network::kept_unlock`]). Its speaker groups are typed actions on the same record
//! ([`tilo_workspace_service::workspaces::person_workspace::speaker`]). The workspace carries all
//! of it between the person's devices, a device that comes up later included.
//!
//! Every value is the postcard bytes of its type, written only when it changed; a value that does
//! not read, or does not hold for the device it is under, is left out, and so is a device whose
//! certificate the person's root did not sign or no longer holds.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

use super::{DeviceAbout, PersonDevice};
use crate::PersonResources;
use crate::platform::fabric::DeviceId;

/// What a device is: its [`DeviceAbout`].
pub(crate) const ABOUT: &str = "about";

/// Keep `value` under `name` among what `device` says of itself. Returns whether it changed.
///
/// # Errors
///
/// The value or the certificate does not encode.
pub(crate) fn keep_own<T: Serialize>(
    resources: &PersonResources,
    device: &DeviceCert,
    name: &'static str,
    value: &T,
) -> Result<bool, String> {
    let value = hex::encode(postcard::to_allocvec(value).map_err(|error| error.to_string())?);
    let certificate =
        hex::encode(postcard::to_allocvec(device).map_err(|error| error.to_string())?);
    let key = hex::encode(device.device_public_key);
    let held = resources.read::<Devices>(DEVICES).is_some_and(|devices| {
        devices
            .devices
            .get(&tilo_resource::Id::from_raw(key.clone()))
            .is_some_and(|kept| {
                kept.certificate == certificate && kept.facts.get(name) == Some(&value)
            })
    });
    if held {
        return Ok(false);
    }
    resources.apply(DEVICES, move || {
        Devices::KEEP_FACT.with((
            key.clone(),
            certificate.clone(),
            name.to_owned(),
            value.clone(),
        ))
    });
    Ok(true)
}

/// What the person's devices keep of themselves: each device of `person` whose certificate holds,
/// with what it said of itself.
pub(crate) fn kept(resources: &PersonResources, person: &[u8; 32]) -> Vec<PersonDevice> {
    let Some(state) = resources.read::<Devices>(DEVICES) else {
        return Vec::new();
    };
    let now = tilo_clock::unix_now_ns();
    state
        .devices
        .iter()
        .filter_map(|(_, kept)| {
            let cert: DeviceCert = decoded(&kept.certificate)?;
            if &cert.user_public_key != person
                || tilo_user_identity::verify_device_cert_at(&cert, now).is_err()
            {
                return None;
            }
            let device = DeviceId::new(hex::encode(cert.device_public_key)).ok()?;
            let fact = |name: &str| kept.facts.get(name);
            let about = fact(ABOUT)
                .and_then(|value| decoded::<DeviceAbout>(value))
                .filter(|about| about.validate().is_ok())
                .unwrap_or_else(|| {
                    DeviceAbout::named(
                        tilo_user_identity::device_name(&cert.device_label).unwrap_or_default(),
                    )
                });
            let mut person_device = PersonDevice::new(device.clone(), about);
            person_device.unlock = crate::network::kept_unlock(&state, kept, &device);
            Some(person_device)
        })
        .collect()
}

/// The value `hex` holds, postcard in hex, when it reads as a `T`.
pub(crate) fn decoded<T: DeserializeOwned>(hex: &str) -> Option<T> {
    tilo_failure::ignored!(
        "a value no build of this person's wrote is not one of their devices' to read",
        postcard::from_bytes(&hex::decode(hex).ok()?).ok()
    )
}
