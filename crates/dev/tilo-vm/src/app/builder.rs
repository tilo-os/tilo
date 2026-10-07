//! `--builder remote`: the component builds on a remote builder of the
//! machine's architecture and its layer streams from there through this
//! host straight into tilo-vmd's upload ([`crate::builderlayer`]); no
//! binary is written to this host's disk.

use std::io::BufReader;
use std::process::{Command, Stdio};

use tilo_vm_protocol::LayerImport;
use tilo_vm_protocol::LayerSource;
use tilo_vm_protocol::manifest::Component;
use tilo_vm_protocol::{AppInstall, AppInstallResult};

use super::{App, Resolved};
use crate::build;
use crate::builderlayer::{self, Stream};
use crate::error::CliError;
use crate::hypervisor;
use crate::remote::Remote;

pub(super) fn build_header<T>(
    remote: &mut Remote,
    device: bool,
    build: impl FnOnce() -> std::io::Result<T>,
) -> Result<T, CliError> {
    let (result, renewal) = if device {
        remote.keep_awake_while(build)?
    } else {
        (build(), Ok(()))
    };
    let header = result.map_err(CliError::io("read the remote builder's header"))?;
    renewal?;
    Ok(header)
}

pub(super) fn start_builder(
    line: &hypervisor::CommandLine,
    root: &std::path::Path,
) -> Result<std::process::Child, CliError> {
    Command::new(&line.program)
        .args(&line.args)
        .envs(line.env.iter().map(|(k, v)| (k, v)))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CliError::ToolMissing {
                    tool: line.program.clone(),
                    purpose: "build on the remote builder (--builder remote)".into(),
                }
            } else {
                CliError::io("start the remote builder")(e)
            }
        })
}

