//! Slots and their numbered generations under `<root>/slots/<name>/`.
//!
//! ```text
//! slots/<name>/slot.json          owner, kind, current generation, seat
//! slots/<name>/generations/<n>.json  ordered layers + component map
//! ```
//!
//! Generation 0 is the implicit base (no layers). Generation files are
//! immutable once written; switching is a single atomic rewrite of
//! `slot.json`, done only after the new generation was applied, so a
//! failed apply leaves the slot on its previous generation and a retry
//! does the whole transition again.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::display::format_outputs;
use tilo_vm_protocol::{
    ComponentInfo, DEVICE_SLOT, Lease, OutputSpec, Owner, SlotKind, VM_SLOT, valid_slot_name,
};

use crate::error::DaemonError;
use crate::fsutil::write_atomic;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRecord {
    pub name: String,
    pub kind: SlotKind,
    #[serde(default)]
    pub owner: Owner,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub current: u64,
    /// The vkms seat of a headful slot, once allocated.
    #[serde(default)]
    pub seat: Option<String>,
    /// Set while this slot's session runs on seat0 instead of greetd.
    #[serde(default)]
    pub seat0: Option<Lease>,
    /// Headful: the outputs its vkms card has, left to right. Empty means
    /// one output at the compositor's defaults.
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
    /// The `device` slot only: the machine's own session, as found when
    /// the slot was last loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceSession>,
}

/// The machine's own session, which the `device` slot swaps into: the
/// session a board image runs for its account without a greeter, or on a
/// laptop the owner's Tilo login through greetd on seat0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSession {
    /// The unit that runs it: the board session's system unit, e.g.
    /// `tilo-board-headless-session@tester.service`; the login's
    /// `session-<id>.scope`; or `greetd.service` while the owner is logged
    /// out.
    pub unit: String,
    /// The account it runs as.
    pub account: String,
    /// Units `unit` starts for the session's own processes; they get the
    /// slot's layers too.
    #[serde(default)]
    pub units: Vec<String>,
    /// The compositor's control socket (`TILO_TEST_CONTROL`).
    pub control_socket: PathBuf,
    /// Set for a greetd login: its layers merge system-wide with
    /// `systemd-sysext`, and a swap restarts its desktop in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<Login>,
}

/// The owner's greetd login a laptop's `device` slot is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// The logind session; `None` while the owner is logged out.
    #[serde(default)]
    pub session: Option<String>,
    /// The session's `tilo-session`, which restarts the compositor on
    /// SIGUSR1; `None` while the owner is logged out.
    #[serde(default)]
    pub wrapper: Option<u32>,
}

impl DeviceSession {
    /// This login as it is while the owner is logged out: the layers still
    /// apply, and reach the next login.
    #[must_use]
    pub fn logged_out(mut self) -> Self {
        if self.login.is_some() {
            self.unit = crate::apply::GREETER_UNIT.into();
            self.login = Some(Login::default());
        }
        self
    }
}

impl SlotRecord {
    /// vkms connectors on this slot's card.
    pub fn connectors(&self) -> u32 {
        u32::try_from(self.outputs.len()).unwrap_or(u32::MAX).max(1)
    }
}

