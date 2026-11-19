//! A machine installing from the plan the person made for it, on its first
//! start.
//!
//! The plan is the machine's entry in its person's Servers workspace
//! ([`tilo_workspace_service::workspaces::servers`]). A machine that came from
//! a plan's token enrolled with it, so the person's devices know it and one of
//! them approved it: its entry is named by its own device key from then on,
//! and it reads what its entry says.
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
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::servers::machine::{self, Step};
use tilo_workspace_service::workspaces::servers::{Servers, device, device_id};

use super::{apply, read};

/// Where the token's nonce is kept for this machine: written by
/// `tilo-enroll enroll` when it enrolled with a plan's token, and read here
/// after the enrollment.
pub const NONCE: &str = "install-plan.nonce";

/// How often this machine reads its entry back while it waits for an answer.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// How long this machine waits for the root helper to say it applied the
/// configuration: longer than the helper's own window for a network it must
/// confirm, so a slow helper is not taken for a refusal.
const APPLY_WAIT: Duration = Duration::from_secs(150);

/// The nonce of the token this machine enrolled with, when it was handed a
/// plan: `tilo-enroll enroll` writes it beside this server's state.
#[must_use]
pub fn recorded_nonce(state_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_root.join(NONCE)).ok()?;
    let nonce = text.trim();
    (nonce.len() == 32
        && nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| nonce.to_owned())
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

/// Install this machine from the plan its token `nonce` names, applying the
/// configuration its entry asks of its own machine, and say it finished in the
/// entry. `drives` are the ones it really has ([`real_drives`], or a test's).
///
/// It waits at its entry while one of the person's devices has not answered a
/// choice the plan left open: this machine never fills one in itself.
///
/// # Errors
/// No planned machine answers that token, the entry names a plan this build
/// cannot read, the root helper refused the configuration, or the entry could
/// not be read or written.
pub async fn install(
    workspaces: &Workspaces,
    cert: &tilo_user_identity::DeviceCert,
    nonce: &str,
    drives: &[Drive],
    requests: &super::applied::Requests,
) -> Result<(), String> {
    tracing::info!(%nonce, "this machine installs from its plan");
    loop {
        let servers = read(workspaces).await?;
        let (id, entry) = servers
            .planned_by_nonce(nonce)
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .ok_or("no planned machine answers that token")?;
        // What this machine has is its own report: the entry is told once,
        // and again only when the machine's drives are not the ones it keeps.
        if entry.plan.drives.is_empty() {
            let report = machine::report(&entry, drives)?;
            apply(
                workspaces,
                Servers::BEGIN_INSTALL.with((
                    hex::encode(cert.device_public_key),
                    nonce.to_owned(),
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
                wait_for_entry().await;
            }
            Step::Install(plan) => {
                tracing::info!(plan = %plan.plan, "this machine installs with its plan");
                apply_device_config(&requests.device, &entry).await?;
                apply(
                    workspaces,
                    Servers::FINISH_INSTALL.with(device_id(&cert.device_public_key)),
                )
                .await?;
                tracing::info!(%id, "this machine finished installing");
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

/// Wait a moment and read the entry again: an answer one of the person's
/// devices wrote reaches this copy through a holder's, which the write watch
/// does not always hear, so the entry is read back as the document stands.
async fn wait_for_entry() {
    tokio::time::sleep(LOOK_EVERY).await;
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

        phase the_nonce_is_read_only_when_it_names_a_token;
        {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(recorded_nonce(dir.path()), None);
            std::fs::write(dir.path().join(NONCE), "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a\n").unwrap();
            assert_eq!(
                recorded_nonce(dir.path()).as_deref(),
                Some("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a")
            );
            std::fs::write(dir.path().join(NONCE), "not a nonce").unwrap();
            assert_eq!(recorded_nonce(dir.path()), None);
        }
    }
}
