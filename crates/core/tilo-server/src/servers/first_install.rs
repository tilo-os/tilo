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

use std::path::Path;
use std::time::Duration;

use install_profile_model::plan::Drive;
use tilo_workspace_service::workspaces::servers::machine::{self, Step};
use tilo_workspace_service::workspaces::servers::{Servers, device, device_id};
use tilo_workspace_service::workspaces::{ResourceWrite, Workspaces};

use super::{SERVERS, apply, person_workspace_id, read};

/// How long this machine waits for the root helper to say it applied the
/// configuration: longer than the helper's own window for a network it must
/// confirm, so a slow helper is not taken for a refusal.
const APPLY_WAIT: Duration = Duration::from_secs(150);

/// Whether a ticket joined this machine to a person: the certificate its login
/// filed beside the identity root's device key. A machine that founded its own
/// workspace has none, and installs no plan.
#[must_use]
pub fn joined(identity_root: &Path) -> bool {
    let paths = tilo_device_identity::IdentityPaths::under(identity_root);
    tilo_device_identity::login::served_device_key(&paths)
        .and_then(|device| tilo_device_identity::login::ticket_cert(&paths, &device))
        .is_some()
}

/// The drives this machine really has, as a plan's machine reports them.
#[must_use]
pub fn real_drives() -> Vec<Drive> {
    let Ok(output) = std::process::Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,MODEL,SERIAL,WWN,SIZE,TYPE,FSTYPE,MOUNTPOINT",
        ])
        .output()
    else {
        tracing::warn!("this machine cannot list its drives");
        return Vec::new();
    };
    parse_drives(&String::from_utf8_lossy(&output.stdout))
}

/// The drives in `lsblk -J` output: each disk, whether it already holds
/// files, and whether this machine boots from it.
#[must_use]
pub fn parse_drives(listing: &str) -> Vec<Drive> {
    let Ok(document) = serde_json::from_str::<serde_json::Value>(listing) else {
        return Vec::new();
    };
    let Some(devices) = document.get("blockdevices").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut drives = Vec::new();
    for device in devices.iter().filter(|d| name(d, "type") == "disk") {
        let identity = if name(device, "serial").is_empty() {
            name(device, "wwn")
        } else {
            name(device, "serial")
        };
        if identity.is_empty() {
            continue;
        }
        let mut holds_data = false;
        let mut boots = false;
        let mut inspect = |node: &serde_json::Value| {
            holds_data |= !name(node, "fstype").is_empty();
            let mount = name(node, "mountpoint");
            boots |= matches!(mount.as_str(), "/" | "/sysroot" | "/boot" | "/boot/efi");
        };
        inspect(device);
        if let Some(children) = device.get("children").and_then(|c| c.as_array()) {
            for child in children {
                inspect(child);
            }
        }
        drives.push(Drive {
            model: name(device, "model"),
            identity,
            bytes: device
                .get("size")
                .and_then(|size| size.as_u64().or_else(|| size.as_str()?.parse().ok()))
                .unwrap_or(0),
            holds_data,
            boots,
        });
    }
    drives
}

/// One field of `node`, as lsblk spells it in JSON, trimmed and empty when it
/// has none.
fn name(node: &serde_json::Value, field: &str) -> String {
    node.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned()
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
async fn apply_device_config(paths: &device::Paths, entry: &super::Server) -> Result<(), String> {
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
            let drives = parse_drives(listing);
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
            assert!(parse_drives(listing).is_empty());
        }
    }
}
