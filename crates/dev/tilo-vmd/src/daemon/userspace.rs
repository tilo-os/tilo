//! One admitted, source-bound userspace generation and one final refresh.

use super::{
    Action, ApplyResult, BTreeSet, ComponentInfo, Daemon, DaemonError, Instant, Path, Policy,
    Provenance, Scope, Session, SessionChange, SlotRecord, abi_check_verified, apply, fs, slots,
};
use tilo_vm_protocol::{USERSPACE_COMPONENTS, UserspaceApply, UserspacePhase, UserspaceResult};

struct Admitted {
    record: SlotRecord,
    entries: Vec<(String, slots::Entry)>,
    cohort: crate::cohort::VerifiedCohort,
    warnings: Vec<String>,
}

fn source_matches(candidate: &Provenance, source: &Provenance) -> bool {
    candidate.source_commit == source.source_commit
        && candidate.dirty_tree_hash == source.dirty_tree_hash
        && candidate.cargo_lock_hash == source.cargo_lock_hash
}

impl Daemon {
    fn admit_userspace(&mut self, request: &UserspaceApply) -> Result<Admitted, DaemonError> {
        self.admit_userspace_against(request, Path::new("/usr/share/tilo-apps"))
    }

    fn admit_userspace_against(
        &mut self,
        request: &UserspaceApply,
        base_registry: &Path,
    ) -> Result<Admitted, DaemonError> {
        let record = self.load_slot(&request.slot)?;
        if record.current != request.expected_generation
            || self.slots.pending(&request.slot).is_some()
        {
            return Err(DaemonError::InvalidParams(
                "userspace cohort: generation changed or a transition is pending".into(),
            ));
        }
        let session = record
            .session()
            .ok_or_else(|| DaemonError::NoSession(request.slot.clone()))?;
        if session.identity() != request.expected_session {
            return Err(DaemonError::InvalidParams(
                "userspace cohort: owner session changed during staging".into(),
            ));
        }
        if session.logged_out() {
            return Err(DaemonError::LoggedOut(request.slot.clone()));
        }
        if request.source.source_commit.is_none()
            || request.source_inventory.len() != 64
            || !request
                .source_inventory
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || request.members.len() != USERSPACE_COMPONENTS.len()
        {
            return Err(DaemonError::InvalidParams(
                "userspace cohort: missing or unbounded source inventory".into(),
            ));
        }
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        let mut cohort = None;
        for (member, expected) in request.members.iter().zip(USERSPACE_COMPONENTS) {
            if member.component != *expected
                || member.build.component != *expected
                || member.source_inventory != request.source_inventory
                || !source_matches(&member.build, &request.source)
            {
                return Err(DaemonError::InvalidParams(
                    "userspace cohort: member source or order differs from the inventory".into(),
                ));
            }
            let (entry, verified, notes) = self.verify_userspace_member(member, base_registry)?;
            if let Some(verified) = verified {
                cohort = Some(verified);
            }
            warnings.extend(notes);
            entries.push((member.component.clone(), entry));
        }
        Ok(Admitted {
            record,
            entries,
            cohort: cohort.ok_or_else(|| {
                DaemonError::InvalidParams("userspace cohort has no paired apps/runtime".into())
            })?,
            warnings,
        })
    }

