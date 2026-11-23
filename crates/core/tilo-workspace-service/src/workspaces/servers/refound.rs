//! A Servers workspace founded in its person's name, founded again under one
//! device.
//!
//! An earlier build founded the Servers workspace alike on each of its person's
//! devices: its founder row names the person's root, not a device. A workspace
//! is founded by one device, which founds its key group and is the only
//! founder the governance of every copy of it accepts. So such a copy is
//! founded once more, the way any workspace is, and the old one goes.
//!
//! One holder does it: the first of the person's key-holding devices the old
//! copy names, by device key, so the person's devices never found it twice.
//! It keeps what the old copy holds (its records, and the holders it names),
//! lets the old copy go, founds the workspace again under the same id, writes
//! the records back and hands the new copy to each of those holders. A holder
//! takes the new copy in place of its old one when it is handed it: the
//! founder is one of the holders its own copy names. What it keeps is
//! written beside the workspaces before the old copy goes, so a device stopped
//! halfway finishes on its next start, and a holder that could not be reached
//! is handed the copy again on each later pass until it took it.
//!
//! The servers' lease for an update is the old copy's and goes with it, so a
//! server restarting into an update while the workspace is founded again may
//! restart beside another.

use std::path::PathBuf;

use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_peer_state::CopyRole;

use super::{PERSON_WORKSPACE_NAME, holds_person_workspace, person_workspace_id};
use crate::workspaces::Workspaces;

/// What the device founding the workspace again keeps of the old copy until
/// every holder it named took the new one.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Carried {
    /// The old copy's records, `(app, record, saved document)`, until they are
    /// written into the new copy.
    records: Vec<(String, String, Vec<u8>)>,
    /// The old copy's other holders, by device key, the address each signed
    /// and whether it holds the keys, until each took the new copy.
    holders: Vec<([u8; 32], EndpointAddr, bool)>,
}

/// Found this device's person's Servers workspace again under this device,
/// Found the Servers workspace again under `own` when this device is the one to do it: its
/// governance names the person its founder is, and no earlier holder founds it first. Keeps what
/// the old copy holds in `kept`, and discards it. Whether it founded it.
async fn found_again(
    workspaces: &Workspaces,
    kept: &std::path::Path,
    own: &tilo_user_identity::DeviceCert,
    admin: &str,
) -> Result<bool, String> {
    if kept.exists() {
        return Ok(false);
    }
    let Some(state) = workspaces.state(admin).await else {
        return Ok(false);
    };
    let governance = state.governance();
    if governance.founder() != Some(own.user_public_key) {
        return Ok(false);
    }
    let keepers = governance.keeping_devices();
    let first = keepers
        .iter()
        .filter(|hint| {
            hint.host.user_public_key == own.user_public_key
                && tilo_peer_state::holder_role(hint) == CopyRole::KeyHolder
        })
        .map(|hint| hint.host.device_public_key)
        .min();
    if first != Some(own.device_public_key) {
        // Another holder founds it again, and hands this device the new
        // copy.
        return Ok(false);
    }
    let mut carried = Carried::default();
    for (app, record) in workspaces
        .workspace_document_routes(admin)
        .await
        .map_err(|error| error.to_string())?
    {
        if collab_types::is_store_document(&record) {
            continue;
        }
        if let Some(bytes) = workspaces
            .resource_document(admin, &app, &record)
            .await
            .map_err(|error| error.to_string())?
        {
            carried.records.push((app, record, bytes));
        }
    }
    for hint in &keepers {
        if hint.host.device_public_key == own.device_public_key
            || hint.host.user_public_key != own.user_public_key
        {
            continue;
        }
        let Some(address) = tilo_failure::ignored!(
            "an address another build signed is not one this one dials",
            WorkspaceDeviceHintV1::from_transport_hint(hint).ok()
        ) else {
            continue;
        };
        carried.holders.push((
            hint.host.device_public_key,
            address.endpoint_addr,
            address.holds_keys,
        ));
    }
    write(kept, &carried)?;
    let workspace_id = tilo_server_protocol::WorkspaceId::parse(admin)
        .ok_or("the Servers workspace's id is not a workspace id")?;
    workspaces.discard(admin, workspace_id).await?;
    tracing::info!(%admin, records = carried.records.len(), holders = carried.holders.len(), "founding the Servers workspace again under this device");
    Ok(true)
}

/// # Errors
/// What the old copy holds could not be kept, the workspace could not be
/// founded again, or a record could not be written back.
pub(super) async fn refound(workspaces: &Workspaces) -> Result<bool, String> {
    let own = workspaces.serving_certificate().clone();
    let admin = person_workspace_id(&own.user_public_key);
    let kept = kept_at(workspaces, &admin);
    if !found_again(workspaces, &kept, &own, &admin).await? {
        return Ok(false);
    }
    let mut carried: Carried =
        postcard::from_bytes(&std::fs::read(&kept).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if !holds_person_workspace(workspaces).await {
        workspaces
            .create_workspace(PERSON_WORKSPACE_NAME, Some(&admin))
            .await?;
    }
    for (app, record, bytes) in std::mem::take(&mut carried.records) {
        let there = workspaces
            .resource_document(&admin, &app, &record)
            .await
            .map_err(|error| error.to_string())?;
        if there.is_some() {
            continue;
        }
        let document = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        Box::pin(workspaces.author_workspace_document_from(
            (&admin, &own),
            (&app, &record),
            Some(Box::new(document)),
            |_| Ok(()),
        ))
        .await
        .map_err(|error| error.to_string())?;
        workspaces.wrote(&admin, &app, &record);
    }
    write(&kept, &carried)?;
    let mut left = Vec::new();
    for (device, addr, holds_keys) in std::mem::take(&mut carried.holders) {
        let role = if holds_keys {
            CopyRole::KeyHolder
        } else {
            CopyRole::Store
        };
        match workspaces
            .attach_server(&admin, device, addr.clone(), role)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                tracing::info!(holder = %hex::encode(device), %error, "a holder of the Servers workspace did not take its new founding yet");
                left.push((device, addr, holds_keys));
            }
        }
    }
    carried.holders = left;
    if carried.holders.is_empty() {
        std::fs::remove_file(&kept).map_err(|error| error.to_string())?;
    } else {
        write(&kept, &carried)?;
    }
    Ok(true)
}

/// Where the device founding `admin` again keeps what it carries: beside the
/// workspaces' records, under a name that is no workspace's, so letting the
/// old copy go leaves it.
fn kept_at(workspaces: &Workspaces, admin: &str) -> PathBuf {
    workspaces.inner.dir.join(format!("{admin}-refounded"))
}

fn write(path: &std::path::Path, carried: &Carried) -> Result<(), String> {
    tilo_files::native::atomic::write(
        path,
        &postcard::to_stdvec(carried).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}