impl App {
    /// Build `component` with the remote builder whose command line is
    /// `builder`, and import the layer it streams.
    pub(super) fn build_layer_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
    ) -> Result<String, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let inputs = crate::source_inputs::Snapshot::create(&worktree, &component.name)?;
        self.build_layer_remote_inventory(remote, vm, component, builder, &inputs)
            .map(|member| member.layer)
    }

    pub(super) fn build_layer_remote_inventory(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
        inputs: &crate::source_inputs::Snapshot,
    ) -> Result<tilo_vm_protocol::UserspaceMember, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata();
        let extra = build::dev_features(&metadata, &component.packages);
        let mut provenance = inputs.provenance.clone();
        provenance.component.clone_from(&component.name);
        inputs.unchanged(&worktree.root)?;
        let inventory = inputs
            .file
            .path()
            .strip_prefix(&worktree.root)
            .map_err(|error| CliError::InvalidInput(error.to_string()))?;
        let line = builderlayer::remote_command(builder, component, &extra)
            .arg("--source-inventory")
            .arg(inventory.display().to_string());
        if !self.json {
            eprintln!(
                "tilo-vm: building {} on the remote builder ({}); the layer streams to {} without touching this disk",
                component.name,
                line.display(),
                vm.name
            );
        }
        let mut child = AppBuilder(start_builder(&line, &worktree.root)?);
        let stdout = child.0.stdout.take().ok_or_else(|| {
            CliError::io("start the remote builder")(std::io::Error::other("no stdout"))
        })?;
        let mut stream = Stream::new(BufReader::with_capacity(2 << 20, stdout));
        let header = self.receipt.phase("build", || {
            build_header(remote, vm.entry.hypervisor == hypervisor::DEVICE, || {
                stream.header(&mut std::io::stderr())
            })
        });
        let packed = match header {
            Ok(packed) => packed,
            Err(error) => {
                // The owned builder may still be writing its framed layer.
                // Close the reader and reap it before returning an error.
                drop(stream);
                let _ = child.0.kill();
                let status = child
                    .0
                    .wait()
                    .map_or_else(|e| e.to_string(), |s| s.to_string());
                return Err(CliError::CommandFailed {
                    command: line.display(),
                    status,
                    stderr: format!("{error}; the build's output is above"),
                });
            }
        };
        if packed.component != component.name
            || packed.source_digest.as_deref() != Some(inputs.digest.as_str())
            || packed.target.as_deref()
                != Some(format!("{}-unknown-linux-gnu", remote.hello.arch).as_str())
        {
            drop(stream);
            let _ = child.0.kill();
            let _ = child.0.wait();
            return Err(CliError::InvalidInput(
                "remote component source or architecture does not match the requested build".into(),
            ));
        }
        // The build can outlast an idle connection: a fresh one for the upload.
        *remote = self.connect(vm)?;
        let upload = self
            .receipt
            .phase("transfer", || remote.upload(&mut stream));
        drop(stream);
        let upload = upload?;
        let status = child
            .0
            .wait()
            .map_err(CliError::io("wait for the remote builder"))?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: line.display(),
                status: status.to_string(),
                stderr: "the remote builder failed after sending the layer; its output is above"
                    .into(),
            });
        }
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features = component.build_features(&extra);
        provenance.target = packed.target;
        provenance.toolchain = packed.toolchain;
        inputs.unchanged(&worktree.root)?;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(provenance.clone()),
            })
        })?;
        Ok(tilo_vm_protocol::UserspaceMember {
            component: component.name.clone(),
            layer: imported.hash,
            build: provenance,
            source_inventory: inputs.digest.clone(),
        })
    }

    /// Build a native app on the configured architecture's builder, then use
    /// the existing slot-user installer; runtime and publisher checks remain there.
    pub(super) fn install_app_from_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        slot: &str,
        dir: &str,
        builder: &[String],
    ) -> Result<AppInstallResult, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let provenance = build::source_provenance(&worktree, &format!("app:{dir}"), "remote");
        let inputs = crate::source_inputs::Snapshot::create(&worktree, &format!("app:{dir}"))?;
        let inventory = inputs
            .file
            .path()
            .strip_prefix(&worktree.root)
            .map_err(|e| CliError::InvalidInput(e.to_string()))?;
        let line = crate::builderapp::remote_command(builder, dir, inventory)?;
        let arch = remote.hello.arch.clone();
        self.receipt.set("builder", "remote");
        self.receipt.set("app_source", provenance);
        let mut child = AppBuilder(start_builder(&line, &worktree.root)?);
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or_else(|| CliError::InvalidInput("remote app builder has no stdout".into()))?;
        let mut stream = Stream::new(BufReader::with_capacity(2 << 20, stdout));
        let packed: crate::builderapp::PackedApp = self.receipt.phase("build", || {
            build_header(remote, vm.entry.hypervisor == hypervisor::DEVICE, || {
                stream.header_as(&mut std::io::stderr())
            })
        })?;
        packed.validate(dir, &arch, &inputs.digest)?;
        self.receipt.set("app_package", &packed);
        *remote = self.connect(vm)?;
        let upload = self
            .receipt
            .phase("transfer", || remote.upload(&mut stream));
        // Drop the pipe before waiting: a refused upload must not deadlock a
        // builder still blocked writing its remaining package bytes.
        drop(stream);
        let upload = upload?;
        let status = child
            .0
            .wait()
            .map_err(CliError::io("wait for remote app builder"))?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: line.display(),
                status: status.to_string(),
                stderr: "remote app builder failed; no app was installed".into(),
            });
        }
        self.receipt.phase("install", || {
            remote.call(AppInstall {
                slot: slot.into(),
                source: LayerSource::Upload(upload),
            })
        })
    }

    /// No remote builder serves `arch`: the line that adds one.
    pub(super) fn no_remote_builder(&self, arch: &str) -> CliError {
        CliError::Config {
            path: self.registry_path.display().to_string(),
            detail: format!(
                "no remote builder for {arch} is configured; add `[remote_builders]` with `{arch} = [\"<a cargo drop-in whose run runs on an {arch} machine>\", ...]` (tilo-vm guide swapping)"
            ),
        }
    }
}

/// Every failure path terminates and reaps only its owned builder process.
pub(super) struct AppBuilder(pub(super) std::process::Child);

impl Drop for AppBuilder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn remote_app_owned_builder_is_reaped_when_delivery_fails() {
        let child = Command::new("sleep")
            .arg("600")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let process = std::path::PathBuf::from(format!("/proc/{}", child.id()));
        assert!(process.try_exists().unwrap());
        let guard = AppBuilder(child);
        drop(guard);
        assert!(
            !process.try_exists().unwrap(),
            "owned builder was not reaped"
        );
    }
}
