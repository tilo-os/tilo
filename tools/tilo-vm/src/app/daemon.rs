//! Replacing the VM's tilo-vmd in `tilo-vm swap vmd`: the handoff to the daemon
//! of the imported layer, and the daemon this tilo-vm unpacks over SSH when the
//! VM's own speaks another protocol.

use tilo_vm_protocol::LayerList;
use tilo_vm_protocol::manifest::Component;

use super::{App, Resolved, daemon_import_receipt};
use crate::error::CliError;
use crate::hypervisor::CommandLine;
use crate::remote::{self, Remote};
use crate::vmbuild;

impl App {
    pub(super) fn handoff_daemon(
        &mut self,
        vm: &Resolved,
        remote: Remote,
        layer: &str,
    ) -> Result<Remote, CliError> {
        let mut remote = remote;
        let layers = remote.call(LayerList {})?;
        let provenance = daemon_import_receipt(&layers, layer, &remote.hello.arch)?;
        self.receipt.set("daemon_import", provenance);
        let command =
            remote::imported_daemon_command(vm.entry.daemon.as_deref(), &remote.hello.root, layer)?;
        let command: Vec<String> = command.iter().map(|word| vmbuild::sh_quote(word)).collect();
        let endpoint = self.endpoint(vm)?;
        let line = remote::ssh_command(&endpoint, vm.entry.key.as_deref(), &command, false);
        let adapter = Self::adapter(vm)?;
        let debug = Self::debug_commands(adapter.as_ref(), &vm.name);
        let mut imported = self.receipt.phase("daemon-handoff", || {
            remote.handoff(&vm.name, &line, &endpoint, debug)
        })?;
        let verified = imported.call(tilo_vm_protocol::LayerVerify { hash: layer.into() })?;
        if verified.hash != layer {
            return Err(CliError::InvalidInput(
                "replacement daemon refused imported layer integrity".into(),
            ));
        }
        // End the old RPC only after the replacement handshake succeeded.
        drop(remote);
        self.receipt.set("applying_daemon_layer", layer);
        Ok(imported)
    }

    /// Starts this tilo-vm's own tilo-vmd on a VM whose daemon speaks another
    /// protocol, for the rest of a `swap vmd`: no upload through that daemon
    /// reaches it, so the layer is built for the VM and unpacked over plain
    /// SSH, which needs no protocol. Returns the command that starts it; the
    /// swap then imports the layer through it and applies it as it always does.
    pub(super) fn bridge_daemon(
        &mut self,
        vm: &Resolved,
        component: &Component,
    ) -> Result<String, CliError> {
        let endpoint = self.endpoint(vm)?;
        let arch = if let Some(arch) = vm.entry.arch.clone() {
            arch
        } else {
            let output = self.vm_sh(vm, &endpoint, "uname -m")?;
            let arch = output.stdout.trim().to_owned();
            if !output.success || arch.is_empty() {
                return Err(CliError::CommandFailed {
                    command: "uname -m".into(),
                    status: "no architecture".into(),
                    stderr: output.stderr,
                });
            }
            arch
        };
        let builder = match self.registry()?.remote_builders.get(&arch) {
            Some(line) if !line.is_empty() => line.clone(),
            _ if arch == std::env::consts::ARCH => Vec::new(),
            _ => return Err(self.no_remote_builder(&arch)),
        };
        let configured = vm.entry.daemon.clone();
        let root = remote::configured_root(configured.as_deref());
        let sink = remote::ssh_command(
            &endpoint,
            vm.entry.key.as_deref(),
            &[remote::bridge_unpack_line(&root)],
            false,
        );
        if !self.json {
            eprintln!(
                "tilo-vm: {}'s tilo-vmd speaks another protocol; replacing it with this tilo-vm's own",
                vm.name
            );
        }
        self.pack_layer_over_ssh(component, &builder, &arch, &sink)?;
        self.receipt
            .set("daemon_bridge", format!("{root}/{}", remote::BRIDGE_DIR));
        Ok(remote::bridge_daemon_command(configured.as_deref(), &root)?.join(" "))
    }

    /// Removes the daemon [`Self::bridge_daemon`] unpacked once the imported
    /// layer's own daemon has taken over.
    pub(super) fn remove_bridge(&mut self, vm: &Resolved) {
        let dir = format!(
            "{}/{}",
            remote::configured_root(vm.entry.daemon.as_deref()),
            remote::BRIDGE_DIR
        );
        let removed = self.endpoint(vm).and_then(|endpoint| {
            self.vm_sh(
                vm,
                &endpoint,
                &format!("rm -rf {}", vmbuild::sh_quote(&dir)),
            )
        });
        if !removed.is_ok_and(|output| output.success) {
            self.warnings.push(format!(
                "the tilo-vmd unpacked for this swap stays at {dir} on {}; remove it with tilo-vm vm ssh {} --raw -- rm -rf {dir}",
                vm.name, vm.name
            ));
        }
    }
}

/// The local counterpart of [`App::handoff_daemon`]: the imported daemon of
/// `layer` takes over through this machine's own process, with no SSH, before
/// the swap it applies.
pub(super) fn handoff_local(remote: &Remote, layer: &str) -> Result<Remote, CliError> {
    let command = remote::imported_daemon_command(None, &remote.hello.root, layer)?;
    let Some((program, rest)) = command.split_first() else {
        return Err(CliError::InvalidInput(
            "imported daemon command is empty".into(),
        ));
    };
    let args: Vec<&str> = rest.iter().map(String::as_str).collect();
    let line = CommandLine::new(program, &args);
    let mut imported = remote.handoff_here(&line)?;
    let verified = imported.call(tilo_vm_protocol::LayerVerify { hash: layer.into() })?;
    if verified.hash != layer {
        return Err(CliError::InvalidInput(
            "replacement daemon refused imported layer integrity".into(),
        ));
    }
    Ok(imported)
}
