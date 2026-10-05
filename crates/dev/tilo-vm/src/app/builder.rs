//! `--builder remote`: the component builds on a remote builder of the
//! machine's architecture and its layer streams from there through this
//! host straight into tilo-vmd's upload ([`crate::builderlayer`]); no
//! binary is written to this host's disk.

use std::io::BufReader;
use std::process::{Command, Stdio};

use tilo_vm_protocol::LayerImport;
use tilo_vm_protocol::LayerSource;
use tilo_vm_protocol::manifest::Component;

use super::{App, Resolved};
use crate::build;
use crate::builderlayer::{self, Stream};
use crate::error::CliError;
use crate::remote::Remote;

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
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata();
        let extra = build::dev_features(&metadata, &component.packages);
        let line = builderlayer::remote_command(builder, component, &extra);
        if !self.json {
            eprintln!(
                "tilo-vm: building {} on the remote builder ({}); the layer streams to {} without touching this disk",
                component.name,
                line.display(),
                vm.name
            );
        }
        let mut child = Command::new(&line.program)
            .args(&line.args)
            .envs(line.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&worktree.root)
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
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CliError::io("start the remote builder")(std::io::Error::other("no stdout"))
        })?;
        let mut stream = Stream::new(BufReader::with_capacity(2 << 20, stdout));
        let header = self
            .receipt
            .phase("build", || stream.header(&mut std::io::stderr()));
        let packed = match header {
            Ok(packed) => packed,
            Err(error) => {
                let status = child
                    .wait()
                    .map_or_else(|e| e.to_string(), |s| s.to_string());
                return Err(CliError::CommandFailed {
                    command: line.display(),
                    status,
                    stderr: format!("{error}; the build's output is above"),
                });
            }
        };
        // The build can outlast an idle connection: a fresh one for the upload.
        *remote = self.connect(vm)?;
        let upload = self
            .receipt
            .phase("transfer", || remote.upload(&mut stream));
        let status = child
            .wait()
            .map_err(CliError::io("wait for the remote builder"))?;
        let upload = upload?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: line.display(),
                status: status.to_string(),
                stderr: "the remote builder failed after sending the layer; its output is above"
                    .into(),
            });
        }
        let mut provenance = build::source_provenance(&worktree, &component.name, "remote");
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features = component.build_features(&extra);
        provenance.target = packed.target;
        provenance.toolchain = packed.toolchain;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(provenance),
            })
        })?;
        Ok(imported.hash)
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
