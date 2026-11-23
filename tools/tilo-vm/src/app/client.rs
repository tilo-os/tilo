//! Driving a VM whose daemon speaks another protocol (`vm client`). A dev disk
//! keeps the tilo-vmd it was built with, so a checkout of another main cannot
//! speak to it; the image ships the client built with that daemon, and copying
//! it out is the short way to the binary a rebuild at the image's commit would
//! produce.

use std::fmt::Write as _;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use super::{App, Outcome, insert};
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner as _};
use crate::imageup;

impl App {
    /// The client the VM's image ships speaks the protocol its daemon speaks:
    /// copy it out once and execute the arguments with it.
    #[expect(
        clippy::too_many_lines,
        reason = "one linear sequence: ask the daemon, fetch its client, write it, execute it"
    )]
    pub(super) fn vm_client(
        &mut self,
        name: Option<&str>,
        command: &[String],
    ) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let own = tilo_vm_protocol::PROTOCOL_VERSION;
        let daemon = match self.connect(&vm) {
            Ok(remote) => {
                let protocol = remote.hello.protocol;
                return Ok(Outcome::new(
                    format!(
                        "VM {}'s tilo-vmd speaks protocol {protocol}, which this tilo-vm speaks too; use tilo-vm itself",
                        vm.name
                    ),
                    json!({ "vm": vm.name, "client": null, "daemon": protocol, "protocol": own }),
                ));
            }
            Err(CliError::ProtocolMismatch { daemon, .. }) => daemon,
            Err(error) => return Err(error),
        };
        let endpoint = self.endpoint(&vm)?;
        let image = self
            .bootc_status(&vm, &endpoint)
            .ok()
            .and_then(|status| status.booted);
        let probe = self.vm_sh(
            &vm,
            &endpoint,
            "p=$(command -v tilo-vm 2>/dev/null || true); [ -n \"$p\" ] || p=/usr/libexec/tilo/tilo-vm; if [ ! -x \"$p\" ]; then echo '@@absent'; exit 0; fi; printf '@@path %s\\n' \"$(readlink -f \"$p\")\"; base64 -w0 \"$p\"",
        )?;
        if probe.stdout.contains("@@absent") {
            return Err(CliError::Unsupported {
                what: format!("a matching client from VM {}", vm.name),
                alternative: format!(
                    "this image ships no tilo-vm; move it onto a current one (tilo-vm vm upgrade {}) or use the image's own daemon with a checkout of its commit",
                    vm.name
                ),
            });
        }
        let (image_path, payload) = probe
            .stdout
            .split_once('\n')
            .filter(|(first, _)| first.starts_with("@@path "))
            .and_then(|(first, rest)| {
                first
                    .strip_prefix("@@path ")
                    .map(|path| (path.trim(), rest))
            })
            .ok_or_else(|| CliError::CommandFailed {
                command: "find the VM's tilo-vm".into(),
                status: probe.status.clone(),
                stderr: probe.stderr.trim().to_owned(),
            })?;
        let bytes = STANDARD
            .decode(payload.trim())
            .map_err(|error| CliError::CommandFailed {
                command: "read the VM's tilo-vm".into(),
                status: "corrupt".into(),
                stderr: error.to_string(),
            })?;
        let dir = self
            .registry_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("clients")
            .join(&vm.name);
        std::fs::create_dir_all(&dir).map_err(CliError::io(format!("create {}", dir.display())))?;
        let path = dir.join("tilo-vm");
        std::fs::write(&path, &bytes).map_err(CliError::io(format!("write {}", path.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(CliError::io(format!("make {} executable", path.display())))?;
        }
        let path = path.display().to_string();
        let booted_image = image.as_ref().map_or_else(
            || "an image that reports no bootc status".to_owned(),
            imageup::Deployment::describe,
        );
        let mut text = format!(
            "VM {} boots {booted_image}, whose tilo-vmd speaks protocol {daemon}; this tilo-vm speaks {own}.\n  its own tilo-vm is now at {path} (copied from {image_path})",
            vm.name
        );
        let mut data = json!({
            "vm": vm.name,
            "image": image.as_ref().map(|d| &d.image),
            "digest": image.as_ref().map(|d| &d.digest),
            "daemon": daemon,
            "protocol": own,
            "client": path,
        });
        if command.is_empty() {
            let _ = write!(
                text,
                "\n  execute what you meant with it, e.g. {path} --target {} vm status (or rerun your command as: tilo-vm vm client {} -- <arguments>)",
                vm.name, vm.name
            );
            return Ok(Outcome::new(text, data));
        }
        // The fetched client is another version: name the VM both ways it may
        // read, so it never falls back to this machine's default VM.
        let mut line =
            CommandLine::new(&path, &["--target", vm.name.as_str()]).env("TILO_VM", &vm.name);
        for argument in command {
            line = line.arg(argument.clone());
        }
        let ok = self.runner.interactive(&line)?;
        insert(&mut data, "ok", json!(ok));
        let mut outcome = Outcome::new(format!("{text}\n  ran: {}", line.display()), data);
        outcome.failed = !ok;
        Ok(outcome)
    }
}
