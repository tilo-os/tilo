//! Atomic, local shell replay selection. The sole workspace owner serializes all
//! continuations. Source files are trusted authenticated-ingress records, not
//! independently authenticated by their outer (often zero) Envelope signature.
//! Unknown data never grants retirement. Only exact historical proof pairs do.
use super::{
    StoreError, WorkspaceStore,
    bounded::{self, Meta, Sorter, Step, WorkLimit, WorkStats, invalid, read_meta, write_meta},
    replay, retained,
};
use crate::coedit::CoeditPayload;
use qos_peer_state::{
    NeighbourMsg,
    coverage::{Coverage, route_key},
    retained_proof::*,
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Seek,
    path::{Path, PathBuf},
};

mod selected;
use selected::SelectedGc;
pub(crate) use selected::SelectedRead;
#[cfg(test)]
mod tests;

#[cfg(test)]
static SELECTION_CONSTRUCTIONS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    >,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn track_selection_constructions(
    root: &Path,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    SELECTION_CONSTRUCTIONS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), observed.clone());
    observed
}

#[cfg(test)]
pub(crate) fn selection_constructions(root: &Path) -> usize {
    SELECTION_CONSTRUCTIONS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(root)
        .map_or(0, |count| count.load(std::sync::atomic::Ordering::SeqCst))
}