impl SlotRecord {
    /// The user the slot's session runs as: `tds-<slot>`, the device
    /// session's account, or none for the `vm` slot.
    pub fn user(&self) -> Option<String> {
        match self.kind {
            SlotKind::Vm => None,
            SlotKind::Device => self.device.as_ref().map(|d| d.account.clone()),
            SlotKind::Headful | SlotKind::Headless => Some(format!("tds-{}", self.name)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub layer: String,
    pub info: ComponentInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub number: u64,
    /// Stacking order, bottom first: the last `ExtensionDirectories=` entry wins.
    pub layers: Vec<String>,
    pub components: BTreeMap<String, Entry>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub reason: String,
}

pub struct SlotStore {
    dir: PathBuf,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl SlotStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join("slots"),
        }
    }

    fn record_path(&self, name: &str) -> PathBuf {
        self.dir.join(name).join("slot.json")
    }

    fn generation_path(&self, name: &str, number: u64) -> PathBuf {
        self.dir
            .join(name)
            .join("generations")
            .join(format!("{number}.json"))
    }

    fn check_name(name: &str) -> Result<(), DaemonError> {
        if name == VM_SLOT || valid_slot_name(name) {
            Ok(())
        } else {
            Err(DaemonError::InvalidName(name.to_owned()))
        }
    }

    /// Create a slot, or return the existing one when it matches.
    ///
    /// # Errors
    /// Invalid or reserved names, outputs on a slot that is not headful,
    /// and a kind or outputs conflicting with an existing slot.
    pub fn create(
        &self,
        name: &str,
        kind: SlotKind,
        owner: Owner,
        outputs: &[OutputSpec],
    ) -> Result<(SlotRecord, bool), DaemonError> {
        if name == VM_SLOT || kind == SlotKind::Vm {
            return Err(DaemonError::ReservedName(name.to_owned()));
        }
        if name == DEVICE_SLOT || kind == SlotKind::Device {
            return Err(DaemonError::DeviceSlot("created".into()));
        }
        Self::check_name(name)?;
        if !outputs.is_empty() {
            if kind != SlotKind::Headful {
                return Err(DaemonError::OutputsNeedHeadful(name.to_owned()));
            }
            tilo_vm_protocol::display::validate_outputs(outputs)
                .map_err(DaemonError::BadOutputs)?;
        }
        if let Some(existing) = self.try_load(name)? {
            if existing.kind != kind {
                return Err(DaemonError::SlotKindConflict {
                    name: name.to_owned(),
                    kind: existing.kind.as_str().into(),
                });
            }
            if existing.outputs != outputs {
                return Err(DaemonError::SlotOutputsConflict {
                    name: name.to_owned(),
                    outputs: if existing.outputs.is_empty() {
                        "one default output".into()
                    } else {
                        format_outputs(&existing.outputs)
                    },
                });
            }
            return Ok((existing, false));
        }
        let record = SlotRecord {
            name: name.to_owned(),
            kind,
            owner,
            created_at: now(),
            current: 0,
            seat: None,
            seat0: None,
            outputs: outputs.to_vec(),
            device: None,
        };
        self.save(&record)?;
        Ok((record, true))
    }

    /// Load a slot; the `vm` pseudo-slot always exists, and the `device`
    /// pseudo-slot exists wherever `device` (the machine's own session) is
    /// given, with that session recorded in it.
    ///
    /// # Errors
    /// [`DaemonError::SlotNotFound`], [`DaemonError::NoDeviceSession`] and
    /// I/O failures.
    pub fn load_with(
        &self,
        name: &str,
        device: Option<DeviceSession>,
    ) -> Result<SlotRecord, DaemonError> {
        Self::check_name(name)?;
        if name == DEVICE_SLOT {
            let stored = self.try_load(name)?;
            // A laptop's slot outlives the owner's login, so a swap that
            // broke the desktop can still be rolled back from the greeter.
            let device = device
                .or_else(|| {
                    let device = stored.as_ref()?.device.clone()?;
                    device.login.is_some().then(|| device.logged_out())
                })
                .ok_or(DaemonError::NoDeviceSession)?;
            let mut record = stored.unwrap_or_else(|| SlotRecord {
                name: DEVICE_SLOT.into(),
                kind: SlotKind::Device,
                owner: Owner::default(),
                created_at: now(),
                current: 0,
                seat: None,
                seat0: None,
                outputs: Vec::new(),
                device: None,
            });
            record.device = Some(device);
            return Ok(record);
        }
        if let Some(record) = self.try_load(name)? {
            return Ok(record);
        }
        if name == VM_SLOT {
            return Ok(SlotRecord {
                name: VM_SLOT.into(),
                kind: SlotKind::Vm,
                owner: Owner::default(),
                created_at: now(),
                current: 0,
                seat: None,
                seat0: None,
                outputs: Vec::new(),
                device: None,
            });
        }
        Err(DaemonError::SlotNotFound(name.to_owned()))
    }

    /// [`Self::load_with`] for a machine without a session of its own.
    ///
    /// # Errors
    /// As [`Self::load_with`].
    pub fn load(&self, name: &str) -> Result<SlotRecord, DaemonError> {
        self.load_with(name, None)
    }

    /// The slot as recorded on disk, if it is.
    ///
    /// # Errors
    /// I/O failures.
    pub fn stored(&self, name: &str) -> Result<Option<SlotRecord>, DaemonError> {
        self.try_load(name)
    }

    fn try_load(&self, name: &str) -> Result<Option<SlotRecord>, DaemonError> {
        match fs::read(self.record_path(name)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| DaemonError::Io {
                    context: format!("parse slot {name}"),
                    source: std::io::Error::other(e),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DaemonError::io(format!("read slot {name}"))(e)),
        }
    }

    /// # Errors
    /// I/O failures.
    pub fn save(&self, record: &SlotRecord) -> Result<(), DaemonError> {
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        write_atomic(&self.record_path(&record.name), &bytes)
    }

    /// Every slot on disk, sorted by name.
    ///
    /// # Errors
    /// I/O failures.
    pub fn list(&self) -> Result<Vec<SlotRecord>, DaemonError> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Ok(out);
        };
        for entry in entries {
            let name = entry
                .map_err(DaemonError::io("list slots"))?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.starts_with('.') {
                continue;
            }
            if let Some(record) = self.try_load(&name)? {
                out.push(record);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Remove a slot's state; `false` when it did not exist.
    ///
    /// # Errors
    /// I/O failures.
    pub fn remove(&self, name: &str) -> Result<bool, DaemonError> {
        let dir = self.dir.join(name);
        if !dir.exists() {
            return Ok(false);
        }
        // Rename first so a half-finished removal is never mistaken for a slot.
        let doomed = self
            .dir
            .join(format!(".rm-{name}-{}", uuid::Uuid::new_v4().simple()));
        fs::rename(&dir, &doomed).map_err(DaemonError::io(format!("remove slot {name}")))?;
        fs::remove_dir_all(&doomed).map_err(DaemonError::io(format!("remove slot {name}")))?;
        Ok(true)
    }

    /// Generation `number` (0 is the empty base).
    ///
    /// # Errors
    /// [`DaemonError::GenerationNotFound`] and I/O failures.
    pub fn generation(&self, name: &str, number: u64) -> Result<Generation, DaemonError> {
        if number == 0 {
            return Ok(Generation::default());
        }
        match fs::read(self.generation_path(name, number)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| DaemonError::Io {
                context: format!("parse generation {number} of {name}"),
                source: std::io::Error::other(e),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(DaemonError::GenerationNotFound {
                    slot: name.to_owned(),
                    generation: number,
                })
            }
            Err(e) => Err(DaemonError::io(format!(
                "read generation {number} of {name}"
            ))(e)),
        }
    }

    /// Recorded generation numbers, ascending.
    pub fn generations(&self, name: &str) -> Vec<u64> {
        let mut numbers: Vec<u64> = fs::read_dir(self.dir.join(name).join("generations"))
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let file = entry.ok()?.file_name().to_string_lossy().into_owned();
                file.strip_suffix(".json")?.parse().ok()
            })
            .collect();
        numbers.sort_unstable();
        numbers
    }

