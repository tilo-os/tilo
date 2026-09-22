//! Request handling: one [`Daemon`] per RPC connection.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use tilo_dev_protocol::{
    Action, ApplyResult, BaseInfo, ComponentInfo, GcResult, HelloResult, Issue, LayerImportResult,
    LayerSource, LayerUploadBeginResult, LayerUploadChunkResult, Lease, LogsResult, Manifest,
    Owner, PROTOCOL_VERSION, Policy, Provenance, Request, Scope, SeatResult, ShotResult,
    SlotCreateResult, SlotInfo, SlotKind, SlotRmResult, StatusResult, Storage, UPLOAD_CHUNK_BYTES,
    VM_SLOT,
};

use crate::apply::{self, Apply, GREETER_UNIT, SessionChange, session_unit};
use crate::error::DaemonError;
use crate::layers::LayerStore;
use crate::locks::{Guard, Locks};
use crate::slots::{self, Generation, SlotRecord, SlotStore};

/// Layers and uploads younger than this survive gc: they may belong to a
/// swap that is still in flight on another connection.
const GC_MIN_AGE: Duration = Duration::from_secs(3600);

pub struct Daemon {
    layers: LayerStore,
    slots: SlotStore,
    locks: Locks,
    uploads_dir: PathBuf,
    uploads: BTreeMap<String, PathBuf>,
    apply: Box<dyn Apply>,
    manifest: Option<Manifest>,
    base: BaseInfo,
    client_protocol: Option<u32>,
    root: PathBuf,
}

fn to_value<T: Serialize>(value: &T) -> Result<serde_json::Value, DaemonError> {
    serde_json::to_value(value).map_err(|e| DaemonError::InvalidParams(e.to_string()))
}

fn unchanged(record: SlotRecord) -> ApplyResult {
    ApplyResult {
        slot: record.name,
        generation: record.current,
        changed: false,
        policy: Policy::NoRestart,
        actions: Vec::new(),
        warnings: Vec::new(),
    }
}

/// Refuse layers the VM cannot run; warn about differences it can.
///
/// # Errors
/// [`DaemonError::AbiMismatch`] for a foreign architecture or builder.
pub fn abi_check(
    provenance: &Provenance,
    base: &BaseInfo,
    arch: &str,
) -> Result<Vec<String>, DaemonError> {
    let mismatch = |field: &str, layer: &str, vm: &str| DaemonError::AbiMismatch {
        field: field.into(),
        layer: layer.into(),
        vm: vm.into(),
    };
    if let Some(target) = &provenance.target
        && target.split('-').next() != Some(arch)
    {
        return Err(mismatch("target", target, arch));
    }
    if let (Some(layer), Some(vm)) = (&provenance.builder_image_digest, &base.builder_image_digest)
        && layer != vm
    {
        return Err(mismatch("builder image", layer, vm));
    }
    let mut warnings = Vec::new();
    if let (Some(layer), Some(vm)) = (&provenance.base_image_digest, &base.image_digest)
        && layer != vm
    {
        warnings.push(format!(
            "layer was built against base {layer}, the VM runs {vm}"
        ));
    }
    if let (Some(layer), Some(vm)) = (&provenance.toolchain, &base.toolchain)
        && layer != vm
    {
        warnings.push(format!(
            "layer toolchain {layer} differs from the VM's {vm}"
        ));
    }
    if provenance.builder.as_deref() == Some("host") && base.builder_image_digest.is_some() {
        warnings.push(
            "host build: not built in the VM's builder image, so ABI parity is not guaranteed"
                .into(),
        );
    }
    Ok(warnings)
}

impl Daemon {
    pub fn new(
        root: &Path,
        apply: Box<dyn Apply>,
        manifest: Option<Manifest>,
        base: BaseInfo,
    ) -> Self {
        Self {
            layers: LayerStore::new(root),
            slots: SlotStore::new(root),
            locks: Locks::new(root),
            uploads_dir: root.join("uploads"),
            uploads: BTreeMap::new(),
            apply,
            manifest,
            base,
            client_protocol: None,
            root: root.to_owned(),
        }
    }