pub(crate) const DIRECTORY: &str = "shell-selected-v1";
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Publication {
    Original,
    Suppress,
    Activate(PathBuf),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionOutcome {
    Published(Publication),
    BatchPublished {
        publication: Publication,
        dispositions: Vec<IncomingDisposition>,
    },
    UnresolvedTailDependencies,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncomingDisposition {
    Covered,
    Staged,
    SelectedLive,
    Original,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchSelectionOutcome {
    pub publication: Publication,
    pub dispositions: Vec<IncomingDisposition>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetainOutcome {
    pub written: u64,
    pub publication: Publication,
}

/// Identity of the authenticated retained input being classified. Transport time
/// is irrelevant, but another source/route/kind cannot inherit its disposition.
fn input_identity(source: [u8; 32], channel: [u8; 32], checkpoint: bool, body: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(&source);
    hash.update(&channel);
    hash.update(&[u8::from(checkpoint)]);
    hash.update(body);
    *hash.finalize().as_bytes()
}
pub(crate) fn incoming_identity(bytes: &[u8], workspace: u64) -> Option<[u8; 32]> {
    let (source, payload) = retained::payload(bytes, workspace)?;
    Some(match payload {
        CoeditPayload::Checkpoint { channel, body } => input_identity(source, channel, true, &body),
        CoeditPayload::RetainedDelta { channel, body } => {
            input_identity(source, channel, false, &body)
        }
    })
}

pub(super) fn frame(path: &Path, workspace: u64) -> Result<Option<Frame>, StoreError> {
    let Some(bytes) = replay::frame_bytes(path)? else {
        return Ok(None);
    };
    let Some((source, payload)) = retained::payload(&bytes, workspace) else {
        return Ok(None);
    };
    let (channel, body, kind) = match payload {
        CoeditPayload::Checkpoint { channel, body } => (channel, body, RoomInput::Checkpoint),
        CoeditPayload::RetainedDelta { channel, body } => (channel, body, RoomInput::Delta),
    };
    Ok(Some(Frame {
        source,
        channel,
        body,
        kind,
    }))
}
pub(super) struct Frame {
    pub source: [u8; 32],
    pub channel: [u8; 32],
    pub body: Vec<u8>,
    pub kind: RoomInput,
}
impl Frame {
    fn route(&self) -> Option<(String, u64)> {
        let (
            NeighbourMsg::AppDocument {
                app_id,
                record_id,
                envelope,
                ..
            },
            rest,
        ) = postcard::take_from_bytes(&self.body).ok()?
        else {
            return None;
        };
        rest.is_empty()
            .then(|| (route_key(&app_id, &record_id), envelope.seq))
    }
    fn room(&self) -> bool {
        self.channel == *blake3::hash(b"shell-tree").as_bytes()
    }
}
#[derive(Clone)]
struct Route {
    coverage: Coverage,
    file: Option<Meta>,
}
#[derive(Default, Clone)]
struct Basis {
    room: Option<VerifiedRoom>,
    routes: BTreeMap<String, Route>,
}
impl Basis {
    fn complete(&self) -> bool {
        self.room.is_some() && self.routes.values().all(|r| r.file.is_some())
    }
    fn set_room(&mut self, room: VerifiedRoom) -> Result<(), StoreError> {
        self.routes = room
            .coverages()
            .map_err(|_| invalid("invalid verified room coverage"))?
            .into_iter()
            .map(|(k, coverage)| {
                (
                    k,
                    Route {
                        coverage,
                        file: None,
                    },
                )
            })
            .collect();
        self.room = Some(room);
        Ok(())
    }
}

enum Phase {
    Old,
    Scan,
    Sort,
    Rooms,
    Pair,
    Check,
    Historical,
    SortExtrasInput,
    SortExtras,
    ValidateExtras,
    StageRoom,
    StageApps,
    StageExtras,
    StageRetire,
    Commit,
    Gc,
    Done,
}
/// History metadata spills to disk. At most the current route map, a constant number of room
/// documents, one successor and one prior frame are resident; no history cache.
pub(crate) struct Selection {
    store: WorkspaceStore,
    scratch: PathBuf,
    authority: RetentionAuthority,
    old_read: SelectedRead,
    old: Basis,
    basis: Basis,
    phase: Phase,
    outer: Option<fs::ReadDir>,
    child: Option<fs::ReadDir>,
    active_lineage: Option<std::ffi::OsString>,
    old_scan: Option<File>,
    sorter: Sorter,
    inventory: Option<PathBuf>,
    input: Option<File>,
    extras_sorter: Option<Sorter>,
    sorted_extras: Option<PathBuf>,
    extras_ordinal: u64,
    last_tail: Option<Meta>,
    tail_readiness: Option<(String, OrderedTailReadiness)>,
    room_list: File,
    extras: File,
    retire: File,
    historical: Option<(Meta, Frame, File, Option<VerifiedRoom>)>,
    prospective: Option<VerifiedRoom>,
    fallback: bool,
    blocked: bool,
    staged: PathBuf,
    output: File,
    live_output: File,
    ordinal: u64,
    route_iter: Option<std::collections::btree_map::IntoIter<String, Route>>,
    collect: Option<retained::GenerationGc>,
    active_gc: Option<SelectedGc>,
    gc_output: File,
    changed: bool,
    switch_attempted: bool,
    incoming: Vec<[u8; 32]>,
    incoming_dispositions: Vec<Option<IncomingDisposition>>,
    lineage_overrides: BTreeMap<std::ffi::OsString, PathBuf>,
    batch: bool,
    incoming_staged: bool,
}
impl Selection {
    pub(crate) fn new(
        store: &WorkspaceStore,
        scratch: PathBuf,
        incoming: Option<[u8; 32]>,
    ) -> Result<Self, StoreError> {
        Self::new_inner(
            store,
            scratch,
            incoming.into_iter().collect(),
            BTreeMap::new(),
            false,
        )
    }
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "tests exercise cumulative checkpoint selection without projected lineage overrides"
        )
    )]
    pub(crate) fn new_batch(
        store: &WorkspaceStore,
        scratch: PathBuf,
        incoming: Vec<[u8; 32]>,
    ) -> Result<Self, StoreError> {
        Self::new_inner(store, scratch, incoming, BTreeMap::new(), true)
    }
    pub(crate) fn new_projected(
        store: &WorkspaceStore,
        scratch: PathBuf,
        incoming: Vec<[u8; 32]>,
        lineage_overrides: BTreeMap<std::ffi::OsString, PathBuf>,
    ) -> Result<Self, StoreError> {
        Self::new_inner(store, scratch, incoming, lineage_overrides, true)
    }
    fn new_inner(
        store: &WorkspaceStore,
        scratch: PathBuf,
        incoming: Vec<[u8; 32]>,
        lineage_overrides: BTreeMap<std::ffi::OsString, PathBuf>,
        batch: bool,
    ) -> Result<Self, StoreError> {
        #[cfg(test)]
        if let Some(count) = SELECTION_CONSTRUCTIONS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .get(store.root())
        {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let incoming_dispositions = vec![None; incoming.len()];
        fs::create_dir_all(&scratch)?;
        let old_read =
            SelectedRead::new(store)?.ok_or_else(|| invalid("unbound shell selector"))?;
        let old_scan = retained::current(&store.root.join(DIRECTORY))?
            .map(|p| File::open(p.join("basis.manifest")))
            .transpose()?;
        let dir = store.root.join(DIRECTORY);
        retained::create_dir(&dir)?;
        let staged = dir.join(hex::encode(rand::random::<[u8; 16]>()));
        retained::create_dir(&staged)?;
        Ok(Self {
            store: store.clone(),
            authority: store.shell_authority().unwrap().clone(),
            old_read,
            old: Basis::default(),
            basis: Basis::default(),
            phase: Phase::Old,
            outer: None,
            child: None,
            active_lineage: None,
            old_scan,
            sorter: Sorter::new(scratch.join("shell-sort"))?,
            inventory: None,
            input: None,
            extras_sorter: None,
            sorted_extras: None,
            extras_ordinal: 0,
            last_tail: None,
            tail_readiness: None,
            room_list: File::create(scratch.join("room.manifest"))?,
            extras: File::create(scratch.join("extras.manifest"))?,
            retire: File::create(scratch.join("retire.manifest"))?,
            historical: None,
            prospective: None,
            fallback: false,
            blocked: false,
            output: File::create(staged.join("basis.manifest"))?,
            live_output: File::create(staged.join("live.manifest"))?,
            staged: staged.clone(),
            ordinal: 0,
            route_iter: None,
            collect: None,
            active_gc: None,
            gc_output: File::create(staged.join("gc.manifest"))?,
            changed: false,
            switch_attempted: false,
            incoming,
            incoming_dispositions,
            lineage_overrides,
            batch,
            incoming_staged: false,
            scratch,
        })
    }
    /// A complete changed cut may already be visible if CURRENT's rename or
    /// directory barrier reports an error. The owner must fence its captured
    /// recipients on any later failure, even before an Activate result exists.
    pub(crate) fn activation_on_error(&self) -> Option<PathBuf> {
        (self.changed && self.switch_attempted).then(|| self.staged.clone())
    }
    pub(crate) fn cleanup_paths(&self) -> (PathBuf, PathBuf) {
        (self.store.root.join(DIRECTORY), self.staged.clone())
    }
    pub(crate) fn stats(&self) -> &WorkStats {
        &self.sorter.stats
    }
    fn batch_outcome(&self) -> BatchSelectionOutcome {
        BatchSelectionOutcome {
            publication: if self.changed {
                Publication::Activate(self.staged.clone())
            } else if self.incoming_staged
                || (self.batch
                    && self.incoming_dispositions.iter().all(|value| {
                        value.is_some_and(|value| value != IncomingDisposition::Original)
                    }))
            {
                Publication::Suppress
            } else {
                Publication::Original
            },
            dispositions: self
                .incoming_dispositions
                .iter()
                .map(|value| value.unwrap_or(IncomingDisposition::Original))
                .collect(),
        }
    }
    /// Finish the projected manifests without switching selected CURRENT.
    /// The caller must promote every projected lineage before resuming `step`.
    pub(crate) fn step_projection(
        &mut self,
        limit: WorkLimit,
    ) -> Result<Step<BatchSelectionOutcome>, StoreError> {
        if matches!(self.phase, Phase::Commit) {
            return Ok(Step::Done(self.batch_outcome()));
        }
        match self.step(limit)? {
            Step::More => Ok(Step::More),
            Step::Done(SelectionOutcome::UnresolvedTailDependencies) => {
                Ok(Step::Done(BatchSelectionOutcome {
                    publication: Publication::Original,
                    dispositions: vec![IncomingDisposition::Original; self.incoming.len()],
                }))
            }
            Step::Done(_) => Err(invalid("projected selection committed before promotion")),
        }
    }
    fn restart(&mut self, phase: Phase) -> Result<(), StoreError> {
        self.input = Some(File::open(self.inventory.as_ref().unwrap())?);
        self.phase = phase;
        Ok(())
    }
    fn read(&mut self, meta: &Meta) -> Result<Option<Frame>, StoreError> {
        bounded::increment(&mut self.sorter.stats.preparation_reads)?;
        self.sorter.stats.peak_frame_bytes =
            self.sorter.stats.peak_frame_bytes.max(meta.len as usize);
        frame(&self.store.root.join(&meta.source), self.store.workspace_id)
    }
    fn add(&mut self, meta: &Meta, role: &str) -> Result<(), StoreError> {
        let name = format!("{:016x}.delta", self.ordinal);
        bounded::increment(&mut self.ordinal)?;
        let path = self.staged.join(name);
        // Every source belongs to an already durable retained generation. The
        // Commit phase syncs this hard-link directory before switching CURRENT.
        fs::hard_link(self.store.root.join(&meta.source), &path)?;
        let mut selected = meta.clone();
        selected.source = path.strip_prefix(&self.store.root).unwrap().into();
        selected.key = vec![role.into()];
        write_meta(&mut self.output, &selected)?;
        let live = if role == "room" {
            let value = self
                .read(meta)?
                .ok_or_else(|| invalid("invalid live prerequisite"))?;
            self.note_incoming(&value, IncomingDisposition::SelectedLive);
            self.old.room.as_ref().is_none_or(|room| {
                room.covers_frame(value.source, value.channel, &value.body)
                    != CoverageDecision::Covered
            })
        } else if role == "app" {
            let value = self
                .read(meta)?
                .ok_or_else(|| invalid("invalid live application"))?;
            self.note_incoming(&value, IncomingDisposition::SelectedLive);
            let key = value
                .route()
                .ok_or_else(|| invalid("invalid live application route"))?
                .0;
            self.old.routes.get(&key).map(|r| &r.coverage)
                != self.basis.routes.get(&key).map(|r| &r.coverage)
        } else {
            role == "tail" && self.old.room.is_none()
        };
        if live {
            write_meta(&mut self.live_output, &selected)?
        }
        Ok(())
    }
    fn note_incoming(&mut self, value: &Frame, disposition: IncomingDisposition) {
        let identity = input_identity(
            value.source,
            value.channel,
            value.kind == RoomInput::Checkpoint,
            &value.body,
        );
        for (index, incoming) in self.incoming.iter().enumerate() {
            if incoming != &identity {
                continue;
            }
            let slot = &mut self.incoming_dispositions[index];
            let replace = match (*slot, disposition) {
                (None, _) => true,
                (Some(IncomingDisposition::Staged), IncomingDisposition::Covered)
                | (Some(IncomingDisposition::Staged), IncomingDisposition::SelectedLive)
                | (Some(IncomingDisposition::Covered), IncomingDisposition::SelectedLive) => true,
                _ => false,
            };
            if replace {
                *slot = Some(disposition);
            }
        }
    }
    fn note_staged(&mut self, value: &Frame) {
        self.note_incoming(value, IncomingDisposition::Staged);
        if self.incoming.iter().any(|incoming| {
            incoming
                == &input_identity(
                    value.source,
                    value.channel,
                    value.kind == RoomInput::Checkpoint,
                    &value.body,
                )
        }) {
            self.incoming_staged = true
        }
    }
    fn stage_or_preserve_extra(&mut self, meta: &Meta, value: &Frame) -> Result<(), StoreError> {
        if self.fallback {
            self.note_incoming(value, IncomingDisposition::Original);
            self.extra(meta)
        } else {
            self.note_staged(value);
            Ok(())
        }
    }
    fn extra(&mut self, meta: &Meta) -> Result<(), StoreError> {
        let mut extra = meta.clone();
        extra.key = vec!["0".into(), format!("{:016x}", self.extras_ordinal).into()];
        bounded::increment(&mut self.extras_ordinal)?;
        write_meta(&mut self.extras, &extra)
    }
    fn tail(&mut self, meta: &Meta, value: &Frame, seq: u64) -> Result<(), StoreError> {
        let mut extra = meta.clone();
        extra.key = vec![
            "1".into(),
            hex::encode(value.channel).into(),
            hex::encode(value.source).into(),
            format!("{seq:016x}").into(),
        ];
        write_meta(&mut self.extras, &extra)
    }
    fn covered(&mut self, meta: &Meta, value: &Frame) -> Result<(), StoreError> {
        self.note_incoming(value, IncomingDisposition::Covered);
        // Only v2 immutable originals are eligible. Selected-generation files
        // are collected separately after replacement; legacy files stay intact.
        if meta.source.starts_with("retained/v2") {
            write_meta(&mut self.retire, meta)?
        }
        Ok(())
    }
    fn candidate_snapshot(&mut self, key: &str) -> Result<Option<VerifiedSnapshot>, StoreError> {
        let Some(meta) = self.basis.routes.get(key).and_then(|r| r.file.clone()) else {
            return Ok(None);
        };
        let value = self
            .read(&meta)?
            .ok_or_else(|| invalid("selected route vanished"))?;
        Ok(verify_snapshot(
            self.basis.room.as_ref().unwrap(),
            value.source,
            value.channel,
            &value.body,
        )
        .ok())
    }
    pub(crate) fn step(&mut self, limit: WorkLimit) -> Result<Step<SelectionOutcome>, StoreError> {
        let limit = limit.checked()?;
        self.sorter.stats.last_step_work = 1;
        match self.phase {
            Phase::Old => {
                if let Step::Done(()) = self.old_read.step()? {
                    self.old = std::mem::take(&mut self.old_read.basis);
                    self.phase = Phase::Scan
                }
            }
            Phase::Scan => {
                if self.sorter.spilling() {
                    self.sorter.spill_step(limit)?;
                    return Ok(Step::More);
                }
                if let Some(file) = &mut self.old_scan {
                    if let Some(mut meta) = read_meta(file)? {
                        if meta
                            .key
                            .first()
                            .is_some_and(|v| v == "extra" || v == "tail")
                        {
                            return Ok(Step::More);
                        }
                        meta.key = vec!["0".into(), format!("{:016x}", self.ordinal).into()];
                        bounded::increment(&mut self.ordinal)?;
                        self.sorter.push(meta)?;
                        return Ok(Step::More);
                    }
                    self.old_scan = None;
                }
                if self.outer.is_none() {
                    match fs::read_dir(self.store.root.join("retained/v2")) {
                        Ok(d) => self.outer = Some(d),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            self.phase = Phase::Sort;
                            return Ok(Step::More);
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                if let Some(child) = &mut self.child {
                    if let Some(entry) = child.next() {
                        let entry = entry?;
                        let name = entry.file_name();
                        if entry.file_type()?.is_file()
                            && name.to_string_lossy().ends_with(".delta")
                            && !name.to_string_lossy().starts_with('.')
                        {
                            let path = entry.path();
                            let lineage = self
                                .active_lineage
                                .as_ref()
                                .expect("active retained lineage")
                                .clone();
                            self.sorter.push(Meta {
                                key: vec![
                                    "1".into(),
                                    lineage,
                                    if name == "checkpoint.delta" {
                                        "0".into()
                                    } else {
                                        "1".into()
                                    },
                                    name,
                                ],
                                source: path.strip_prefix(&self.store.root).unwrap().into(),
                                pin: 0,
                                len: entry.metadata()?.len(),
                                pending: false,
                            })?;
                        }
                    } else {
                        self.child = None;
                        self.active_lineage = None;
                    }
                } else if let Some(entry) = self.outer.as_mut().unwrap().next() {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        let active = self
                            .lineage_overrides
                            .get(&entry.file_name())
                            .cloned()
                            .or(retained::current(&entry.path())?);
                        if let Some(active) = active {
                            self.child = Some(fs::read_dir(active)?);
                            self.active_lineage = Some(entry.file_name());
                        }
                    }
                } else {
                    self.phase = Phase::Sort
                }
            }
            Phase::Sort => {
                if let Step::Done(path) = self.sorter.step(limit)? {
                    self.inventory = Some(path);
                    self.ordinal = 0;
                    self.restart(Phase::Rooms)?
                }
            }
            Phase::Rooms => {
                if let Some(meta) = read_meta(self.input.as_mut().unwrap())? {
                    if let Some(value) = self.read(&meta)? {
                        if value.room() {
                            if self.basis.room.as_ref().is_some_and(|r| {
                                r.covers_frame(value.source, value.channel, &value.body)
                                    == CoverageDecision::Covered
                            }) {
                                return Ok(Step::More);
                            }
                            if let Ok(room) = verify_room(
                                &self.authority,
                                value.source,
                                value.channel,
                                &value.body,
                                value.kind,
                                self.basis.room.as_ref(),
                            ) {
                                if value.kind == RoomInput::Checkpoint {
                                    self.room_list.set_len(0)?;
                                    self.room_list.rewind()?;
                                }
                                write_meta(&mut self.room_list, &meta)?;
                                self.basis.set_room(room)?;
                            }
                        }
                    }
                } else {
                    self.restart(Phase::Pair)?
                }
            }
            Phase::Pair => {
                if let Some(meta) = read_meta(self.input.as_mut().unwrap())? {
                    if let Some(value) = self.read(&meta)? {
                        if let Some(room) = &self.basis.room {
                            if let Ok(proof) =
                                verify_snapshot(room, value.source, value.channel, &value.body)
                            {
                                let key =
                                    route_key(&proof.coverage().app, &proof.coverage().record);
                                let route = self.basis.routes.get_mut(&key).unwrap();
                                if route.file.is_none() {
                                    route.file = Some(meta)
                                }
                            }
                        }
                    }
                } else {
                    self.prospective = self.basis.room.clone();
                    if !self.basis.complete() {
                        self.basis = self.old.clone();
                        self.fallback = true;
                        self.incoming_dispositions.fill(None);
                        self.incoming_staged = false;
                    }
                    self.restart(Phase::Check)?;
                }
            }
            Phase::Check => {
                let Some(meta) = read_meta(self.input.as_mut().unwrap())? else {
                    if self.blocked && !self.fallback {
                        self.basis = self.old.clone();
                        self.fallback = true;
                        self.incoming_dispositions.fill(None);
                        self.incoming_staged = false;
                        self.blocked = false;
                        self.extras.set_len(0)?;
                        self.extras.rewind()?;
                        self.retire.set_len(0)?;
                        self.retire.rewind()?;
                        self.restart(Phase::Check)?;
                        return Ok(Step::More);
                    }
                    self.changed = !self.fallback
                        && self.basis.room.as_ref().map(VerifiedRoom::heads)
                            != self.old.room.as_ref().map(VerifiedRoom::heads);
                    self.input = Some(File::open(self.scratch.join("extras.manifest"))?);
                    let sort = if self.fallback {
                        "extras-sort-fallback"
                    } else {
                        "extras-sort"
                    };
                    self.extras_sorter = Some(Sorter::new(self.scratch.join(sort))?);
                    self.phase = Phase::SortExtrasInput;
                    return Ok(Step::More);
                };
                let Some(value) = self.read(&meta)? else {
                    self.blocked = true;
                    self.extra(&meta)?;
                    return Ok(Step::More);
                };
                if value.room() {
                    if self.basis.room.as_ref().is_some_and(|r| {
                        r.covers_frame(value.source, value.channel, &value.body)
                            == CoverageDecision::Covered
                    }) {
                        self.covered(&meta, &value)?
                    } else if verify_room(
                        &self.authority,
                        value.source,
                        value.channel,
                        &value.body,
                        value.kind,
                        self.basis.room.as_ref(),
                    )
                    .is_ok()
                    {
                        self.stage_or_preserve_extra(&meta, &value)?
                    } else {
                        self.blocked = true;
                        self.extra(&meta)?
                    }
                    return Ok(Step::More);
                }
                let Some((key, seq)) = value.route() else {
                    self.extra(&meta)?;
                    return Ok(Step::More);
                };
                let snapshot_result =
                    self.basis.room.as_ref().map(|room| {
                        verify_snapshot(room, value.source, value.channel, &value.body)
                    });
                if matches!(snapshot_result, Some(Ok(_))) {
                    self.covered(&meta, &value)?;
                    return Ok(Step::More);
                }
                if matches!(snapshot_result, Some(Err(ProofError::Unpaired))) {
                    if self
                        .basis
                        .routes
                        .get(&key)
                        .is_none_or(|r| seq > r.coverage.sequence)
                    {
                        self.stage_or_preserve_extra(&meta, &value)?;
                        return Ok(Step::More);
                    }
                    self.historical = Some((
                        meta,
                        value,
                        File::open(self.inventory.as_ref().unwrap())?,
                        None,
                    ));
                    self.phase = Phase::Historical;
                    return Ok(Step::More);
                }
                if self.basis.room.is_none() {
                    // No usable pair yet. A room proof can still establish valid
                    // staging; without one this remains an opaque original.
                    if let Some(room) = &self.prospective {
                        if !matches!(
                            verify_snapshot(room, value.source, value.channel, &value.body),
                            Err(ProofError::Unknown)
                        ) {
                            self.stage_or_preserve_extra(&meta, &value)?;
                            return Ok(Step::More);
                        }
                    }
                }
                if let Some(snapshot) = self.candidate_snapshot(&key)? {
                    let decision = snapshot.classify_tail(value.source, value.channel, &value.body);
                    if decision.coverage == CoverageDecision::Covered {
                        self.covered(&meta, &value)?;
                        return Ok(Step::More);
                    }
                    // Floor-safe tails are validated cumulatively only after the
                    // bounded sorter establishes exact route/author sequence order.
                    if decision.floor == FloorSafety::Safe {
                        self.tail(&meta, &value, seq)?;
                        return Ok(Step::More);
                    }
                    if decision.floor != FloorSafety::Unknown
                        && (self.fallback || self.old.room.is_none())
                    {
                        return Ok(Step::Done(SelectionOutcome::UnresolvedTailDependencies));
                    }
                    self.blocked = true;
                } else {
                    self.blocked = true;
                }
                self.extra(&meta)?;
            }
            Phase::Historical => {
                let (meta, target, mut file, mut prior) = self.historical.take().unwrap();
                let mut covered = false;
                if let Some(candidate) = read_meta(&mut file)? {
                    if let Some(value) = self.read(&candidate)? {
                        if value.room() {
                            let previous = if value.kind == RoomInput::Checkpoint {
                                None
                            } else {
                                prior.as_ref()
                            };
                            if let Ok(room) = verify_room(
                                &self.authority,
                                value.source,
                                value.channel,
                                &value.body,
                                value.kind,
                                previous,
                            ) {
                                if let Ok(old) = verify_snapshot(
                                    &room,
                                    target.source,
                                    target.channel,
                                    &target.body,
                                ) {
                                    if let Some(next) =
                                        self.candidate_snapshot(&target.route().unwrap().0)?
                                    {
                                        covered = next.covers_snapshot(&old)
                                    }
                                }
                                prior = Some(room);
                            }
                        }
                    }
                    if covered {
                        self.covered(&meta, &target)?;
                        self.phase = Phase::Check
                    } else {
                        self.historical = Some((meta, target, file, prior));
                    }
                } else {
                    self.blocked = true;
                    self.extra(&meta)?;
                    self.phase = Phase::Check
                }
            }
            Phase::SortExtrasInput => {
                let sorter = self.extras_sorter.as_mut().unwrap();
                if sorter.spilling() {
                    sorter.spill_step(limit)?;
                    return Ok(Step::More);
                }
                if let Some(meta) = read_meta(self.input.as_mut().unwrap())? {
                    sorter.push(meta)?
                } else {
                    self.phase = Phase::SortExtras
                }
            }
            Phase::SortExtras => {
                let sorter = self.extras_sorter.as_mut().unwrap();
                let result = sorter.step(limit)?;
                self.sorter.stats.last_step_work = sorter.stats.last_step_work;
                self.sorter.stats.peak_metadata_entries = self
                    .sorter
                    .stats
                    .peak_metadata_entries
                    .max(sorter.stats.peak_metadata_entries);
                self.sorter.stats.peak_metadata_bytes = self.sorter.stats.peak_metadata_bytes.max(
                    sorter.stats.peak_metadata_bytes
                        + bounded::RUN_ENTRIES * std::mem::size_of::<Meta>(),
                );
                self.sorter.stats.peak_open_files = self
                    .sorter
                    .stats
                    .peak_open_files
                    .max(sorter.stats.peak_open_files + 5);
                if let Step::Done(path) = result {
                    self.input = Some(File::open(&path)?);
                    self.sorted_extras = Some(path);
                    self.last_tail = None;
                    self.tail_readiness = None;
                    self.phase = Phase::ValidateExtras;
                }
            }
            Phase::ValidateExtras => {
                let Some(meta) = read_meta(self.input.as_mut().unwrap())? else {
                    self.last_tail = None;
                    self.tail_readiness = None;
                    self.input = if self.fallback {
                        retained::current(&self.store.root.join(DIRECTORY))?
                            .map(|p| File::open(p.join("basis.manifest")))
                            .transpose()?
                    } else {
                        Some(File::open(self.scratch.join("room.manifest"))?)
                    };
                    self.phase = Phase::StageRoom;
                    return Ok(Step::More);
                };
                if !meta.key.first().is_some_and(|value| value == "1") {
                    return Ok(Step::More);
                }
                let value = self
                    .read(&meta)?
                    .ok_or_else(|| invalid("missing tail during readiness validation"))?;
                if let Some(prior) = self.last_tail.clone()
                    && prior.key == meta.key
                {
                    let before = self
                        .read(&prior)?
                        .ok_or_else(|| invalid("missing prior tail"))?;
                    if before.body != value.body {
                        return Err(invalid(
                            "conflicting signed tail sequence prevents coherent shell selection",
                        ));
                    }
                }
                self.last_tail = Some(meta);
                let (key, _) = value
                    .route()
                    .ok_or_else(|| invalid("invalid sorted tail route"))?;
                if self
                    .tail_readiness
                    .as_ref()
                    .is_none_or(|(route, _)| route != &key)
                {
                    let snapshot = self
                        .candidate_snapshot(&key)?
                        .ok_or_else(|| invalid("missing sorted tail snapshot"))?;
                    self.tail_readiness = Some((key, snapshot.ordered_tail_readiness()));
                }
                let ready = self.tail_readiness.as_mut().is_some_and(|(_, cursor)| {
                    cursor.append(value.source, value.channel, &value.body)
                });
                if !ready {
                    if self.fallback || self.old.room.is_none() {
                        return Ok(Step::Done(SelectionOutcome::UnresolvedTailDependencies));
                    }
                    self.basis = self.old.clone();
                    self.fallback = true;
                    self.incoming_dispositions.fill(None);
                    self.incoming_staged = false;
                    self.blocked = false;
                    self.extras.set_len(0)?;
                    self.extras.rewind()?;
                    self.retire.set_len(0)?;
                    self.retire.rewind()?;
                    self.last_tail = None;
                    self.tail_readiness = None;
                    self.restart(Phase::Check)?;
                }
            }
            Phase::StageRoom => {
                if let Some(input) = &mut self.input {
                    if let Some(meta) = read_meta(input)? {
                        if !self.fallback || meta.key.first().is_some_and(|v| v == "room") {
                            self.add(&meta, "room")?
                        }
                        return Ok(Step::More);
                    }
                }
                self.route_iter = Some(self.basis.routes.clone().into_iter());
                self.phase = Phase::StageApps;
            }
            Phase::StageApps => {
                if let Some((_, route)) = self.route_iter.as_mut().unwrap().next() {
                    if let Some(meta) = route.file {
                        self.add(&meta, "app")?
                    }
                } else {
                    self.input = Some(File::open(self.sorted_extras.as_ref().unwrap())?);
                    self.phase = Phase::StageExtras
                }
            }
            Phase::StageExtras => {
                if let Some(meta) = read_meta(self.input.as_mut().unwrap())? {
                    let tail = meta.key.first().is_some_and(|v| v == "1");
                    self.add(&meta, if tail { "tail" } else { "extra" })?
                } else {
                    self.input = if self.fallback {
                        None
                    } else {
                        Some(File::open(self.scratch.join("retire.manifest"))?)
                    };
                    self.phase = Phase::StageRetire;
                }
            }
            Phase::StageRetire => {
                if let Some(input) = &mut self.input {
                    if let Some(mut meta) = read_meta(input)? {
                        meta.pin = self.ordinal;
                        bounded::increment(&mut self.ordinal)?;
                        let pin = self.staged.join(format!("gc-{:016x}.delta", meta.pin));
                        // This is another link to an already durable retained
                        // inode; the staged-directory barrier precedes CURRENT.
                        fs::hard_link(self.store.root.join(&meta.source), &pin)?;
                        write_meta(&mut self.gc_output, &meta)?;
                        return Ok(Step::More);
                    }
                }
                self.input = None;
                self.phase = Phase::Commit;
            }
            Phase::Commit => {
                #[cfg(test)]
                super::test_barrier::check(&self.staged, "selection-full-flush")?;
                self.sorter.stats.staging_full_flushes += 3;
                self.output.sync_all()?;
                self.live_output.sync_all()?;
                self.gc_output.sync_all()?;
                retained::sync_dir(&self.staged)?;
                #[cfg(test)]
                super::test_barrier::pause(self.store.root(), "shell-select-switch");
                self.switch_attempted = true;
                retained::durable_write(
                    &self.store.root.join(DIRECTORY).join("CURRENT"),
                    self.staged.file_name().unwrap().as_encoded_bytes(),
                )?;
                #[cfg(test)]
                super::test_barrier::check(self.store.root(), "shell-after-current")?;
                self.active_gc = Some(SelectedGc::new(
                    self.store.root.clone(),
                    self.staged.clone(),
                )?);
                self.phase = Phase::Gc;
            }
            Phase::Gc => {
                if let Some(gc) = &mut self.active_gc {
                    if let Step::Done(()) = gc.step()? {
                        self.active_gc = None;
                    }
                    return Ok(Step::More);
                }
                if self.collect.is_none() {
                    self.collect = Some(retained::GenerationGc::new(
                        self.store.root.join(DIRECTORY),
                    )?);
                }
                if let Step::Done(()) = self.collect.as_mut().unwrap().step(limit)? {
                    self.phase = Phase::Done
                }
            }
            Phase::Done => {
                let publication = if self.changed {
                    Publication::Activate(self.staged.clone())
                } else if self.incoming_staged {
                    Publication::Suppress
                } else {
                    Publication::Original
                };
                if self.batch {
                    let outcome = self.batch_outcome();
                    return Ok(Step::Done(SelectionOutcome::BatchPublished {
                        publication: outcome.publication,
                        dispositions: outcome.dispositions,
                    }));
                }
                return Ok(Step::Done(SelectionOutcome::Published(publication)));
            }
        }
        Ok(Step::More)
    }
}
