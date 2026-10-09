//! Offline preparation uses the normal producer and validates its whole stream.

use std::io::BufRead;

pub(super) mod store;

use tilo_vm_protocol::{Manifest, USERSPACE_COMPONENTS};

use super::builder::{AppBuilder, start_builder};
use super::userspace_builder::validate_member;
use super::{App, CliError, Outcome, json};
use crate::builderlayer::{Packed, Stream};
use crate::source_inputs::Snapshot;

impl App {
    pub(super) fn prepare_userspace(&mut self, arch: &str) -> Result<Outcome, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let inputs = Snapshot::create(&worktree, "userspace")?;
        self.receipt.set("cohort_source", &inputs.provenance);
        self.receipt.set("source_inventory", &inputs.digest);
        let manifest = self.manifest()?.ok_or_else(|| CliError::NoManifest {
            path: worktree
                .root
                .join("os/components.toml")
                .display()
                .to_string(),
        })?;
        let command = self
            .registry()?
            .remote_builders
            .get(arch)
            .filter(|line| !line.is_empty())
            .cloned()
            .ok_or_else(|| self.no_remote_builder(arch))?;
        let carrier = inputs.carrier(&worktree.root)?;
        let inventory = carrier
            .path()
            .strip_prefix(&worktree.root)
            .map_err(|error| CliError::InvalidInput(error.to_string()))?;
        let line = crate::builderlayer::userspace_command(&command, inventory);
        self.receipt.set("cohort_builder_admissions", 1);
        let mut child = AppBuilder(start_builder(&line, &worktree.root)?);
        let stdout =
            child.0.stdout.take().ok_or_else(|| {
                CliError::InvalidInput("remote cohort builder has no stdout".into())
            })?;
        let mut capture = store::Capture::new(&worktree.root)?;
        let members = self.receipt.phase("prepare-userspace", || {
            capture.receive(stdout, &manifest, &inputs, arch)
        })?;
        let status = child
            .0
            .wait()
            .map_err(CliError::io("wait for the remote cohort builder"))?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: line.display(),
                status: status.to_string(),
                stderr: "the remote cohort builder failed; no userspace was prepared".into(),
            });
        }
        inputs.unchanged(&worktree.root)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata();
        if !metadata
            .get("packages")
            .is_some_and(serde_json::Value::is_array)
        {
            return Err(CliError::InvalidInput(
                "cannot record prepared userspace build metadata".into(),
            ));
        }
        inputs.unchanged(&worktree.root)?;
        self.receipt.set("prepared_cohort", &members);
        let prepared_path = capture.publish(&inputs, arch, members.clone(), metadata)?;
        self.receipt.set("prepared_path", &prepared_path);
        self.receipt.set("device_contacted", false);
        let next = crate::hypervisor::CommandLine::new(
            "tilo-vm",
            &["swap", "userspace", "--slot", "device", "--from"],
        )
        .arg(format!("prepared:{}", prepared_path.display()))
        .display();
        Ok(Outcome::new(
            format!(
                "Prepared {} userspace members for {arch}; the device is unchanged",
                members.len()
            ),
            json!({"prepared": true, "arch": arch, "source": inputs.provenance,
                "source_inventory": inputs.digest, "members": members,
                "prepared_path": prepared_path,
                "device_contacted": false, "activated": false}),
        )
        .next(&[&next]))
    }
}

#[cfg(test)]
pub(crate) fn verify_prepared_stream<R: BufRead>(
    stream: &mut Stream<R>,
    manifest: &Manifest,
    digest: &str,
    arch: &str,
) -> Result<Vec<Packed>, CliError> {
    let directory =
        tempfile::tempdir().map_err(CliError::io("create preparation verification spool"))?;
    Ok(
        stage_prepared_stream(stream, manifest, digest, arch, directory.path())?
            .into_iter()
            .map(|(packed, _)| packed)
            .collect(),
    )
}

fn stage_prepared_stream<R: BufRead>(
    stream: &mut Stream<R>,
    manifest: &Manifest,
    digest: &str,
    arch: &str,
    directory: &std::path::Path,
) -> Result<Vec<(Packed, super::userspace_builder::space::Archive)>, CliError> {
    let mut members = Vec::new();
    for name in USERSPACE_COMPONENTS {
        let component = manifest
            .get(name)
            .ok_or_else(|| CliError::UnknownComponent {
                name: (*name).into(),
                known: manifest.components.keys().cloned().collect(),
            })?;
        if !members.is_empty() {
            stream
                .next_layer()
                .map_err(CliError::io("advance the prepared cohort stream"))?;
        }
        let packed = stream
            .header(&mut std::io::stderr())
            .map_err(CliError::io("read the prepared cohort header"))?;
        validate_member(&packed, component, digest, arch)?;
        if packed.reused {
            return Err(CliError::InvalidInput(
                "prepared userspace requires every member's complete payload".into(),
            ));
        }
        let archive = super::userspace_builder::space::Archive::receive(stream, directory)?;
        members.push((packed, archive));
    }
    stream
        .finish(&mut std::io::stderr())
        .map_err(CliError::io("finish the prepared cohort stream"))?;
    Ok(members)
}