    /// Handle one request.
    ///
    /// # Errors
    /// Whatever the method reports; see [`DaemonError::hint`].
    pub fn handle(&mut self, request: Request) -> Result<serde_json::Value, DaemonError> {
        if let Request::Hello(hello) = &request {
            self.client_protocol = Some(hello.protocol);
            return to_value(&HelloResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                root: self.root.display().to_string(),
                arch: std::env::consts::ARCH.into(),
                base: self.base.clone(),
                issues: self.issues()?,
            });
        }
        match self.client_protocol {
            None => return Err(DaemonError::HelloRequired),
            Some(client) if client != PROTOCOL_VERSION => {
                return Err(DaemonError::VersionMismatch {
                    client,
                    daemon: PROTOCOL_VERSION,
                });
            }
            Some(_) => {}
        }
        match request {
            Request::Hello(_) => Err(DaemonError::HelloRequired),
            Request::Status(p) => to_value(&self.status(p.slot)?),
            Request::SlotCreate(p) => {
                let _locks = self.slot_locks(&p.name)?;
                to_value(&self.create(&p.name, p.kind, p.owner)?)
            }
            Request::SlotList(_) => {
                let records = self.slots.list()?;
                to_value(
                    &records
                        .iter()
                        .map(|r| self.info(r))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Request::SlotRestart(p) => {
                let _locks = self.slot_locks(&p.name)?;
                to_value(&self.restart(&p.name)?)
            }
            Request::SlotRm(p) => {
                let _locks = self.slot_locks(&p.name)?;
                to_value(&self.remove(&p.name)?)
            }
            Request::LayerUploadBegin(_) => to_value(&self.upload_begin()?),
            Request::LayerUploadChunk(p) => to_value(&self.upload_chunk(&p.upload, &p.data)?),
            Request::LayerImport(p) => {
                let _vm = self.locks.vm_shared()?;
                to_value(&self.import(p.source, &p.provenance)?)
            }
            Request::LayerList(_) => to_value(&self.layers.list()?),
            Request::Swap(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.swap(&p.slot, &p.component, &p.layer, p.info)?)
            }
            Request::Rollback(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.rollback(&p.slot, p.to)?)
            }
            Request::Logs(p) => {
                let record = self.slots.load(&p.slot)?;
                let matches = self.journal_matches(&record)?;
                to_value(&LogsResult {
                    text: self.apply.journal(&matches, p.lines)?,
                })
            }
            Request::SeatClaim(p) => {
                let _vm = self.locks.vm_shared()?;
                let _seat = self.locks.slot("seat0")?;
                to_value(&self.claim(&p.slot, p.owner, p.steal)?)
            }
            Request::SeatRelease(p) => {
                let _vm = self.locks.vm_shared()?;
                let _seat = self.locks.slot("seat0")?;
                to_value(&self.release(&p.slot)?)
            }
            Request::Shot(p) => {
                let (record, user) = self.user_slot(&p.slot)?;
                let png = self.apply.shot(&record.name, &user)?;
                to_value(&ShotResult {
                    png: STANDARD.encode(png),
                })
            }
            Request::Control(p) => {
                let (record, user) = self.user_slot(&p.slot)?;
                to_value(&self.apply.control(&record.name, &user, &p.args)?)
            }
            Request::Gc(p) => {
                let _vm = self.locks.vm_exclusive()?;
                to_value(&self.gc(p.keep, p.dry_run)?)
            }
        }
    }

    fn status(&mut self, slot: Option<String>) -> Result<StatusResult, DaemonError> {
        let records = match slot {
            Some(name) => vec![self.slots.load(&name)?],
            None => self.slots.list()?,
        };
        let slots = records
            .iter()
            .map(|r| self.info(r))
            .collect::<Result<_, _>>()?;
        let seat0 = self.slots.list()?.into_iter().find_map(|r| r.seat0);
        Ok(StatusResult {
            protocol: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").into(),
            slots,
            seat0,
            storage: self.storage(),
            issues: self.issues()?,
        })
    }

    fn upload_begin(&mut self) -> Result<LayerUploadBeginResult, DaemonError> {
        fs::create_dir_all(&self.uploads_dir).map_err(DaemonError::io("create uploads"))?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = self.uploads_dir.join(format!("{id}.tar"));
        fs::File::create(&path).map_err(DaemonError::io("create upload"))?;
        self.uploads.insert(id.clone(), path);
        Ok(LayerUploadBeginResult { upload: id })
    }

    fn upload_chunk(
        &self,
        upload: &str,
        data: &str,
    ) -> Result<LayerUploadChunkResult, DaemonError> {
        let path = self
            .uploads
            .get(upload)
            .ok_or_else(|| DaemonError::UploadNotFound(upload.to_owned()))?;
        let bytes = STANDARD
            .decode(data)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        if bytes.len() > UPLOAD_CHUNK_BYTES {
            return Err(DaemonError::InvalidParams(format!(
                "chunk exceeds {UPLOAD_CHUNK_BYTES} bytes"
            )));
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(DaemonError::io("open upload"))?;
        file.write_all(&bytes)
            .map_err(DaemonError::io("write upload"))?;
        let received = file
            .metadata()
            .map_err(DaemonError::io("stat upload"))?
            .len();
        Ok(LayerUploadChunkResult { received })
    }

    fn import(
        &mut self,
        source: LayerSource,
        provenance: &Provenance,
    ) -> Result<LayerImportResult, DaemonError> {
        let apply = &mut self.apply;
        let (hash, existed) = match source {
            LayerSource::Upload(id) => {
                let path = self
                    .uploads
                    .remove(&id)
                    .ok_or(DaemonError::UploadNotFound(id))?;
                let file = fs::File::open(&path).map_err(DaemonError::io("open upload"));
                let result = file.and_then(|f| {
                    self.layers
                        .import_tar(std::io::BufReader::new(f), provenance, &mut |l| {
                            apply.label_layer(l)
                        })
                });
                let _ = fs::remove_file(&path);
                result?
            }
            LayerSource::Path(path) => {
                self.layers
                    .import_path(Path::new(&path), provenance, &mut |l| apply.label_layer(l))?
            }
        };
        Ok(LayerImportResult { hash, existed })
    }

    /// VM-wide work on the `vm` slot excludes everything; slot work shares
    /// the VM and excludes other work on the same slot.
    fn slot_locks(&self, name: &str) -> Result<(Guard, Option<Guard>), DaemonError> {
        if name == VM_SLOT {
            Ok((self.locks.vm_exclusive()?, None))
        } else {
            Ok((self.locks.vm_shared()?, Some(self.locks.slot(name)?)))
        }
    }

    fn user_slot(&self, name: &str) -> Result<(SlotRecord, String), DaemonError> {
        let record = self.slots.load(name)?;
        let user = record
            .user()
            .ok_or_else(|| DaemonError::NoSession(name.to_owned()))?;
        Ok((record, user))
    }

    fn storage(&mut self) -> Storage {
        let space = self.apply.disk_space(&self.root);
        Storage {
            layers_bytes: self.layers.allocated_bytes(),
            free_bytes: space.map(|s| s.0),
            total_bytes: space.map(|s| s.1),
        }
    }

    /// Reconcile recorded state with what is running.
    fn issues(&mut self) -> Result<Vec<Issue>, DaemonError> {
        let mut issues = Vec::new();
        for record in self.slots.list()? {
            if let Some(number) = self.slots.pending(&record.name) {
                issues.push(Issue {
                    slot: record.name.clone(),
                    problem: format!(
                        "a transition to generation {number} was interrupted; generation {} is recorded as current",
                        record.current
                    ),
                    hint: format!("re-run the swap, or re-apply the current generation: tilo-dev slot restart {}", record.name),
                });
            }
            if record.user().is_some() {
                let state = self.apply.unit_state(&session_unit(&record.name));
                if state != "active" && state != "activating" {
                    issues.push(Issue {
                        slot: record.name.clone(),
                        problem: format!("session is {state}"),
                        hint: format!(
                            "tilo-dev logs {}   then   tilo-dev slot restart {}",
                            record.name, record.name
                        ),
                    });
                }
            }
        }
        Ok(issues)
    }

    fn info(&mut self, record: &SlotRecord) -> Result<SlotInfo, DaemonError> {
        let generation = self.slots.generation(&record.name, record.current)?;
        let session = if record.user().is_some() {
            self.apply.unit_state(&session_unit(&record.name))
        } else {
            "n/a".into()
        };
        Ok(SlotInfo {
            name: record.name.clone(),
            kind: record.kind,
            owner: record.owner.clone(),
            user: record.user(),
            generation: record.current,
            generations: self.slots.generations(&record.name),
            components: generation
                .components
                .into_iter()
                .map(|(k, v)| (k, v.layer))
                .collect(),
            session,
            seat: if record.seat0.is_some() {
                Some("seat0".into())
            } else {
                record.seat.clone()
            },
        })
    }

    fn layer_paths(&self, generation: &Generation) -> Result<Vec<PathBuf>, DaemonError> {
        generation
            .layers
            .iter()
            .map(|h| self.layers.require(h))
            .collect()
    }

    fn create(
        &mut self,
        name: &str,
        kind: SlotKind,
        owner: Owner,
    ) -> Result<SlotCreateResult, DaemonError> {
        let (mut record, created) = self.slots.create(name, kind, owner)?;
        let running = self.apply.unit_state(&session_unit(name)) == "active";
        let actions = if created || !running {
            let generation = self.slots.generation(name, record.current)?;
            let layers = self.layer_paths(&generation)?;
            let actions = apply::activate(
                self.apply.as_mut(),
                &mut record,
                &generation,
                &layers,
                Policy::NoRestart,
                &[],
                SessionChange::Ensure,
            )?;
            self.slots.save(&record)?;
            actions
        } else {
            Vec::new()
        };
        Ok(SlotCreateResult {
            slot: self.info(&record)?,
            created,
            actions,
        })
    }

    fn restart(&mut self, name: &str) -> Result<ApplyResult, DaemonError> {
        let mut record = self.slots.load(name)?;
        let generation = self.slots.generation(name, record.current)?;
        let layers = self.layer_paths(&generation)?;
        let (policy, units) = if record.kind == SlotKind::Vm {
            let units = generation
                .components
                .values()
                .flat_map(|e| e.info.units.clone())
                .collect();
            (Policy::VmRefresh, units)
        } else {
            (Policy::SessionRestart, Vec::new())
        };
        let mut actions = apply::activate(
            self.apply.as_mut(),
            &mut record,
            &generation,
            &layers,
            policy,
            &units,
            SessionChange::Restart,
        )?;
        self.slots.save(&record)?;
        self.slots.clear_pending(name)?;
        if record.kind == SlotKind::Vm {
            actions.extend(self.restart_all_sessions()?);
        }
        Ok(ApplyResult {
            slot: record.name,
            generation: record.current,
            changed: true,
            policy,
            actions,
            warnings: Vec::new(),
        })
    }

    fn remove(&mut self, name: &str) -> Result<SlotRmResult, DaemonError> {
        if name == VM_SLOT {
            return Err(DaemonError::ReservedName(name.to_owned()));
        }
        let record = match self.slots.load(name) {
            Ok(record) => record,
            Err(DaemonError::SlotNotFound(_)) => {
                return Ok(SlotRmResult {
                    removed: false,
                    actions: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let mut actions = apply::deactivate(self.apply.as_mut(), &record)?;
        if record.seat0.is_some() {
            self.apply.start_unit(GREETER_UNIT)?;
            actions.push(Action::StartUnit {
                unit: GREETER_UNIT.into(),
            });
        }
        let removed = self.slots.remove(name)?;
        Ok(SlotRmResult { removed, actions })
    }

    fn component_info(
        &self,
        record: &SlotRecord,
        component: &str,
        info: Option<ComponentInfo>,
    ) -> ComponentInfo {
        info.or_else(|| {
            self.manifest
                .as_ref()?
                .get(component)
                .map(|c| c.info.clone())
        })
        .unwrap_or_else(|| {
            // Unknown components take the scope of the slot they are swapped into.
            let scope = if record.kind == SlotKind::Vm {
                Scope::Vm
            } else {
                Scope::Session
            };
            ComponentInfo {
                scope,
                units: Vec::new(),
            }
        })
    }

    fn swap(
        &mut self,
        name: &str,
        component: &str,
        layer: &str,
        info: Option<ComponentInfo>,
    ) -> Result<ApplyResult, DaemonError> {
        let mut record = self.slots.load(name)?;
        self.layers.require(layer)?;
        let provenance = self
            .layers
            .provenance(layer)
            .unwrap_or_else(|| Provenance::local(component));
        let warnings = abi_check(&provenance, &self.base, std::env::consts::ARCH)?;
        let info = self.component_info(&record, component, info);
        apply::check_scope(&record, component, &info)?;
        let Some((from, to)) = self.slots.prepare_swap(&record, component, layer, info)? else {
            return Ok(ApplyResult {
                warnings,
                ..unchanged(record)
            });
        };
        let mut result = self.transition(&mut record, &from, &to)?;
        result.warnings = warnings;
        Ok(result)
    }

    fn rollback(&mut self, name: &str, target: Option<u64>) -> Result<ApplyResult, DaemonError> {
        let mut record = self.slots.load(name)?;
        let (from, to) = self.slots.rollback_target(&record, target)?;
        if from.number == to.number {
            return Ok(unchanged(record));
        }
        self.transition(&mut record, &from, &to)
    }

    fn transition(
        &mut self,
        record: &mut SlotRecord,
        from: &Generation,
        to: &Generation,
    ) -> Result<ApplyResult, DaemonError> {
        let (policy, units) = apply::policy(record, from, to)?;
        let layers = self.layer_paths(to)?;
        self.slots.mark_pending(&record.name, to.number)?;
        let mut actions = apply::activate(
            self.apply.as_mut(),
            record,
            to,
            &layers,
            policy,
            &units,
            SessionChange::Keep,
        )?;
        self.slots.set_current(record, to.number)?;
        if policy == Policy::VmRefresh {
            actions.extend(self.restart_all_sessions()?);
        }
        Ok(ApplyResult {
            slot: record.name.clone(),
            generation: to.number,
            changed: true,
            policy,
            actions,
            warnings: Vec::new(),
        })
    }

    /// Slot sessions snapshot `/usr` when they start, so a refreshed VM
    /// `/usr` reaches them only through a restart.
    fn restart_all_sessions(&mut self) -> Result<Vec<Action>, DaemonError> {
        let mut actions = Vec::new();
        for record in self.slots.list()? {
            let Some(user) = record.user() else { continue };
            let uid = self.apply.ensure_user(&user)?;
            actions.extend(apply::restart_session(
                self.apply.as_mut(),
                &record.name,
                uid,
            )?);
        }
        Ok(actions)
    }

    fn journal_matches(&mut self, record: &SlotRecord) -> Result<Vec<String>, DaemonError> {
        let mut matches = Vec::new();
        if let Some(user) = record.user() {
            // PAM moves the session into session-N.scope, so its output is
            // found by the unit's SyslogIdentifier, not by the unit name.
            let uid = self.apply.ensure_user(&user)?;
            matches.push(format!("SYSLOG_IDENTIFIER=tilo-session-{}", record.name));
            matches.push(format!("UNIT={}", session_unit(&record.name)));
            matches.push(format!("_UID={uid}"));
        } else {
            let generation = self.slots.generation(&record.name, record.current)?;
            for unit in generation
                .components
                .values()
                .flat_map(|e| e.info.units.iter())
            {
                matches.push(format!("_SYSTEMD_UNIT={unit}"));
                matches.push(format!("UNIT={unit}"));
            }
            matches.push("SYSLOG_IDENTIFIER=systemd-sysext".into());
        }
        Ok(matches)
    }

    /// Re-activate a slot's current generation with a session restart,
    /// after its seat assignment changed.
    fn reseat(&mut self, record: &mut SlotRecord) -> Result<Vec<Action>, DaemonError> {
        let generation = self.slots.generation(&record.name, record.current)?;
        let layers = self.layer_paths(&generation)?;
        let actions = apply::activate(
            self.apply.as_mut(),
            record,
            &generation,
            &layers,
            Policy::SessionRestart,
            &[],
            SessionChange::Restart,
        )?;
        self.slots.save(record)?;
        Ok(actions)
    }

    fn claim(&mut self, name: &str, owner: Owner, steal: bool) -> Result<SeatResult, DaemonError> {
        let (mut record, _) = self.user_slot(name)?;
        if record.seat0.is_some() {
            return Ok(SeatResult {
                slot: record.name,
                seat: "seat0".into(),
                changed: false,
                actions: Vec::new(),
            });
        }
        let mut actions = Vec::new();
        if let Some(mut holder) = self.slots.list()?.into_iter().find(|r| r.seat0.is_some()) {
            if !steal {
                let lease = holder.seat0.unwrap_or_else(|| Lease {
                    slot: holder.name.clone(),
                    owner: Owner::default(),
                    since: String::new(),
                });
                let owner = [lease.owner.host, lease.owner.worktree, lease.owner.branch]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                return Err(DaemonError::SeatBusy {
                    slot: lease.slot,
                    owner,
                    since: lease.since,
                });
            }
            holder.seat0 = None;
            actions.extend(self.reseat(&mut holder)?);
        } else {
            self.apply.stop_unit(GREETER_UNIT)?;
            actions.push(Action::StopUnit {
                unit: GREETER_UNIT.into(),
            });
        }
        record.seat0 = Some(Lease {
            slot: record.name.clone(),
            owner,
            since: slots::now(),
        });
        actions.extend(self.reseat(&mut record)?);
        Ok(SeatResult {
            slot: record.name,
            seat: "seat0".into(),
            changed: true,
            actions,
        })
    }

    fn release(&mut self, name: &str) -> Result<SeatResult, DaemonError> {
        let (mut record, _) = self.user_slot(name)?;
        let own_seat = |r: &SlotRecord| r.seat.clone().unwrap_or_else(|| "none".into());
        if record.seat0.is_none() {
            return Ok(SeatResult {
                seat: own_seat(&record),
                slot: record.name,
                changed: false,
                actions: Vec::new(),
            });
        }
        record.seat0 = None;
        let mut actions = self.reseat(&mut record)?;
        self.apply.start_unit(GREETER_UNIT)?;
        actions.push(Action::StartUnit {
            unit: GREETER_UNIT.into(),
        });
        Ok(SeatResult {
            seat: own_seat(&record),
            slot: record.name,
            changed: true,
            actions,
        })
    }

    fn gc(&mut self, keep: u32, dry_run: bool) -> Result<GcResult, DaemonError> {
        let mut referenced = BTreeSet::new();
        let mut removed_generations = Vec::new();
        let mut records = self.slots.list()?;
        if !records.iter().any(|r| r.name == VM_SLOT) {
            records.push(self.slots.load(VM_SLOT)?);
        }
        for record in &records {
            let (layers, removed) = self.slots.prune(record, keep, dry_run)?;
            referenced.extend(layers);
            removed_generations.extend(removed.into_iter().map(|n| format!("{}/{n}", record.name)));
        }
        let (removed_layers, freed_bytes) =
            self.layers.collect(&referenced, GC_MIN_AGE, dry_run)?;
        if !dry_run {
            // Uploads and staging dirs left by dropped connections.
            for dir in [&self.uploads_dir, &self.root.join("staging")] {
                for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                    let old = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if old.is_some_and(|age| age > GC_MIN_AGE) {
                        let path = entry.path();
                        let _ = if path.is_dir() {
                            fs::remove_dir_all(&path)
                        } else {
                            fs::remove_file(&path)
                        };
                    }
                }
            }
        }
        Ok(GcResult {
            dry_run,
            removed_layers,
            removed_generations,
            freed_bytes,
            storage: self.storage(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_refuses_foreign_arch_and_builder() {
        let base = BaseInfo {
            image_digest: Some("sha256:base".into()),
            builder_image_digest: Some("sha256:builder".into()),
            toolchain: Some("1.97.1".into()),
        };
        let mut layer = Provenance::local("shell");
        layer.target = Some("aarch64-unknown-linux-gnu".into());
        assert!(matches!(
            abi_check(&layer, &base, "x86_64"),
            Err(DaemonError::AbiMismatch { .. })
        ));
        layer.target = Some("x86_64-unknown-linux-gnu".into());
        assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
        layer.builder_image_digest = Some("sha256:other".into());
        assert!(matches!(
            abi_check(&layer, &base, "x86_64"),
            Err(DaemonError::AbiMismatch { .. })
        ));
        layer.builder_image_digest = None;
        layer.base_image_digest = Some("sha256:older".into());
        layer.toolchain = Some("1.96.0".into());
        assert_eq!(abi_check(&layer, &base, "x86_64").unwrap().len(), 2);
        // Nothing known about the VM: nothing to compare.
        assert!(
            abi_check(&layer, &BaseInfo::default(), "x86_64")
                .unwrap()
                .is_empty()
        );
    }
}
