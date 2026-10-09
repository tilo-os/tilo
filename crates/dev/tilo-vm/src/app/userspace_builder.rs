//! One remote admission, still independently verified and imported layers.

use std::io::BufReader;

pub(super) mod space;
use std::time::Instant;

use tilo_vm_protocol::manifest::Component;
use tilo_vm_protocol::{LayerImport, LayerSource, Manifest, USERSPACE_COMPONENTS, UserspaceMember};

use super::builder::{AppBuilder, build_header, start_builder};
use super::{App, Resolved};
use crate::builderlayer::{self, Packed, Stream};
use crate::error::CliError;
use crate::remote::Remote;
use crate::source_inputs::Snapshot;

impl App {
    pub(super) fn build_userspace_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        manifest: &Manifest,
        builder: &[String],
        inputs: &Snapshot,
    ) -> Result<Vec<UserspaceMember>, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata_started = Instant::now();
        let metadata = self.cargo_metadata();
        self.receipt
            .record("cohort-metadata", metadata_started.elapsed().as_secs_f64());
        inputs.unchanged(&worktree.root)?;
        self.check_vm_space(remote, vm)?;
        let carrier = inputs.carrier(&worktree.root)?;
        let inventory = carrier
            .path()
            .strip_prefix(&worktree.root)
            .map_err(|error| CliError::InvalidInput(error.to_string()))?;
        let line = builderlayer::userspace_command(builder, inventory);
        self.receipt.set("cohort_builder_admissions", 1);
        if !self.json {
            eprintln!(
                "tilo-vm: building the matched userspace cohort in one remote admission ({})",
                line.display()
            );
        }
        let mut child = AppBuilder(start_builder(&line, &worktree.root)?);
        let stdout =
            child.0.stdout.take().ok_or_else(|| {
                CliError::InvalidInput("remote cohort builder has no stdout".into())
            })?;
        let mut stream = Stream::new(BufReader::with_capacity(2 << 20, stdout));
        let spool_parent = worktree.root.join("target/tilo-vm");
        std::fs::create_dir_all(&spool_parent)
            .map_err(CliError::io("create cohort spool parent"))?;
        let spool = tempfile::Builder::new()
            .prefix("cohort-")
            .tempdir_in(&spool_parent)
            .map_err(CliError::io("create private cohort spool"))?;
        let mut staged = Vec::new();
        for name in USERSPACE_COMPONENTS {
            inputs.unchanged(&worktree.root)?;
            let component = manifest
                .get(name)
                .ok_or_else(|| CliError::UnknownComponent {
                    name: (*name).into(),
                    known: manifest.components.keys().cloned().collect(),
                })?;
            if !staged.is_empty() {
                stream
                    .next_layer()
                    .map_err(CliError::io("advance the cohort stream"))?;
            }
            let packed = self.receipt.phase(&format!("build-{name}"), || {
                build_header(
                    remote,
                    vm.entry.hypervisor == crate::hypervisor::DEVICE,
                    || stream.header(&mut std::io::stderr()),
                )
            })?;
            validate_member(&packed, component, &inputs.digest, &remote.hello.arch)?;
            let archive = space::Archive::receive_tracked(
                &mut stream,
                spool.path(),
                &mut self.receipt,
                name,
            )?;
            staged.push((packed, archive));
        }
        self.receipt.phase("cohort-stream-finish", || {
            stream
                .finish(&mut std::io::stderr())
                .map_err(CliError::io("finish the cohort stream"))
        })?;
        drop(stream);
        let status = self.receipt.phase("cohort-builder-exit", || {
            child
                .0
                .wait()
                .map_err(CliError::io("wait for the remote cohort builder"))
        })?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: line.display(),
                status: status.to_string(),
                stderr: "the remote cohort builder failed; no cohort was transferred".into(),
            });
        }
        inputs.unchanged(&worktree.root)?;
        let floor = self.registry()?.min_free_bytes();
        let budget = space::Budget::new(staged.iter().map(|(_, archive)| archive), floor)?;
        *remote = self.connect(vm)?;
        let storage = remote.call(super::Status { slot: None })?.storage;
        self.receipt.set("cohort_space_budget", &budget);
        budget.admit(storage.free_bytes)?;
        self.import_userspace_members(remote, vm, manifest, inputs, &metadata, staged)
    }

    pub(super) fn import_userspace_members(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        manifest: &Manifest,
        inputs: &Snapshot,
        metadata: &serde_json::Value,
        staged: Vec<(Packed, space::Archive)>,
    ) -> Result<Vec<UserspaceMember>, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let mut members = Vec::new();
        for (packed, mut archive) in staged {
            self.check_vm_space(remote, vm)?;
            inputs.unchanged(&worktree.root)?;
            let component = manifest.get(&packed.component).ok_or_else(|| {
                CliError::InvalidInput("prepared cohort component disappeared".into())
            })?;
            let start = Instant::now();
            let upload = self
                .receipt
                .phase(&format!("transfer-{}", component.name), || {
                    remote.upload(&mut archive.file)
                })?;
            inputs.unchanged(&worktree.root)?;
            let mut provenance = inputs.provenance.clone();
            provenance.component.clone_from(&component.name);
            provenance.cargo_profile = Some(component.profile.clone());
            let extra = crate::build::dev_features(metadata, &component.packages);
            provenance.features = component.build_features(&extra);
            provenance.target = packed.target;
            provenance.toolchain = packed.toolchain;
            let imported = self
                .receipt
                .phase(&format!("import-{}", component.name), || {
                    remote.call(LayerImport {
                        source: LayerSource::Upload(upload),
                        provenance: Box::new(provenance.clone()),
                    })
                })?;
            if packed
                .layer_hash
                .as_ref()
                .is_some_and(|expected| *expected != imported.hash)
            {
                return Err(CliError::InvalidInput(
                    "imported userspace member differs from its prepared content identity".into(),
                ));
            }
            members.push(UserspaceMember {
                component: component.name.clone(),
                layer: imported.hash,
                build: provenance,
                source_inventory: inputs.digest.clone(),
            });
            self.receipt.record(
                &format!("stage-{}", component.name),
                start.elapsed().as_secs_f64(),
            );
            self.receipt.set("cohort_members", &members);
        }
        inputs.unchanged(&worktree.root)?;
        Ok(members)
    }
}

