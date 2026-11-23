//! The normal swap story, with a single matched session cohort admission.

use super::{
    App, Builder, CliError, Instant, Outcome, Remote, Status, VM_ACCOUNT, describe_apply, json,
};
use tilo_device_protocol::{
    LayerVerify, Partner, Swap, USERSPACE_COMPONENTS, UserspaceApply, VM_RUNTIME_SERVICES,
};

impl App {
    pub(super) fn swap_userspace(
        &mut self,
        account: &str,
        from: &str,
        builder: Option<Builder>,
        profile: Option<&str>,
    ) -> Result<Outcome, CliError> {
        if from != "worktree"
            || profile.is_some()
            || matches!(builder, Some(Builder::Host | Builder::Vm))
            || account == VM_ACCOUNT
        {
            return Err(CliError::Unsupported {
                what: "a userspace cohort outside its canonical remote worktree profiles".into(),
                alternative: "tilo-vm swap --account <session> userspace --builder remote".into(),
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
        let observed = Self::cohort_state(&mut remote, account)?;
        let expected_generation = observed.generation;
        let expected_session = observed.session.clone();
        let command = self
            .registry()?
            .remote_builders
            .get(&remote.status.arch)
            .filter(|line| !line.is_empty())
            .cloned()
            .ok_or_else(|| self.no_remote_builder(&remote.status.arch))?;
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
                account,
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
                account: account.into(),
                expected_generation,
                expected_session,
                source: Box::new(inputs.provenance.clone()),
                source_inventory: inputs.digest.clone(),
                members,
            },
        )
    }

    fn cohort_state(remote: &mut Remote, account: &str) -> Result<CohortState, CliError> {
        let status = remote.call(Status { account: None })?;
        let current = status
            .accounts
            .iter()
            .find(|info| info.name == account)
            .ok_or_else(|| {
                CliError::InvalidInput("the requested cohort account does not exist".into())
            })?;
        let session = current.session_identity.clone().ok_or_else(|| {
            CliError::InvalidInput("the cohort account has no verified session identity".into())
        })?;
        let services = status
            .accounts
            .iter()
            .find(|info| info.name == VM_ACCOUNT)
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
            .get(tilo_device_protocol::RUNTIME)
            .copied();
        // A service the VM account never swapped is the image's own, built
        // for the image's runtime version: the daemon refuses the cohort
        // while it differs from the tree's, so it moves with the cohort too.
        let mut image_runtime = None;
        let mut served = std::collections::BTreeMap::new();
        for service in VM_RUNTIME_SERVICES.iter().copied() {
            let version = if let Some(layer) = plan.services.get(service) {
                remote
                    .call(LayerVerify {
                        hash: layer.clone(),
                    })?
                    .provenance
                    .interfaces
                    .get(tilo_device_protocol::RUNTIME)
                    .copied()
            } else {
                if image_runtime.is_none() {
                    image_runtime = Some(self.image_runtime(plan.vm));
                }
                image_runtime.flatten()
            };
            served.insert(service, version);
        }
        let services = services_to_move(declared, &served);
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
        let partner = partner_for(plan.account, plan.current, plan.members);
        for member in &moved {
            *remote = self.connect_with_wake(plan.vm, true)?;
            let info = plan
                .manifest
                .get(member.component.as_str())
                .map(|entry| entry.info.clone());
            let phase = format!("service-{}", member.component);
            let result = self.receipt.phase(&phase, || {
                remote.call(Swap {
                    account: VM_ACCOUNT.into(),
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

impl App {
    /// The runtime interface version the VM's image serves, read from its
    /// `system-interfaces.toml`; `None` when it cannot be read.
    fn image_runtime(&mut self, vm: &super::Resolved) -> Option<u32> {
        let mut remote = self.connect(vm).ok()?;
        let text = super::reach::device_sh(
            &mut remote,
            &format!("cat {}", tilo_device_protocol::SYSTEM_INTERFACES),
        )
        .ok()?;
        tilo_device_protocol::system_interfaces(&text)
            .get(tilo_device_protocol::RUNTIME)
            .copied()
    }
}

/// The VM runtime services to rebuild from the cohort's tree: each that
/// serves another runtime version than the tree declares, or whose version
/// is unknown (rebuilding it from the same tree is what the daemon asks).
fn services_to_move<'a>(
    declared: Option<u32>,
    served: &std::collections::BTreeMap<&'a str, Option<u32>>,
) -> Vec<&'a str> {
    let Some(declared) = declared else {
        return Vec::new();
    };
    served
        .iter()
        .filter(|(_, version)| **version != Some(declared))
        .map(|(service, _)| *service)
        .collect()
}

struct CohortState {
    generation: u64,
    session: tilo_device_protocol::UserspaceSession,
    services: std::collections::BTreeMap<String, String>,
    current: std::collections::BTreeMap<String, String>,
}

struct CohortMove<'a> {
    vm: &'a super::Resolved,
    root: &'a std::path::Path,
    account: &'a str,
    manifest: &'a tilo_device_protocol::manifest::Manifest,
    command: &'a [String],
    inputs: &'a crate::source_inputs::Snapshot,
    members: &'a [tilo_device_protocol::UserspaceMember],
    services: &'a std::collections::BTreeMap<String, String>,
    current: &'a std::collections::BTreeMap<String, String>,
}

fn partner_for(
    account: &str,
    current: &std::collections::BTreeMap<String, String>,
    members: &[tilo_device_protocol::UserspaceMember],
) -> Partner {
    let mut components = current.clone();
    for member in members {
        components.insert(member.component.clone(), member.layer.clone());
    }
    Partner {
        account: account.to_owned(),
        components,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn member(component: &str, layer: &str) -> tilo_device_protocol::UserspaceMember {
        tilo_device_protocol::UserspaceMember {
            component: component.into(),
            layer: layer.into(),
            build: tilo_device_protocol::Provenance::local(component),
            source_inventory: "digest".into(),
        }
    }

    #[test]
    fn the_images_own_runtime_services_move_with_a_cohort_built_for_another_runtime() {
        let served = |fs: Option<u32>| [("fs", fs)].into_iter().collect::<BTreeMap<_, _>>();
        // The image's fs built for runtime 31, the tree declares 32: the real
        // refusal a cohort met when only swapped services were compared.
        assert_eq!(services_to_move(Some(32), &served(Some(31))), vec!["fs"]);
        assert_eq!(services_to_move(Some(32), &served(None)), vec!["fs"]);
        assert!(services_to_move(Some(32), &served(Some(32))).is_empty());
        assert!(services_to_move(None, &served(Some(31))).is_empty());
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
            &[member("tilo-session", "cc"), member("ush", "dd")],
        );
        assert_eq!(partner.account, "device");
        assert_eq!(partner.components.get("tilo-session").unwrap(), "cc");
        assert_eq!(partner.components.get("ush").unwrap(), "dd");
        assert_eq!(partner.components.get("model-manager").unwrap(), "bb");
    }
}
