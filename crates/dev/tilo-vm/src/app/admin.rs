//! Enrolling this machine as one of the owner's devices, so it may
//! administer their devices over the peer network (`vm enrol-admin`).
//!
//! The identity lives beside the registry ([`config::admin_identity_root`]):
//! a device key `tilo-device-admin key` makes, and the certificate the
//! owner's root issues for it with `tilo-enroll certify --capability admin`
//! on the device that holds the identity. That command also lists this
//! machine in the owner's fleet, so Settings shows it and can remove it.

use std::path::Path;

use base64::Engine as _;
use serde_json::json;

use super::{App, Outcome};
use crate::config;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, DEVICE, Runner as _};
use crate::remote;

/// This machine's name in the owner's device list when none is given.
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

/// The `tilo-enroll certify` line the owner's device runs for `device` over
/// SSH, printing the certificate as base64. As the identity's owner its
/// active identity record signs (in their session, which reaches its secret
/// store) and its fleet lists the new device; as root only a root key file
/// in /var/lib/tilo can sign.
pub fn certify_line(device: &str, label: &str, as_root: bool) -> String {
    let certify = if as_root {
        "tilo-enroll certify --identity-root /var/lib/tilo"
    } else {
        "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus tilo-enroll certify --identity-dir \"$HOME/.config/tilo/identity\""
    };
    format!(
        "f=$(mktemp) && {certify} --device {} --label {} --capability admin --out \"$f\" && base64 -w0 \"$f\"; s=$?; rm -f \"$f\"; exit $s",
        crate::vmbuild::sh_quote(device),
        crate::vmbuild::sh_quote(label)
    )
}

/// The same certification for the owner to run by hand.
pub fn certify_by_hand(device: &str, label: &str) -> String {
    format!(
        "tilo-enroll certify --identity-dir ~/.config/tilo/identity --device {} --label {} --capability admin --out admin.cert",
        crate::vmbuild::sh_quote(device),
        crate::vmbuild::sh_quote(label)
    )
}

/// What `tilo-device-admin key` and `install-cert` print.
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
                purpose: "enrol this machine and reach devices beyond this network; build it beside tilo-vm (cargo build --release -p tilo-device-admin, then copy target/release/tilo-device-admin next to tilo-vm), or name it in TILO_DEVICE_ADMIN".into(),
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

    pub(super) fn vm_enrol_admin(
        &mut self,
        via: Option<&str>,
        cert: Option<&Path>,
        label: Option<String>,
    ) -> Result<Outcome, CliError> {
        let key = self.admin_tool(&["key"])?;
        let device = key.device.clone();
        let label = label.unwrap_or_else(default_label);
        let root = config::admin_identity_root(&self.registry_path);
        let installed = if let Some(cert) = cert {
            Some(self.install_admin_cert(cert)?)
        } else if let Some(via) = via {
            let path = self.certify_via(via, &device, &label, &root)?;
            Some(self.install_admin_cert(&path)?)
        } else {
            None
        };
        let state = installed.unwrap_or(key);
        let enrolled = state.administrator;
        let data = json!({
            "device": device,
            "identity_root": root,
            "identity": state.identity,
            "label": state.label,
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
            "this machine's device key is {device} ({}), not enrolled yet. Enrol it from the device that holds the owner's identity:\n  tilo-vm vm enrol-admin --via <device>\nor have the owner run this there, as themselves, and hand you admin.cert:\n  {}\nthen: tilo-vm vm enrol-admin --cert admin.cert",
            root.display(),
            certify_by_hand(&device, &label),
        );
        Ok(Outcome::new(text, data).next(&[
            "tilo-vm vm enrol-admin --via <device>",
            "tilo-vm vm enrol-admin --cert <file>",
        ]))
    }

    fn install_admin_cert(&mut self, cert: &Path) -> Result<Enrolment, CliError> {
        let cert = cert.display().to_string();
        self.admin_tool(&["install-cert", "--cert", &cert])
    }

    /// Have `via` certify this machine's key over SSH; the certificate's path.
    fn certify_via(
        &mut self,
        via: &str,
        device: &str,
        label: &str,
        root: &Path,
    ) -> Result<std::path::PathBuf, CliError> {
        let vm = self.resolve_vm(Some(via))?;
        if vm.entry.hypervisor != DEVICE {
            return Err(CliError::InvalidInput(format!(
                "--via {via}: a VM does not hold the owner's identity; name the device where they approve new devices (vm add --ssh <owner>@<address>)"
            )));
        }
        let endpoint = self.endpoint(&vm)?;
        let as_root = endpoint.user == "root";
        let mut line = remote::ssh_transport(&endpoint, vm.entry.key.as_deref(), false);
        line.args.push(certify_line(device, label, as_root));
        let output = self.runner.output(&line)?;
        if !output.success {
            let said = output.stderr.trim();
            return Err(CliError::NotEnrolledAdmin {
                detail: format!(
                    "{via} did not certify this machine{}: {said}",
                    if as_root {
                        " (as root it signs only with a root key file; log in as the identity's owner instead)"
                    } else {
                        ""
                    }
                ),
            });
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(output.stdout.trim())
            .map_err(|error| CliError::NotEnrolledAdmin {
                detail: format!("{via} answered with no certificate ({error})"),
            })?;
        let path = root.join("admin.cert");
        std::fs::write(&path, bytes).map_err(CliError::io(format!("write {}", path.display())))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's account certifies with its own identity record; root only
    /// with a root key file.
    #[test]
    fn the_certify_line_signs_as_the_identitys_owner() {
        let owner = certify_line("ab12", "work (tilo-vm)", false);
        assert!(
            owner.starts_with("f=$(mktemp) && DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus tilo-enroll certify --identity-dir \"$HOME/.config/tilo/identity\""),
            "{owner}"
        );
        assert!(
            owner.contains("--device ab12 --label 'work (tilo-vm)' --capability admin"),
            "{owner}"
        );
        assert!(owner.ends_with("rm -f \"$f\"; exit $s"), "{owner}");
        let root = certify_line("ab12", "w", true);
        assert!(
            root.contains("tilo-enroll certify --identity-root /var/lib/tilo --device"),
            "{root}"
        );
        assert!(!root.contains("DBUS"), "{root}");
        let by_hand = certify_by_hand("ab12", "w");
        assert!(
            by_hand.ends_with("--capability admin --out admin.cert"),
            "{by_hand}"
        );
    }
}
