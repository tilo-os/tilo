//! The normal swap story, with a single matched session cohort admission.

use super::{App, Builder, CliError, Outcome, Status, VM_SLOT, describe_apply, json};
use tilo_vm_protocol::{Manifest, UserspaceApply, UserspaceMember};

use super::userspace_prepare::store::Loaded;
use super::{Remote, Resolved};
use crate::source_inputs::Snapshot;

impl App {
    pub(super) fn swap_userspace(
        &mut self,
        slot: &str,
        from: &str,
        builder: Option<Builder>,
        profile: Option<&str>,
    ) -> Result<Outcome, CliError> {
        let prepared_path = from
            .strip_prefix("prepared:")
            .filter(|path| !path.is_empty());
        if (from != "worktree" && prepared_path.is_none())
            || profile.is_some()
            || matches!(builder, Some(Builder::Host | Builder::Vm))
            || slot == VM_SLOT
        {
            return Err(CliError::Unsupported {
                what: "a userspace cohort outside its canonical remote worktree profiles".into(),
                alternative: "tilo-vm swap --slot <session> userspace --builder remote".into(),
            });
        }
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let inputs = crate::source_inputs::Snapshot::create(&worktree, "userspace")?;
        self.receipt.set("cohort_source", &inputs.provenance);
        self.receipt.set("source_inventory", &inputs.digest);
        let manifest = self.manifest()?.ok_or_else(|| CliError::NoManifest {
            path: worktree
                .root
                .join("os/components.toml")
                .display()
                .to_string(),
        })?;
        // Verify the whole retained payload before even contacting the target.
        let prepared = prepared_path
            .map(|path| {
                self.receipt.phase("verify-prepared-userspace", || {
                    super::userspace_prepare::store::load(
                        std::path::Path::new(path),
                        &manifest,
                        &inputs,
                        &worktree.root,
                    )
                })
            })
            .transpose()?;
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect(&vm)?;
        if !remote
            .hello
            .capabilities
            .iter()
            .any(|capability| capability == "userspace.apply")
        {
            return Err(CliError::Unsupported {
                what: "this daemon has no staged userspace cohort admission".into(),
                alternative: "update the maintained dev-vm-daemon component first".into(),
            });
        }
        let observed = remote
            .call(Status {
                slot: Some(slot.into()),
            })?
            .slots
            .into_iter()
            .find(|info| info.name == slot)
            .ok_or_else(|| {
                CliError::InvalidInput("the requested cohort slot does not exist".into())
            })?;
        let expected_generation = observed.generation;
        let expected_session = observed.session_identity.ok_or_else(|| {
            CliError::InvalidInput("the cohort slot has no verified session identity".into())
        })?;
        if prepared.is_some() {
            self.receipt.set("prepared_path", prepared_path);
        }
        let members = self.stage_userspace(&vm, &mut remote, &manifest, &inputs, prepared)?;
        inputs.unchanged(&worktree.root)?;
        remote = self.connect(&vm)?;
        let result = self.receipt.phase("apply", || {
            remote.call(UserspaceApply {
                slot: slot.into(),
                expected_generation,
                expected_session,
                source: Box::new(inputs.provenance.clone()),
                source_inventory: inputs.digest.clone(),
                members,
            })
        })?;
        self.receipt.set("cohort_activation", &result);
        self.warnings.extend(result.apply.warnings.iter().cloned());
        if let Some(failure) = &result.failure {
            return Err(CliError::InvalidInput(format!(
                "userspace cohort partially selected; generation {} retains its SDK closure: {failure}",
                result.apply.generation
            )));
        }
        Ok(
            Outcome::new(describe_apply(&result.apply), json!(result)).next(&[
                "tilo-vm status",
                "tilo-vm control state-dump --pretty",
                "tilo-vm logs",
            ]),
        )
    }

    fn stage_userspace(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        manifest: &Manifest,
        inputs: &Snapshot,
        prepared: Option<Loaded>,
    ) -> Result<Vec<UserspaceMember>, CliError> {
        if let Some(prepared) = prepared {
            if prepared.prepared.arch != remote.hello.arch {
                return Err(CliError::InvalidInput(
                    "prepared userspace architecture differs from the target".into(),
                ));
            }
            self.receipt.set("cohort_builder_admissions", 0);
            let floor = self.registry()?.min_free_bytes();
            let budget = super::userspace_builder::space::Budget::new(
                prepared.members.iter().map(|(_, archive)| archive),
                floor,
            )?;
            let storage = remote.call(Status { slot: None })?.storage;
            self.receipt.set("cohort_space_budget", &budget);
            budget.admit(storage.free_bytes)?;
            self.import_userspace_members(
                remote,
                vm,
                manifest,
                inputs,
                &prepared.prepared.metadata,
                prepared.members,
            )
        } else {
            let command = self
                .registry()?
                .remote_builders
                .get(&remote.hello.arch)
                .filter(|line| !line.is_empty())
                .cloned()
                .ok_or_else(|| self.no_remote_builder(&remote.hello.arch))?;
            self.build_userspace_remote(remote, vm, manifest, &command, inputs)
        }
    }
}
