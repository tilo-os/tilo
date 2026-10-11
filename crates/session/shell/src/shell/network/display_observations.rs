//! Private, root-bound cached monitor facts. This sidecar is not a wire
//! checkpoint and contains neither online authority nor input leases.

use crate::shell::device_fabric::{
    DeviceFabric, DisplayConnectorPlacement, DisplayOutputObservation, FabricStamp, MAX_DEVICES,
};
use crate::shell::personal_continuity::DeviceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::Path;

const SCHEMA_VERSION: u8 = 1;
const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Store {
    schema_version: u8,
    user_public_key: [u8; 32],
    observations: Vec<Observation>,
}

#[derive(Serialize, Deserialize)]
struct Observation {
    owner: DeviceId,
    stamp: FabricStamp,
    connectors: Vec<DisplayConnectorPlacement>,
    outputs: Vec<DisplayOutputObservation>,
}

pub(super) fn save(path: &Path, fabric: &DeviceFabric, user: [u8; 32]) -> Result<(), String> {
    let observations = fabric
        .snapshots()
        .iter()
        .filter_map(|(owner, snapshot)| {
            let display = snapshot.display.as_ref()?;
            Some(Observation {
                owner: owner.clone(),
                stamp: display.stamp.clone(),
                connectors: display.outputs.clone(),
                outputs: fabric.display_outputs(owner)?.to_vec(),
            })
        })
        .collect();
    let store = Store {
        schema_version: SCHEMA_VERSION,
        user_public_key: user,
        observations,
    };
    let bytes = serde_json::to_vec(&store).map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STORE_BYTES {
        return Err("display observation cache is too large".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())
}

pub(super) fn restore(
    path: &Path,
    fabric: &mut DeviceFabric,
    user: [u8; 32],
    enrolled: &BTreeSet<DeviceId>,
) -> Result<bool, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STORE_BYTES {
        return Err("display observation cache is too large".into());
    }
    let store: Store = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if store.schema_version != SCHEMA_VERSION
        || store.user_public_key != user
        || store.observations.len() > MAX_DEVICES
    {
        return Err("display observation cache identity or version does not match".into());
    }
    let mut seen = BTreeSet::new();
    let mut restored = fabric.clone();
    let mut changed = false;
    for observation in store.observations {
        if observation.stamp.device != observation.owner || !seen.insert(observation.owner.clone())
        {
            return Err("display observation cache contains conflicting authorship".into());
        }
        // Forgotten certificates and stale revisions are expected during
        // enrolment changes or a crash between the two atomic store writes.
        // Neither can project hardware or obtain fresh authority.
        if !enrolled.contains(&observation.owner)
            || restored
                .snapshots()
                .get(&observation.owner)
                .and_then(|snapshot| snapshot.display.as_ref())
                .is_none_or(|display| display.stamp != observation.stamp)
        {
            continue;
        }
        changed |= restored
            .observe_display_connectors(
                &observation.owner,
                &observation.stamp,
                observation.connectors,
            )
            .map_err(|error| error.to_string())?;
        changed |= restored
            .observe_display_outputs(&observation.owner, &observation.stamp, observation.outputs)
            .map_err(|error| error.to_string())?;
    }
    *fabric = restored;
    Ok(changed)
}