    /// Write the generation that results from putting `layer` in place of
    /// `component`'s current layer. `None` when that is already current.
    /// The slot keeps its current generation until [`Self::set_current`].
    ///
    /// # Errors
    /// I/O failures.
    pub fn prepare_swap(
        &self,
        record: &SlotRecord,
        component: &str,
        layer: &str,
        info: ComponentInfo,
    ) -> Result<Option<(Generation, Generation)>, DaemonError> {
        let from = self.generation(&record.name, record.current)?;
        let mut to = from.clone();
        let entry = Entry {
            layer: layer.to_owned(),
            info,
        };
        match to.components.insert(component.to_owned(), entry.clone()) {
            Some(previous) if previous == entry => return Ok(None),
            Some(previous) => {
                for slot in &mut to.layers {
                    if *slot == previous.layer {
                        layer.clone_into(slot);
                    }
                }
            }
            None => to.layers.push(layer.to_owned()),
        }
        to.layers.dedup();
        to.number = self
            .generations(&record.name)
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        to.created_at = now();
        to.reason = format!("swap {component} {layer}");
        let bytes = serde_json::to_vec_pretty(&to)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        write_atomic(&self.generation_path(&record.name, to.number), &bytes)?;
        Ok(Some((from, to)))
    }

    /// Prepare all admitted members as one generation. This is called while
    /// holding the slot lock, after every import and scope was verified.
    pub(crate) fn prepare_userspace(
        &self,
        record: &SlotRecord,
        members: &[(String, Entry)],
    ) -> Result<(Generation, Generation), DaemonError> {
        self.userspace_generation(record, members, true)
    }