pub(super) fn validate_member(
    packed: &Packed,
    component: &Component,
    digest: &str,
    arch: &str,
) -> Result<(), CliError> {
    if packed.component != component.name
        || packed.source_digest.as_deref() != Some(digest)
        || packed.target.as_deref() != Some(format!("{arch}-unknown-linux-gnu").as_str())
    {
        return Err(CliError::InvalidInput(
            "remote cohort member source, order or architecture does not match the requested build"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userspace_batch_member_admission_rejects_wrong_order_source_and_architecture() {
        let component = Component {
            name: "shell".into(),
            boot: false,
            scenario: None,
            dev_build: None,
            info: tilo_vm_protocol::manifest::ComponentInfo::new(
                tilo_vm_protocol::manifest::Scope::Session,
                vec![],
            ),
            packages: vec![],
            bins: vec![],
            features: vec![],
            dev_features: vec![],
            profile: "release".into(),
            paths: vec![],
            image_assembled: false,
        };
        let packed = Packed {
            component: "shell".into(),
            binary: false,
            target: Some("aarch64-unknown-linux-gnu".into()),
            source_digest: Some("verified inventory".into()),
            toolchain: Some("actual compiler".into()),
            layer_hash: None,
            reused: false,
        };
        validate_member(&packed, &component, "verified inventory", "aarch64").unwrap();
        for altered in [
            Packed {
                component: "ush".into(),
                ..packed.clone()
            },
            Packed {
                source_digest: None,
                ..packed.clone()
            },
            Packed {
                source_digest: Some("different source".into()),
                ..packed.clone()
            },
            Packed {
                target: Some("x86_64-unknown-linux-gnu".into()),
                ..packed
            },
        ] {
            assert!(
                validate_member(&altered, &component, "verified inventory", "aarch64").is_err()
            );
        }
    }
}
