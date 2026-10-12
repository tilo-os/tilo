//! Private, root-bound manual display-input declines; never roamed or granted.
use super::super::personal_continuity::DeviceId;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

const MAX_DECLINES: usize = 1024;
const MAX_STORE_BYTES: usize = 262_144;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declines {
    schema_version: u16,
    user_public_key: [u8; 32],
    devices: BTreeSet<DeviceId>,
}

pub(super) fn load(path: &Path, user: [u8; 32]) -> Result<BTreeSet<DeviceId>, String> {
    let Some(bytes) = tilo_files::native::bounded::read(path, MAX_STORE_BYTES)
        .map_err(|error| error.to_string())?
    else {
        return Ok(BTreeSet::new());
    };
    let value: Declines = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if value.schema_version != 1
        || value.user_public_key != user
        || value.devices.len() > MAX_DECLINES
    {
        return Err("display input decline identity or bounds are invalid".into());
    }
    Ok(value.devices)
}

pub(super) fn save(
    path: &Path,
    user: [u8; 32],
    devices: &BTreeSet<DeviceId>,
) -> Result<(), String> {
    if devices.len() > MAX_DECLINES {
        return Err("too many display input declines".into());
    }
    let bytes = serde_json::to_vec(&Declines {
        schema_version: 1,
        user_public_key: user,
        devices: devices.clone(),
    })
    .map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())
}
