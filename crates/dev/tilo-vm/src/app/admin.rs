//! Joining this machine as one of the owner's devices, so it may administer
//! their devices over the peer network (`vm enrol-admin`). The identity lives
//! beside the registry ([`config::admin_identity_root`]): `tilo device add
//! --capability admin` on a device that holds the owner's identity prints the
//! ticket, and `tilo login` here joins this machine with it.

use std::path::Path;

use serde_json::json;

use super::{App, Outcome};
use crate::config;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, DEVICE, Runner as _};
use crate::remote;

fn default_label() -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this machine".into());
    format!("{host} (tilo-vm)")
}

/// The `tilo device add --capability admin` line an owner device runs for this
/// machine, printing the ticket as one JSON line and returning without waiting
/// for the login that spends it. As root the identity is the system's.
pub fn add_line(as_root: bool) -> String {
    let mut line = String::from("tilo device add --capability admin --json --no-wait");
    if as_root {
        line =
            format!("TILO_IDENTITY_ROOT=/var/lib/tilo XDG_STATE_HOME=/var/lib/tilo/state {line}");
    }
    line
}

pub fn add_by_hand() -> String {
    "tilo device add --capability admin".to_owned()
}

#[derive(Debug, Default, serde::Deserialize)]
struct Enrolment {
    device: String,
    identity: Option<String>,
    label: Option<String>,
    administrator: bool,
}

impl App {
    fn admin_tool(&mut self, args: &[&str]) -> Result<Enrolment, CliError> {
        let mut line = CommandLine::new(&remote::peer_tool(), args);
        line.args.push("--identity-root".into());
        line.args.push(
            config::admin_identity_root(&self.registry_path)
                .display()
                .to_string(),
        );
        let output = self.runner.output(&line).map_err(|error| match error {
            CliError::ToolMissing { .. } => CliError::ToolMissing {
                tool: "tilo-device-admin".into(),
                purpose: "join this machine and reach devices beyond this network; build it beside tilo-vm (cargo build --release -p tilo-device-admin, then copy target/release/tilo-device-admin next to tilo-vm), or name it in TILO_DEVICE_ADMIN".into(),
            },
            other => other,
        })?;
        if !output.success {
            return Err(CliError::NotEnrolledAdmin {
                detail: output.stderr.trim().to_owned(),
            });
        }
        serde_json::from_str(output.stdout.trim()).map_err(|error| CliError::CommandFailed {
            command: line.display(),
            status: format!("printed no JSON ({error})"),
            stderr: output.stderr.trim().to_owned(),
        })
    }

    pub(super) fn vm_enrol_admin(&mut self, via: Option<&str>) -> Result<Outcome, CliError> {
        let key = self.admin_tool(&["key"])?;
        let device = key.device;
        let root = config::admin_identity_root(&self.registry_path);
        if let Some(via) = via {
            self.join_via(via, &root)?;
        }
        let state = self.admin_tool(&["key"])?;
        let enrolled = state.administrator;
        let data = json!({
            "device": device,
            "identity_root": root,
            "identity": state.identity,
            "label": state.label.clone().or_else(|| Some(default_label())),
            "enrolled": enrolled,
        });
        if enrolled {
            let text = format!(
                "this machine (device {device}) is enrolled as {} in identity {}: tilo-vm reaches that identity's devices over the peer network when they are not on this network",
                state.label.as_deref().unwrap_or("?"),
                state.identity.as_deref().unwrap_or("?")
            );
            return Ok(Outcome::new(text, data).next(&[
                "tilo-vm vm status <device>   (on its LAN once: records its peer route)",
                "tilo-vm guide devices",
            ]));
        }
        let text = format!(
            "this machine's device key is {device} ({}), not one of the owner's devices yet. On a device that holds their identity, run:\n  {}\nIt prints a ticket; on this machine, run:\n  TILO_IDENTITY_ROOT={} XDG_STATE_HOME={} tilo login <ticket>",
            root.display(),
            add_by_hand(),
            root.display(),
            root.join("state").display(),
        );
        Ok(Outcome::new(text, data).next(&[
            "tilo-vm vm enrol-admin --via <device>",
            "tilo-vm guide devices",
        ]))
    }

    /// The ticket `via` prints for this machine, spent on this one: `tilo
    /// device add --capability admin` over SSH on the device that holds the
    /// owner's identity, then `tilo login` here, keeping the login with the
    /// admin identity.
    fn join_via(&mut self, via: &str, root: &Path) -> Result<(), CliError> {
        let vm = self.resolve_vm(Some(via))?;
        if vm.entry.hypervisor != DEVICE {
            return Err(CliError::InvalidInput(format!(
                "--via {via}: a VM does not hold the owner's identity; name the device where they add new devices (vm add --ssh <owner>@<address>)"
            )));
        }
        let endpoint = self.endpoint(&vm)?;
        let as_root = endpoint.user == "root";
        let mut line = remote::ssh_transport(&endpoint, vm.entry.key.as_deref(), false);
        line.args.push(add_line(as_root));
        let output = self.runner.output(&line)?;
        if !output.success {
            return Err(CliError::NotEnrolledAdmin {
                detail: format!("{via} printed no ticket: {}", output.stderr.trim()),
            });
        }
        let issued: Ticket = serde_json::from_str(output.stdout.lines().next().unwrap_or_default())
            .map_err(|_| CliError::NotEnrolledAdmin {
                detail: format!("{via} printed no ticket: {}", output.stdout.trim()),
            })?;
        let mut login = CommandLine::new("tilo", &["login", &issued.ticket]);
        login
            .env
            .push(("TILO_IDENTITY_ROOT".into(), root.display().to_string()));
        login.env.push((
            "XDG_STATE_HOME".into(),
            root.join("state").display().to_string(),
        ));
        let joined = self.runner.output(&login)?;
        if !joined.success {
            return Err(CliError::NotEnrolledAdmin {
                detail: format!("this machine did not log in: {}", joined.stderr.trim()),
            });
        }
        Ok(())
    }
}

/// What `tilo device add --json --no-wait` prints.
#[derive(serde::Deserialize)]
struct Ticket {
    ticket: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_add_line_prints_the_ticket_without_waiting() {
        let owner = add_line(false);
        assert_eq!(owner, "tilo device add --capability admin --json --no-wait");
        let root = add_line(true);
        assert!(
            root.starts_with("TILO_IDENTITY_ROOT=/var/lib/tilo XDG_STATE_HOME=/var/lib/tilo/state tilo device add"),
            "{root}"
        );
    }
}
