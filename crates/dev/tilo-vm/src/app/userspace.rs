//! The normal swap story, with a single matched session cohort admission.

use super::{App, Builder, CliError, Instant, Outcome, Status, VM_SLOT, describe_apply, json};
use tilo_vm_protocol::{USERSPACE_COMPONENTS, UserspaceApply};

impl App {
    pub(super) fn swap_userspace(
        &mut self,
        slot: &str,
        from: &str,
        builder: Option<Builder>,
        profile: Option<&str>,
    ) -> Result<Outcome, CliError> {
        if from != "worktree"
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
        let command = self
            .registry()?
            .remote_builders
            .get(&remote.hello.arch)
            .filter(|line| !line.is_empty())
            .cloned()
            .ok_or_else(|| self.no_remote_builder(&remote.hello.arch))?;
        let mut members = Vec::new();
        for name in USERSPACE_COMPONENTS {
            inputs.unchanged(&worktree.root)?;
            self.check_vm_space(&mut remote, &vm)?;
            let component = manifest
                .get(name)
                .ok_or_else(|| CliError::UnknownComponent {
                    name: (*name).into(),
                    known: manifest.components.keys().cloned().collect(),
                })?;
            let start = Instant::now();
            let member =
                self.build_layer_remote_inventory(&mut remote, &vm, component, &command, &inputs)?;
            self.receipt
                .record(&format!("stage-{name}"), start.elapsed().as_secs_f64());
            members.push(member);
            self.receipt.set("cohort_members", &members);
        }
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
}