    pub(crate) fn userspace_preview(
        &self,
        record: &SlotRecord,
        members: &[(String, Entry)],
    ) -> Result<Generation, DaemonError> {
        self.userspace_generation(record, members, false)
            .map(|(_, to)| to)
    }

    fn userspace_generation(
        &self,
        record: &SlotRecord,
        members: &[(String, Entry)],
        publish: bool,
    ) -> Result<(Generation, Generation), DaemonError> {
        let from = self.generation(&record.name, record.current)?;
        let mut to = from.clone();
        if let Some(retained) = to.components.remove("runtime-retention") {
            to.layers.retain(|hash| hash != &retained.layer);
        }
        for (name, entry) in members {
            match to.components.insert(name.clone(), entry.clone()) {
                Some(previous) => {
                    for layer in &mut to.layers {
                        if *layer == previous.layer {
                            layer.clone_from(&entry.layer);
                        }
                    }
                }
                None => to.layers.push(entry.layer.clone()),
            }
        }
        to.layers.dedup();
        to.number = self
            .generations(&record.name)
            .last()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| DaemonError::InvalidParams("generation exhausted".into()))?;
        to.created_at = now();
        to.reason = "matched userspace cohort".into();
        let bytes = serde_json::to_vec_pretty(&to)
            .map_err(|error| DaemonError::InvalidParams(error.to_string()))?;
        if publish {
            write_atomic(&self.generation_path(&record.name, to.number), &bytes)?;
        }
        Ok((from, to))
    }

    /// The transition a rollback makes: to `to`, or the newest generation
    /// older than the current one (the base when none is recorded).
    ///
    /// # Errors
    /// Unknown targets, and rolling back from the base.
    pub fn rollback_target(
        &self,
        record: &SlotRecord,
        to: Option<u64>,
    ) -> Result<(Generation, Generation), DaemonError> {
        let from = self.generation(&record.name, record.current)?;
        let target = match to {
            Some(number) => number,
            None if record.current == 0 => {
                return Err(DaemonError::NoEarlierGeneration(record.name.clone()));
            }
            None => self
                .generations(&record.name)
                .into_iter()
                .rev()
                .find(|n| *n < record.current)
                .unwrap_or(0),
        };
        Ok((from, self.generation(&record.name, target)?))
    }

    /// # Errors
    /// I/O failures.
    pub fn set_current(&self, record: &mut SlotRecord, number: u64) -> Result<(), DaemonError> {
        record.current = number;
        self.save(record)?;
        self.clear_pending(&record.name)
    }

    fn pending_path(&self, name: &str) -> PathBuf {
        self.dir.join(name).join("pending")
    }

    /// Record that a transition to `number` is being applied. Cleared by
    /// [`Self::set_current`]; still present afterwards means it was cut short.
    ///
    /// # Errors
    /// I/O failures.
    pub fn mark_pending(&self, name: &str, number: u64) -> Result<(), DaemonError> {
        write_atomic(&self.pending_path(name), number.to_string().as_bytes())
    }

    /// # Errors
    /// I/O failures.
    pub fn clear_pending(&self, name: &str) -> Result<(), DaemonError> {
        match fs::remove_file(self.pending_path(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(DaemonError::io("clear pending")(e))
            }
            _ => Ok(()),
        }
    }

    /// The generation an interrupted transition was heading for.
    pub fn pending(&self, name: &str) -> Option<u64> {
        fs::read_to_string(self.pending_path(name))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Delete generations older than the newest `keep` (the current one, an
    /// interrupted transition's and `also_keep` are always kept) and return
    /// the survivors' layers plus what was deleted.
    ///
    /// # Errors
    /// I/O failures.
    pub fn prune(
        &self,
        record: &SlotRecord,
        keep: u32,
        also_keep: &std::collections::BTreeSet<u64>,
        dry_run: bool,
    ) -> Result<(Vec<String>, Vec<u64>), DaemonError> {
        let numbers = self.generations(&record.name);
        let keep = usize::try_from(keep).unwrap_or(usize::MAX);
        let cutoff = numbers.len().saturating_sub(keep);
        let mut referenced = Vec::new();
        let mut removed = Vec::new();
        for (index, number) in numbers.into_iter().enumerate() {
            let pending = self.pending(&record.name) == Some(number);
            if index >= cutoff || number == record.current || pending || also_keep.contains(&number)
            {
                referenced.extend(self.generation(&record.name, number)?.layers);
            } else {
                if !dry_run {
                    fs::remove_file(self.generation_path(&record.name, number)).map_err(
                        DaemonError::io(format!("prune generation {number} of {}", record.name)),
                    )?;
                }
                removed.push(number);
            }
        }
        Ok((referenced, removed))
    }
}

#[cfg(test)]
mod tests {
    use tilo_vm_protocol::Scope;

    use super::*;

    #[test]
    fn outputs_are_fixed_at_creation_and_need_a_headful_slot() {
        let root = tempfile::tempdir().unwrap();
        let store = SlotStore::new(root.path());
        let two = tilo_vm_protocol::display::parse_outputs("2560x1600@2,1920x1080").unwrap();
        let (record, created) = store
            .create("a", SlotKind::Headful, Owner::default(), &two)
            .unwrap();
        assert!(created);
        assert_eq!(record.connectors(), 2);
        assert_eq!(store.load("a").unwrap().outputs, two);
        let (_, created) = store
            .create("a", SlotKind::Headful, Owner::default(), &two)
            .unwrap();
        assert!(!created);
        let Err(DaemonError::SlotOutputsConflict { outputs, .. }) =
            store.create("a", SlotKind::Headful, Owner::default(), &[])
        else {
            panic!("a different output list must conflict");
        };
        assert_eq!(outputs, "2560x1600@2,1920x1080@1");
        assert!(matches!(
            store.create("b", SlotKind::Headless, Owner::default(), &two),
            Err(DaemonError::OutputsNeedHeadful(_))
        ));
        let (plain, _) = store
            .create("c", SlotKind::Headful, Owner::default(), &[])
            .unwrap();
        assert_eq!(plain.connectors(), 1);
    }

    fn session() -> ComponentInfo {
        ComponentInfo::new(Scope::Session, vec![])
    }

    fn swap(
        store: &SlotStore,
        record: &mut SlotRecord,
        component: &str,
        layer: &str,
    ) -> Option<u64> {
        let (_, to) = store
            .prepare_swap(record, component, layer, session())
            .unwrap()?;
        store.set_current(record, to.number).unwrap();
        Some(to.number)
    }

    #[test]
    fn frame_cpu_trace_changes_generation_for_the_same_layer() {
        let root = tempfile::tempdir().unwrap();
        let store = SlotStore::new(root.path());
        let (mut record, _) = store
            .create("cpu", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        assert_eq!(swap(&store, &mut record, "shell", "h1"), Some(1));
        let mut info = session();
        info.frame_cpu_trace = true;
        let (from, to) = store
            .prepare_swap(&record, "shell", "h1", info.clone())
            .unwrap()
            .unwrap();
        assert!(!from.components["shell"].info.frame_cpu_trace);
        assert!(to.components["shell"].info.frame_cpu_trace);
        assert_eq!(to.number, 2);
        store.set_current(&mut record, to.number).unwrap();
        assert!(
            store
                .prepare_swap(&record, "shell", "h1", info)
                .unwrap()
                .is_none()
        );
        let (_, disabled) = store
            .prepare_swap(&record, "shell", "h1", session())
            .unwrap()
            .unwrap();
        assert!(!disabled.components["shell"].info.frame_cpu_trace);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn slot_generations_swap_roll_back_and_mark_interruptions() {
        // Phase `create_is_idempotent_and_checks_kind`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            let (_, created) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            assert!(created);
            let (_, created) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            assert!(!created);
            assert!(matches!(
                store.create("a", SlotKind::Headful, Owner::default(), &[]),
                Err(DaemonError::SlotKindConflict { .. })
            ));
            assert!(matches!(
                store.create("vm", SlotKind::Headless, Owner::default(), &[]),
                Err(DaemonError::ReservedName(_))
            ));
            assert!(matches!(
                store.create("Bad", SlotKind::Headless, Owner::default(), &[]),
                Err(DaemonError::InvalidName(_))
            ));
            assert!(matches!(
                store.load("nope"),
                Err(DaemonError::SlotNotFound(_))
            ));
            assert_eq!(store.load("vm").unwrap().kind, SlotKind::Vm);
        }
        // Phase `swaps_make_generations_in_order`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            let (mut record, _) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            assert_eq!(swap(&store, &mut record, "shell", "h1"), Some(1));
            assert_eq!(swap(&store, &mut record, "launcher", "h2"), Some(2));
            // Same layer again: no new generation.
            assert_eq!(swap(&store, &mut record, "shell", "h1"), None);
            // Replacing keeps the stacking position.
            assert_eq!(swap(&store, &mut record, "shell", "h3"), Some(3));
            let current = store.generation("a", 3).unwrap();
            assert_eq!(current.layers, ["h3", "h2"]);
            assert_eq!(current.components["shell"].layer, "h3");
            assert_eq!(store.generations("a"), [1, 2, 3]);
            assert_eq!(store.load("a").unwrap().current, 3);
        }
        // Phase `unapplied_swap_does_not_move_current`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            let (record, _) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            let (from, to) = store
                .prepare_swap(&record, "shell", "h1", session())
                .unwrap()
                .unwrap();
            assert_eq!(from.number, 0);
            assert_eq!(to.number, 1);
            assert_eq!(store.load("a").unwrap().current, 0);
            // A retry sees the same transition again rather than "unchanged".
            assert!(
                store
                    .prepare_swap(&record, "shell", "h1", session())
                    .unwrap()
                    .is_some()
            );
        }
        // Phase `rollback_walks_back_to_base`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            let (mut record, _) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            swap(&store, &mut record, "shell", "h1");
            swap(&store, &mut record, "shell", "h2");
            let (from, to) = store.rollback_target(&record, None).unwrap();
            assert_eq!((from.number, to.number), (2, 1));
            store.set_current(&mut record, to.number).unwrap();
            // A swap after a rollback gets a fresh number; history is never rewritten.
            assert_eq!(swap(&store, &mut record, "shell", "h4"), Some(3));
            assert_eq!(store.generation("a", 3).unwrap().layers, ["h4"]);
            let (_, to) = store.rollback_target(&record, None).unwrap();
            assert_eq!(to.number, 2);
            let (_, base) = store.rollback_target(&record, Some(0)).unwrap();
            assert!(base.layers.is_empty());
            store.set_current(&mut record, 0).unwrap();
            assert!(matches!(
                store.rollback_target(&record, None),
                Err(DaemonError::NoEarlierGeneration(_))
            ));
            assert!(matches!(
                store.rollback_target(&record, Some(9)),
                Err(DaemonError::GenerationNotFound { .. })
            ));
        }
        // Phase `pending_marks_interrupted_transitions`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            let (mut record, _) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            store.mark_pending("a", 1).unwrap();
            assert_eq!(store.pending("a"), Some(1));
            store.set_current(&mut record, 0).unwrap();
            assert_eq!(store.pending("a"), None);
        }
    }

    /// A laptop's device slot outlives the owner's login, so a swap that
    /// broke the desktop is rolled back from the login screen; a board's
    /// slot exists only while its session does.
    #[test]
    fn a_logins_device_slot_stays_while_the_owner_is_logged_out() {
        let root = tempfile::tempdir().unwrap();
        let store = SlotStore::new(root.path());
        let login = DeviceSession {
            unit: "session-3.scope".into(),
            account: "tester".into(),
            units: Vec::new(),
            control_socket: PathBuf::from("/run/user/1000/tilo-session/test-control.sock"),
            login: Some(Login {
                session: Some("3".into()),
                wrapper: Some(4242),
            }),
        };
        assert!(matches!(
            store.load_with(DEVICE_SLOT, None),
            Err(DaemonError::NoDeviceSession)
        ));
        let record = store.load_with(DEVICE_SLOT, Some(login.clone())).unwrap();
        store.save(&record).unwrap();
        let out = store.load_with(DEVICE_SLOT, None).unwrap();
        let device = out.device.unwrap();
        assert_eq!(device.unit, crate::apply::GREETER_UNIT);
        assert_eq!(device.login, Some(Login::default()));
        assert_eq!(device.account, "tester");

        let board = DeviceSession {
            login: None,
            unit: "tilo-board-headless-session@tester.service".into(),
            ..login
        };
        let record = store.load_with(DEVICE_SLOT, Some(board)).unwrap();
        store.save(&record).unwrap();
        assert!(matches!(
            store.load_with(DEVICE_SLOT, None),
            Err(DaemonError::NoDeviceSession)
        ));
    }

    #[test]
    fn slot_pruning_and_removal_keep_what_is_current() {
        // Phase `prune_keeps_current_and_newest`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            let (mut record, _) = store
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            for layer in ["h1", "h2", "h3", "h4"] {
                swap(&store, &mut record, "shell", layer);
            }
            store.set_current(&mut record, 1).unwrap();
            let (referenced, removed) = store
                .prune(&record, 2, &std::collections::BTreeSet::new(), true)
                .unwrap();
            assert_eq!(removed, [2]);
            assert_eq!(
                store.generations("a"),
                [1, 2, 3, 4],
                "dry run deletes nothing"
            );
            let (_, removed) = store
                .prune(&record, 2, &std::collections::BTreeSet::new(), false)
                .unwrap();
            assert_eq!(removed, [2]);
            assert_eq!(store.generations("a"), [1, 3, 4]);
            assert!(
                referenced.contains(&"h1".to_string()) && !referenced.contains(&"h2".to_string())
            );
        }
        // Phase `remove_is_idempotent`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = SlotStore::new(root.path());
            store
                .create("a", SlotKind::Headful, Owner::default(), &[])
                .unwrap();
            assert!(store.remove("a").unwrap());
            assert!(!store.remove("a").unwrap());
            assert!(store.list().unwrap().is_empty());
        }
    }
}
