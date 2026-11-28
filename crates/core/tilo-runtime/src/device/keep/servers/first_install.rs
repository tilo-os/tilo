//! A machine installing from the plan the person made for it, on its first
//! start.
//!
//! The plan is the machine's entry in its person's Servers workspace
//! ([`tilo_workspace_service::workspaces::servers`]). A machine that came from
//! a plan joined the person's devices with a ticket, and one of their devices
//! took it into its entry under its own device key: it reads what its entry
//! says from then on.
//!
//! This is the machine's side of that: it reports the drives it really has and
//! the choices its plan left it unable to settle
//! ([`machine::report`](tilo_workspace_service::workspaces::servers::machine::report)),
//! waits at its entry until one of the person's devices answered them, applies
//! the configuration its entry asks of its own machine through the root helper
//! (`tilo-server-device-config`) and says it finished. Nothing is guessed: an
//! unanswered choice is waited for, never filled in here.

use std::time::Duration;

use install_profile_model::plan::Drive;
use tilo_workspace_service::workspaces::servers::machine::{self, Step};
use tilo_workspace_service::workspaces::servers::{Servers, device, device_id};
use tilo_workspace_service::workspaces::{ResourceWrite, Workspaces};

use tilo_server_protocol::servers::person_workspace_id;
use tilo_workspace_service::workspaces::servers::{SERVERS, apply, read};

/// How long this machine waits for the root helper to say it applied the
/// configuration: longer than the helper's own window for a network it must
/// confirm, so a slow helper is not taken for a refusal.
const APPLY_WAIT: Duration = Duration::from_secs(150);

/// The columns [`real_drives`] reads, as `lsblk --output` names them.
const DRIVE_COLUMNS: &str = "NAME,MODEL,SERIAL,WWN,SIZE,TYPE,FSTYPE,MOUNTPOINT";

/// The drives this machine really has, as a plan's machine reports them.
#[must_use]
pub fn real_drives() -> Vec<Drive> {
    let Some(devices) = block_devices::lsblk::Lsblk::new(DRIVE_COLUMNS).list() else {
        tracing::warn!("this machine cannot list its drives");
        return Vec::new();
    };
    drives_of(&devices)
}

/// The drives among `devices`: each disk, whether it already holds files, and
/// whether this machine boots from it.
#[must_use]
fn drives_of(devices: &[block_devices::lsblk::BlockDevice]) -> Vec<Drive> {
    let mut drives = Vec::new();
    for device in devices.iter().filter(|device| device.kind == "disk") {
        let Some(identity) = device.serial.clone().or_else(|| device.wwn.clone()) else {
            continue;
        };
        let (mut holds_data, mut boots) = files_on(device);
        for child in &device.children {
            let (child_holds_data, child_boots) = files_on(child);
            holds_data |= child_holds_data;
            boots |= child_boots;
        }
        drives.push(Drive {
            model: device.model.clone().unwrap_or_default(),
            identity,
            bytes: device.size,
            holds_data,
            boots,
        });
    }
    drives
}

/// Whether `node` holds files, and whether it is mounted where this machine boots from.
fn files_on(node: &block_devices::lsblk::BlockDevice) -> (bool, bool) {
    let holds_data = node.fstype.is_some();
    let boots = matches!(
        node.mountpoint.as_deref(),
        Some("/" | "/sysroot" | "/boot" | "/boot/efi")
    );
    (holds_data, boots)
}

/// Install this machine from the plan its own entry names, applying the
/// configuration its entry asks of its own machine, and say it finished in the
/// entry. `drives` are the ones it really has ([`real_drives`], or a test's).
///
/// It waits for its entry while the workspace's document is still reaching it, and at its entry
/// while one of the person's devices has not answered a choice the plan left open: this machine
/// never fills one in itself. A machine whose person planned nothing for it installs nothing.
///
/// # Errors
/// The entry names a plan this build cannot read, the root helper refused the configuration, or the entry could
/// not be read or written.
pub async fn install(
    workspaces: &Workspaces,
    cert: &tilo_user_identity::DeviceCert,
    drives: &[Drive],
    requests: &super::applied::Requests,
) -> Result<(), String> {
    let here = device_id(&cert.device_public_key);
    let mut told_arriving = false;
    // Heard from before the first read, so a change that lands between a read and the wait
    // after it still wakes the wait.
    let mut changes = EntryChanges::of(workspaces);
    loop {
        let servers = read(workspaces).await?;
        // Holding the workspace comes before its document has all reached this machine: the
        // entry the person's device wrote is on its way, and the machine waits for it as it
        // waits for an answer.
        let Some(entry) = servers.servers.get(&here).cloned() else {
            if servers.waiting().next().is_none() {
                return Ok(());
            }
            if !told_arriving {
                tracing::info!("this machine waits for its plan to reach it");
                told_arriving = true;
            }
            changes.next().await?;
            continue;
        };
        // What this machine has is its own report: the entry is told once,
        // and again only when the machine's drives are not the ones it keeps.
        // An entry no plan made is a server that installs nothing.
        if entry.plan.status.is_empty() {
            return Ok(());
        }
        if entry.plan.drives.is_empty() {
            let report = machine::report(&entry, drives)?;
            apply(
                workspaces,
                Servers::BEGIN_INSTALL.with((
                    hex::encode(cert.device_public_key),
                    entry.plan.nonce.clone(),
                    report.drives,
                    report.ask,
                )),
            )
            .await?;
            continue;
        }
        match machine::step(&entry, drives)? {
            Step::Enlist { nonce, report } => {
                apply(
                    workspaces,
                    Servers::BEGIN_INSTALL.with((
                        hex::encode(cert.device_public_key),
                        nonce,
                        report.drives,
                        report.ask,
                    )),
                )
                .await?;
            }
            Step::Wait(choices) => {
                tracing::info!(?choices, "this machine waits for its person to answer");
                changes.next().await?;
            }
            Step::Install(plan) => {
                tracing::info!(plan = %plan.plan, "this machine installs with its plan");
                apply_device_config(&requests.device, &entry).await?;
                apply(workspaces, Servers::FINISH_INSTALL.with(here.clone())).await?;
                tracing::info!(id = %here, "this machine finished installing");
                return Ok(());
            }
            Step::Finished => return Ok(()),
        }
    }
}

