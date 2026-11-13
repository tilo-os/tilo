//! The normal swap story, with a single matched session cohort admission.

use super::{
    App, Builder, CliError, Instant, Outcome, Remote, Status, VM_SLOT, describe_apply, json,
};
use tilo_vm_protocol::{
    LayerVerify, Partner, Swap, USERSPACE_COMPONENTS, UserspaceApply, VM_RUNTIME_SERVICES,
};

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
        let observed = Self::cohort_state(&mut remote, slot)?;
        let expected_generation = observed.generation;
        let expected_session = observed.session.clone();
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
        self.move_runtime_services(
            &mut remote,
            &CohortMove {
                vm: &vm,
                root: &worktree.root,
                slot,
                manifest: &manifest,
                command: &command,
                inputs: &inputs,
                members: &members,
                services: &observed.services,
                current: &observed.current,
            },
        )?;
        inputs.unchanged(&worktree.root)?;
        remote = self.connect(&vm)?;
        self.apply_cohort(
            &mut remote,
            UserspaceApply {
                slot: slot.into(),
                expected_generation,
                expected_session,
                source: Box::new(inputs.provenance.clone()),
                source_inventory: inputs.digest.clone(),
                members,
            },
        )
    }

    fn cohort_state(remote: &mut Remote, slot: &str) -> Result<CohortState, CliError> {
        let status = remote.call(Status { slot: None })?;
        let current = status
            .slots
            .iter()
            .find(|info| info.name == slot)
            .ok_or_else(|| {
                CliError::InvalidInput("the requested cohort slot does not exist".into())
            })?;
        let session = current.session_identity.clone().ok_or_else(|| {
            CliError::InvalidInput("the cohort slot has no verified session identity".into())
        })?;
        let services = status
            .slots
            .iter()
            .find(|info| info.name == VM_SLOT)
            .map(|info| info.components.clone())
            .unwrap_or_default();
        Ok(CohortState {
            generation: current.generation,
            session,
            services,
            current: current.components.clone(),
        })
    }

    fn apply_cohort(
        &mut self,
        remote: &mut Remote,
        request: UserspaceApply,
    ) -> Result<Outcome, CliError> {
        let result = self.receipt.phase("apply", || remote.call(request))?;
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

    fn move_runtime_services(
        &mut self,
        remote: &mut Remote,
        plan: &CohortMove<'_>,
    ) -> Result<(), CliError> {
        let declared = plan
            .inputs
            .provenance
            .interfaces
            .get(tilo_vm_protocol::RUNTIME)
            .copied();
        let mut services = Vec::new();
        for service in VM_RUNTIME_SERVICES.iter().copied() {
            let Some(layer) = plan.services.get(service) else {
                continue;
            };
            let served = remote
                .call(LayerVerify {
                    hash: layer.clone(),
                })?
                .provenance
                .interfaces
                .get(tilo_vm_protocol::RUNTIME)
                .copied();
            if declared.is_some() && served != declared {
                services.push(service);
            }
        }
        if services.is_empty() {
            return Ok(());
        }
        let mut moved = Vec::new();
        for service in services {
            plan.inputs.unchanged(plan.root)?;
            self.check_vm_space(remote, plan.vm)?;
            let component =
                plan.manifest
                    .get(service)
                    .ok_or_else(|| CliError::UnknownComponent {
                        name: service.into(),
                        known: plan.manifest.components.keys().cloned().collect(),
                    })?;
            let start = Instant::now();
            let member = self.build_layer_remote_inventory(
                remote,
                plan.vm,
                component,
                plan.command,
                plan.inputs,
            )?;
            self.receipt
                .record(&format!("stage-{service}"), start.elapsed().as_secs_f64());
            moved.push(member);
        }
        let partner = partner_for(plan.slot, plan.current, plan.members);
        for member in &moved {
            *remote = self.connect_with_wake(plan.vm, true)?;
            let info = plan
                .manifest
                .get(member.component.as_str())
                .map(|entry| entry.info.clone());
            let phase = format!("service-{}", member.component);
            let result = self.receipt.phase(&phase, || {
                remote.call(Swap {
                    slot: VM_SLOT.into(),
                    component: member.component.clone(),
                    layer: member.layer.clone(),
                    info,
                    partner: Some(partner.clone()),
                })
            })?;
            self.receipt.set(&phase, &result);
            self.warnings.extend(result.warnings.iter().cloned());
        }
        Ok(())
    }
}

struct CohortState {
    generation: u64,
    session: tilo_vm_protocol::UserspaceSession,
    services: std::collections::BTreeMap<String, String>,
    current: std::collections::BTreeMap<String, String>,
}

struct CohortMove<'a> {
    vm: &'a super::Resolved,
    root: &'a std::path::Path,
    slot: &'a str,
    manifest: &'a tilo_vm_protocol::manifest::Manifest,
    command: &'a [String],
    inputs: &'a crate::source_inputs::Snapshot,
    members: &'a [tilo_vm_protocol::UserspaceMember],
    services: &'a std::collections::BTreeMap<String, String>,
    current: &'a std::collections::BTreeMap<String, String>,
}

fn partner_for(
    slot: &str,
    current: &std::collections::BTreeMap<String, String>,
    members: &[tilo_vm_protocol::UserspaceMember],
) -> Partner {
    let mut components = current.clone();
    for member in members {
        components.insert(member.component.clone(), member.layer.clone());
    }
    Partner {
        slot: slot.to_owned(),
        components,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn member(component: &str, layer: &str) -> tilo_vm_protocol::UserspaceMember {
        tilo_vm_protocol::UserspaceMember {
            component: component.into(),
            layer: layer.into(),
            build: tilo_vm_protocol::Provenance::local(component),
            source_inventory: "digest".into(),
        }
    }

    #[test]
    fn a_partner_generation_keeps_what_the_cohort_does_not_replace() {
        let current = [
            ("tilo-session".to_owned(), "aa".to_owned()),
            ("model-manager".to_owned(), "bb".to_owned()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let partner = partner_for(
            "device",
            &current,
            &[member("tilo-session", "cc"), member("app-host", "dd")],
        );
        assert_eq!(partner.slot, "device");
        assert_eq!(partner.components.get("tilo-session").unwrap(), "cc");
        assert_eq!(partner.components.get("app-host").unwrap(), "dd");
        assert_eq!(partner.components.get("model-manager").unwrap(), "bb");
    }
}