    fn verify_userspace_member(
        &self,
        member: &tilo_vm_protocol::UserspaceMember,
        base_registry: &Path,
    ) -> Result<
        (
            slots::Entry,
            Option<crate::cohort::VerifiedCohort>,
            Vec<String>,
        ),
        DaemonError,
    > {
        let component = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.get(&member.component))
            .ok_or_else(|| {
                DaemonError::InvalidParams(
                    "userspace cohort: required canonical component is absent".into(),
                )
            })?;
        if component.info.scope != Scope::Session || component.info.reboot {
            return Err(DaemonError::InvalidParams(
                "userspace cohort cannot change boot or VM-wide components".into(),
            ));
        }
        // Admission rechecks stored content even when the generation already
        // names it. A no-op is not permission to trust an old import receipt.
        let verified_layer = self.layers.verify(&member.layer)?;
        let path = self.layers.require(&member.layer)?;
        let historical = verified_layer.provenance;
        if historical.component != member.component
            || member.build.target.as_deref()
                != Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH).as_str())
            || member.build.cargo_profile.as_deref() != Some(component.profile.as_str())
        {
            return Err(DaemonError::InvalidParams(
                "userspace cohort: imported component, build profile or architecture differs"
                    .into(),
            ));
        }
        let cohort = if member.component == "apps+runtime" {
            if !component.image_assembled
                || member.build.cargo_profile.as_deref() != Some("thin-app")
            {
                return Err(DaemonError::InvalidParams(
                    "userspace cohort requires the complete thin graph".into(),
                ));
            }
            let cohort = crate::cohort::verify(&path, base_registry, std::env::consts::ARCH)?;
            if cohort.source_inventory.as_deref() != Some(member.source_inventory.as_str()) {
                return Err(DaemonError::InvalidParams(
                    "userspace cohort: signed system membership differs from the source inventory"
                        .into(),
                ));
            }
            Some(cohort)
        } else {
            None
        };
        let warnings = abi_check_verified(
            &historical,
            &self.base,
            std::env::consts::ARCH,
            cohort.as_ref(),
        )?;
        Ok((
            slots::Entry {
                layer: member.layer.clone(),
                info: component.info.clone(),
            },
            cohort,
            warnings,
        ))
    }

    pub(super) fn apply_userspace(
        &mut self,
        request: UserspaceApply,
    ) -> Result<UserspaceResult, DaemonError> {
        let start = Instant::now();
        let admitted = self.admit_userspace(&request)?;
        let phases = vec![UserspacePhase {
            name: "validate".into(),
            seconds: start.elapsed().as_secs_f64(),
        }];
        self.activate_userspace(request, admitted, phases)
    }

    fn prepare_userspace_transition(
        &mut self,
        request: &UserspaceApply,
        admitted: &mut Admitted,
    ) -> Result<
        (
            Session,
            crate::SelectedApps,
            slots::Generation,
            slots::Generation,
        ),
        DaemonError,
    > {
        let session = admitted
            .record
            .session()
            .ok_or_else(|| DaemonError::NoSession(request.slot.clone()))?;
        let preview = self
            .slots
            .userspace_preview(&admitted.record, &admitted.entries)?;
        let candidate_layers = self.system_layers(&admitted.record, self.layer_paths(&preview)?)?;
        // All builds and imports finished. Read actual running selections before
        // any sysext mutation, with the exact candidate catalog and old SDK view.
        self.apply
            .prepare_cohort_selection(&session, &candidate_layers)?;
        let selected = self
            .apply
            .cohort_selected_apps(&session, &candidate_layers)?;
        if !selected.apps.is_empty() {
            let prior_generation = self
                .slots
                .generation(&request.slot, admitted.record.current)?;
            let prior_layers =
                self.system_layers(&admitted.record, self.layer_paths(&prior_generation)?)?;
            let prior = self
                .apply
                .cohort_runtime_view(&session, &prior_layers, true)?;
            let candidate = self
                .apply
                .cohort_runtime_view(&session, &candidate_layers, false)?;
            if let Some(stage) = super::runtime_retention::prepare(
                &self.root.join("staging"),
                &prior,
                &candidate,
                &selected,
            )? {
                let mut provenance = (*request.source).clone();
                provenance.component = "runtime-retention".into();
                let imported = self.layers.import_path(&stage, &provenance, &mut |path| {
                    self.apply.label_layer(path)
                });
                let _ = fs::remove_dir_all(stage);
                let (layer, _) = imported?;
                admitted.entries.push((
                    "runtime-retention".into(),
                    slots::Entry {
                        layer,
                        info: ComponentInfo::new(Scope::Session, Vec::new()),
                    },
                ));
            }
        }
        let (from, to) = self
            .slots
            .prepare_userspace(&admitted.record, &admitted.entries)?;
        Ok((session, selected, from, to))
    }

    pub(super) fn cohort_is_active(
        &mut self,
        session: &Session,
        layers: &[std::path::PathBuf],
        cohort: &crate::cohort::VerifiedCohort,
    ) -> Result<bool, DaemonError> {
        self.apply.prepare_cohort_selection(session, layers)?;
        let selected = self.apply.cohort_selected_apps(session, layers)?;
        if !cohort.selections_identical(&selected) {
            return Ok(false);
        }
        let view = self.apply.cohort_runtime_view(session, layers, true)?;
        super::runtime_retention::validate_current(&view, &selected)?;
        self.verify_parity(Some(session.clone()))?;
        Ok(true)
    }

    fn userspace_identical(
        &mut self,
        request: &UserspaceApply,
        admitted: &Admitted,
    ) -> Result<bool, DaemonError> {
        let current = self
            .slots
            .generation(&request.slot, admitted.record.current)?;
        if admitted
            .entries
            .iter()
            .any(|(name, entry)| current.components.get(name) != Some(entry))
        {
            return Ok(false);
        }
        let session = admitted
            .record
            .session()
            .ok_or_else(|| DaemonError::NoSession(request.slot.clone()))?;
        let layers = self.system_layers(&admitted.record, self.layer_paths(&current)?)?;
        if !self.cohort_is_active(&session, &layers, &admitted.cohort)? {
            return Ok(false);
        }
        let record = self.load_slot(&request.slot)?;
        if record.current != request.expected_generation
            || record.session().as_ref() != Some(&session)
            || self.slots.pending(&request.slot).is_some()
        {
            return Err(DaemonError::InvalidParams(
                "userspace cohort: session changed during no-op verification".into(),
            ));
        }
        Ok(true)
    }

    fn activate_userspace(
        &mut self,
        request: UserspaceApply,
        admitted: Admitted,
        mut phases: Vec<UserspacePhase>,
    ) -> Result<UserspaceResult, DaemonError> {
        let preparation = Instant::now();
        if self.userspace_identical(&request, &admitted)? {
            phases.push(UserspacePhase {
                name: "verify-active-no-op".into(),
                seconds: preparation.elapsed().as_secs_f64(),
            });
            let mut apply = super::unchanged(admitted.record);
            apply.warnings = admitted.warnings;
            return Ok(UserspaceResult {
                apply,
                source: *request.source,
                source_inventory: request.source_inventory,
                members: request.members,
                phases,
                failure: None,
            });
        }
        self.activate_changed_userspace(request, admitted, phases)
    }

    fn activate_changed_userspace(
        &mut self,
        request: UserspaceApply,
        mut admitted: Admitted,
        mut phases: Vec<UserspacePhase>,
    ) -> Result<UserspaceResult, DaemonError> {
        let preparation = Instant::now();
        let (session, selected, from, to) =
            self.prepare_userspace_transition(&request, &mut admitted)?;
        let (_, restarts) = apply::policy(&admitted.record, &from, &to)?;
        let layers = self.system_layers(&admitted.record, self.layer_paths(&to)?)?;
        phases.push(UserspacePhase {
            name: "prepare-selections-and-runtime-retention".into(),
            seconds: preparation.elapsed().as_secs_f64(),
        });
        self.slots.mark_pending(&request.slot, to.number)?;
        let start = Instant::now();
        let mut actions = apply::activate(
            self.apply.as_mut(),
            &mut admitted.record,
            &to,
            &layers,
            Policy::SessionRestart,
            &restarts,
            SessionChange::Stage,
        )?;
        phases.push(UserspacePhase {
            name: "stage".into(),
            seconds: start.elapsed().as_secs_f64(),
        });
        self.apply.prepare_cohort_selection(&session, &layers)?;
        let mut selection = Vec::new();
        let start = Instant::now();
        let failure = crate::cohort::select_into(
            self.apply.as_mut(),
            &session,
            &admitted.cohort,
            Some(&layers),
            &mut selection,
        )
        .err()
        .map(|error| error.to_string());
        phases.push(UserspacePhase {
            name: "select".into(),
            seconds: start.elapsed().as_secs_f64(),
        });
        let current_selections = self.apply.cohort_selected_apps(&session, &layers)?;
        let retained_versions: BTreeSet<_> = selected
            .apps
            .iter()
            .map(|row| row.runtime.as_str())
            .collect();
        if current_selections.apps.iter().any(|row| {
            !row.runtime.is_empty()
                && row.runtime != format!("tilo-runtime/{}", admitted.cohort.version)
                && !retained_versions.contains(row.runtime.as_str())
        }) {
            return Err(DaemonError::InvalidParams("an intervening selection requires a runtime not admitted by this cohort; the owner desktop was not refreshed".into()));
        }
        if !current_selections.apps.is_empty() {
            let current_view = self.apply.cohort_runtime_view(&session, &layers, false)?;
            super::runtime_retention::validate_current(&current_view, &current_selections)?;
        }
        // Selection is individually durable. Reconcile even a partial failure;
        // retained old SDK closure keeps held/intervening selections runnable.
        let start = Instant::now();
        let current = self.load_slot(&request.slot)?;
        if current.current != request.expected_generation
            || current.session().as_ref() != Some(&session)
        {
            return Err(DaemonError::InvalidParams("userspace cohort: session changed before final refresh; staged generation remains pending".into()));
        }
        if session.login.is_some() {
            for unit in &restarts.user_units {
                self.apply.restart_user_unit(&session.user, unit)?;
                actions.push(Action::RestartUserUnit {
                    user: session.user.clone(),
                    unit: unit.clone(),
                });
            }
        }
        actions.retain(|action| !matches!(action, Action::Deferred { .. }));
        let uid = apply::session_uid(self.apply.as_mut(), &session)?;
        actions.extend(apply::restart_session(self.apply.as_mut(), &session, uid)?);
        self.verify_parity(Some(session))?;
        self.slots.set_current(&mut admitted.record, to.number)?;
        phases.push(UserspacePhase {
            name: "refresh".into(),
            seconds: start.elapsed().as_secs_f64(),
        });
        Ok(UserspaceResult {
            apply: ApplyResult {
                slot: request.slot,
                generation: to.number,
                changed: true,
                policy: Policy::SessionRestart,
                actions,
                cohort_selection: selection,
                warnings: admitted.warnings,
                boot: None,
            },
            source: *request.source,
            source_inventory: request.source_inventory,
            members: request.members,
            phases,
            failure,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Daemon, Path, Provenance, fs};
    use crate::apply::RecordingApply;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use tilo_vm_protocol::{
        BaseInfo, DEVICE_SLOT, Owner, SlotKind, USERSPACE_COMPONENTS, UserspaceApply,
        UserspaceMember, manifest::Manifest,
    };

    fn fixture_request(daemon: &mut Daemon, root: &Path, reused: bool) -> UserspaceApply {
        let mut source = Provenance::local("userspace");
        source.source_commit = Some("42".repeat(20));
        source.cargo_lock_hash = Some("source-lock".into());
        source.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
        let mut members = Vec::new();
        for name in USERSPACE_COMPONENTS {
            let mut provenance = source.clone();
            provenance.component = (*name).into();
            provenance.cargo_profile = Some(
                if *name == "apps+runtime" {
                    "thin-app"
                } else {
                    "release"
                }
                .into(),
            );
            let path = if *name == "apps+runtime" {
                root.join("layer")
            } else {
                let path = root.join(name);
                fs::create_dir_all(path.join("usr/bin")).unwrap();
                fs::write(path.join("usr/bin").join(name), name).unwrap();
                path
            };
            let mut historical = provenance.clone();
            if reused {
                historical.source_commit = Some("old-source".into());
            }
            let (layer, _) = daemon
                .layers
                .import_path(&path, &historical, &mut |_| Ok(()))
                .unwrap();
            members.push(UserspaceMember {
                component: (*name).into(),
                layer,
                build: provenance,
                source_inventory: "ab".repeat(32),
            });
        }
        let record = daemon.load_slot(DEVICE_SLOT).unwrap();
        UserspaceApply {
            slot: DEVICE_SLOT.into(),
            expected_generation: record.current,
            expected_session: record.session().unwrap().identity(),
            source: Box::new(source),
            source_inventory: "ab".repeat(32),
            members,
        }
    }

    fn alter_request(request: &mut UserspaceApply, case: &str) {
        match case {
            "stale" => request.expected_generation = 9,
            "owner" => request.expected_session.login_wrapper = Some(999),
            "drift" => request.members.last_mut().unwrap().source_inventory = "cd".repeat(32),
            "missing" => request.members.last_mut().unwrap().layer = "ef".repeat(32),
            "signed-drift" => {
                request.source_inventory = "cd".repeat(32);
                for member in &mut request.members {
                    member.source_inventory = request.source_inventory.clone();
                }
            }
            _ => {}
        }
    }

    fn configure_repeat(
        apply: &mut RecordingApply,
        packages: &[package_store::registry::InstalledPackage],
        root: &Path,
        case: &str,
    ) {
        apply.cohort_runtime_root = Some(root.join("layer"));
        let mut selected = packages.to_vec();
        if case == "noop-root-drift" {
            selected.first_mut().unwrap().package_id_hex = "ff".repeat(32);
        } else if case == "noop-duplicate" {
            selected.push(selected.first().unwrap().clone());
        }
        apply.cohort_selections.extend([
            crate::SelectedApps::default(),
            crate::SelectedApps::default(),
            crate::SelectedApps {
                apps: if case == "noop-missing" {
                    Vec::new()
                } else {
                    selected
                },
                runtime_paths: std::collections::BTreeMap::default(),
            },
        ]);
        for package in packages {
            apply.cohort_replies.push_back(format!(
                "cohort-selection: {}\n",
                serde_json::json!({
                    "id": package.id, "version": package.version, "root": package.package_id_hex,
                    "runtime": package.runtime, "preserved": false, "held": false, "pinned": true,
                })
            ));
        }
    }

    fn verify_repeat(
        daemon: &mut Daemon,
        mut request: UserspaceApply,
        root: &Path,
        case: &str,
        mirror: &Arc<Mutex<Vec<String>>>,
    ) {
        request.expected_generation = 1;
        mirror.lock().unwrap().clear();
        if case == "noop-damaged" {
            let layer = &request.members.first().unwrap().layer;
            fs::write(
                daemon.layers.path(layer).join("usr/bin/launcher"),
                "damaged",
            )
            .unwrap();
            assert!(
                daemon
                    .admit_userspace_against(&request, &root.join("base"))
                    .is_err()
            );
            assert!(mirror.lock().unwrap().is_empty());
            assert_eq!(daemon.load_slot(&request.slot).unwrap().current, 1);
            return;
        }
        let admitted = daemon
            .admit_userspace_against(&request, &root.join("base"))
            .unwrap();
        let repeated = daemon
            .activate_userspace(request, admitted, Vec::new())
            .unwrap();
        if case == "noop" {
            assert!(!repeated.apply.changed);
            assert_eq!(repeated.apply.generation, 1);
            assert!(repeated.apply.actions.is_empty());
            assert_eq!(daemon.slots.generations(&repeated.apply.slot), vec![1]);
            let calls = mirror.lock().unwrap().clone();
            assert!(
                calls
                    .iter()
                    .any(|call| call.starts_with("cohort_selected_apps "))
            );
            assert!(
                calls
                    .iter()
                    .any(|call| call.starts_with("wait_session_ready "))
            );
            assert!(
                !calls
                    .iter()
                    .any(|call| call.starts_with("install_cohort_app ")
                        || call.starts_with("restart")
                        || call.starts_with("stop ")
                        || call.starts_with("start ")
                        || call.starts_with("write_dropin "))
            );
        } else {
            assert!(repeated.apply.changed);
            assert_eq!(repeated.apply.generation, 2);
            assert_eq!(repeated.apply.cohort_selection.len(), 2);
        }
    }

    fn verify_first_refresh(
        result: &tilo_vm_protocol::UserspaceResult,
        mirror: &Arc<Mutex<Vec<String>>>,
    ) {
        let calls = mirror.lock().unwrap();
        let selection = calls
            .iter()
            .position(|line| line.starts_with("install_cohort_app "))
            .unwrap();
        let transitions = result
            .apply
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    Action::RestartDesktop { .. } | Action::StartUnit { .. }
                )
            })
            .count();
        assert_eq!(transitions, 1, "{calls:?}");
        let refresh = calls
            .iter()
            .position(|line| line.starts_with("restart_desktop ") || line.starts_with("stop "))
            .unwrap();
        assert!(
            selection < refresh,
            "selection must finish before the sole transition: {calls:?}"
        );
        assert!(calls.iter().any(|line| line.starts_with("cohort_view ")));
        drop(calls);
    }

    fn subject(root: &Path) {
        for case in [
            "valid",
            "partial",
            "stale",
            "owner",
            "drift",
            "missing",
            "reused",
            "ordinary",
            "signed-drift",
            "noop",
            "noop-missing",
            "noop-root-drift",
            "noop-duplicate",
            "noop-damaged",
        ] {
            let state = root.join(format!("state-{case}"));
            let mirror = Arc::new(Mutex::new(Vec::new()));
            let mut apply = RecordingApply::with_login("owner");
            apply.mirror = Some(mirror.clone());
            let row =
                package_store::registry::Registry::load(&root.join("base/registry.toml")).unwrap();
            for (index, package) in row.entries().iter().enumerate() {
                if case == "partial" && index > 0 {
                    break;
                }
                apply.cohort_replies.push_back(format!("cohort-selection: {}\n", serde_json::json!({
                    "id": package.id, "version": package.version, "root": package.package_id_hex,
                    "runtime": package.runtime, "preserved": false, "held": false, "pinned": true,
                })));
            }
            if case.starts_with("noop") {
                configure_repeat(&mut apply, row.entries(), root, case);
            }
            let manifest = Manifest::load(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/components.toml"),
            )
            .unwrap()
            .unwrap();
            let mut daemon =
                Daemon::new(&state, Box::new(apply), Some(manifest), BaseInfo::default());
            let mut request = fixture_request(&mut daemon, root, case == "reused");
            if case == "ordinary" {
                let owner = Owner {
                    host: Some("original-owner".into()),
                    ..Owner::default()
                };
                let (record, _) = daemon
                    .slots
                    .create("owned-slot", SlotKind::Headless, owner, &[])
                    .unwrap();
                request.slot = record.name.clone();
                request.expected_session = record.session().unwrap().identity();
            }
            alter_request(&mut request, case);
            mirror.lock().unwrap().clear();
            let before = daemon.load_slot(&request.slot).unwrap();
            let result = daemon.admit_userspace_against(&request, &root.join("base"));
            if matches!(
                case,
                "stale" | "owner" | "drift" | "missing" | "signed-drift"
            ) {
                assert!(result.is_err(), "{case} must fail before activation");
                assert!(mirror.lock().unwrap().is_empty());
                assert!(daemon.slots.generations(&request.slot).is_empty());
                continue;
            }
            let result = daemon
                .activate_userspace(request.clone(), result.unwrap(), Vec::new())
                .unwrap();
            assert_eq!(result.apply.generation, 1);
            assert_eq!(result.failure.is_some(), case == "partial");
            assert_eq!(
                result.apply.cohort_selection.len(),
                if case == "partial" { 1 } else { 2 }
            );
            let after = daemon.load_slot(&result.apply.slot).unwrap();
            assert_eq!(before.owner, after.owner);
            assert_eq!(before.device, after.device);
            assert_eq!(after.current, 1);
            verify_first_refresh(&result, &mirror);
            if case.starts_with("noop") {
                verify_repeat(&mut daemon, request, root, case, &mirror);
            }
        }
    }

    #[test]
    fn userspace_cohort_admits_all_imports_and_preserves_one_owner_transition() {
        const SUBJECT: &str = "TILO_VMD_USERSPACE_SUBJECT";
        if let Ok(root) = std::env::var(SUBJECT) {
            subject(Path::new(&root));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        crate::cohort::tests::fixture_pair(root.path());
        let output = Command::new("timeout").args(["60", "bwrap", "--unshare-all", "--uid", "0", "--gid", "0", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--bind"])
            .arg(root.path()).arg(root.path()).args(["--setenv", SUBJECT]).arg(root.path())
            .arg(std::env::current_exe().unwrap()).args(["daemon::userspace::tests::userspace_cohort_admits_all_imports_and_preserves_one_owner_transition", "--exact", "--nocapture"])
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