/// Apply this machine's own configuration, as its entry says it, through the
/// root helper: write the request and wait for the helper's report of that
/// revision.
async fn apply_device_config(
    paths: &device::Paths,
    entry: &tilo_workspace_service::workspaces::servers::Server,
) -> Result<(), String> {
    // The entry's device configuration may have changed since the last pass:
    // what this machine is, is what the entry says at install time.
    let revision = device::write(entry, paths)?;
    let deadline = std::time::Instant::now()
        .checked_add(APPLY_WAIT)
        .ok_or("this machine cannot wait for its configuration")?;
    while std::time::Instant::now() < deadline {
        if let Some(status) = device::report(paths)
            && !status.revision.is_empty()
            && revision
                .as_deref()
                .is_none_or(|revision| status.revision == revision)
        {
            return if status.state == "applied" {
                Ok(())
            } else {
                Err(format!(
                    "the configuration of this machine was not applied: {}",
                    status.detail
                ))
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("the root helper did not apply the configuration of this machine".to_owned())
}

/// What wakes a machine waiting at its entry: a change to its person's Servers document, or to
/// the governance of the workspace that holds it.
struct EntryChanges {
    admin: String,
    writes: tokio::sync::broadcast::Receiver<ResourceWrite>,
    governance: tokio::sync::broadcast::Receiver<String>,
}

impl EntryChanges {
    fn of(workspaces: &Workspaces) -> Self {
        Self {
            admin: person_workspace_id(&workspaces.device_cert().user_public_key),
            writes: workspaces.document_writes(),
            governance: workspaces.governance_changes(),
        }
    }

    /// Wait until the Servers document or its workspace's governance changed, as a write
    /// another device made reaches this one or this device writes it.
    ///
    /// # Errors
    /// This device's workspaces are closing, and nothing will change again.
    async fn next(&mut self) -> Result<(), String> {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            let ours = tokio::select! {
                write = self.writes.recv() => write.map(|ResourceWrite { workspace, app, .. }| {
                    workspace == self.admin && app == SERVERS.0
                }),
                workspace = self.governance.recv() => workspace.map(|workspace| workspace == self.admin),
            };
            match ours {
                Ok(true) | Err(RecvError::Lagged(_)) => return Ok(()),
                Ok(false) => {}
                Err(RecvError::Closed) => {
                    return Err("this device's workspaces closed while it installed".to_owned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drives in a test's `lsblk -J` document, read as [`real_drives`] reads what this
    /// machine lists.
    fn drives_in(listing: &str) -> Vec<Drive> {
        let devices =
            block_devices::lsblk::parse_slice(listing.as_bytes()).expect("a test's lsblk document");
        drives_of(&devices)
    }

    tilo_test_env::scenario! {
        fn drives_are_read_from_what_lsblk_lists;

        phase a_disk_holds_data_when_anything_on_it_does_and_boots_when_root_or_boot_does;
        {
            let listing = r#"{"blockdevices":[
                {"name":"nvme0n1","model":"Samsung SSD 990","serial":"S6XXNJ0T","wwn":null,
                 "size":2000398934016,"type":"disk","fstype":null,"mountpoint":null,
                 "children":[
                   {"name":"nvme0n1p1","model":null,"serial":null,"wwn":null,"size":1073741824,
                    "type":"part","fstype":"vfat","mountpoint":"/boot/efi"},
                   {"name":"nvme0n1p2","model":null,"serial":null,"wwn":null,"size":1999325550592,
                    "type":"part","fstype":"btrfs","mountpoint":"/"}
                 ]},
                {"name":"sda","model":"QEMU HARDDISK","serial":null,"wwn":"0x5000c500a1b2c3d4",
                 "size":4000787030016,"type":"disk","fstype":null,"mountpoint":null}
            ]}"#;
            let drives = drives_in(listing);
            assert_eq!(drives.len(), 2);
            assert_eq!(drives[0].identity, "S6XXNJ0T");
            assert_eq!(drives[0].model, "Samsung SSD 990");
            assert_eq!(drives[0].bytes, 2_000_398_934_016);
            assert!(drives[0].holds_data, "its partitions hold files");
            assert!(drives[0].boots, "this machine boots from it");
            assert_eq!(drives[1].identity, "0x5000c500a1b2c3d4");
            assert!(!drives[1].holds_data);
            assert!(!drives[1].boots);
        }

        phase a_disk_without_a_serial_or_wwn_is_no_drive_to_report;
        {
            let listing = r#"{"blockdevices":[
                {"name":"sdz","model":null,"serial":null,"wwn":null,"size":1000,
                 "type":"disk","fstype":null,"mountpoint":null}
            ]}"#;
            assert!(drives_in(listing).is_empty());
        }
    }
}
