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

pub(crate) const DIRECTORY: &str = "shell-selected-v1";
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Publication {
    Original,
    Suppress,
    Activate(PathBuf),
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
    old_scan: Option<File>,
    sorter: Sorter,
    inventory: Option<PathBuf>,
    input: Option<File>,
    extras_sorter: Option<Sorter>,
    sorted_extras: Option<PathBuf>,
    extras_ordinal: u64,
    last_tail: Option<Meta>,
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
    incoming: Option<[u8; 32]>,
    incoming_staged: bool,
}
impl Selection {
    pub(crate) fn new(
        store: &WorkspaceStore,
        scratch: PathBuf,
        incoming: Option<[u8; 32]>,
    ) -> Result<Self, StoreError> {
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
            old_scan,
            sorter: Sorter::new(scratch.join("shell-sort"))?,
            inventory: None,
            input: None,
            extras_sorter: None,
            sorted_extras: None,
            extras_ordinal: 0,
            last_tail: None,
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
            incoming,
            incoming_staged: false,
            scratch,
        })
    }
    pub(crate) fn stats(&self) -> &WorkStats {
        &self.sorter.stats
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
        fs::hard_link(self.store.root.join(&meta.source), &path)?;
        File::open(&path)?.sync_all()?;
        let mut selected = meta.clone();
        selected.source = path.strip_prefix(&self.store.root).unwrap().into();
        selected.key = vec![role.into()];
        write_meta(&mut self.output, &selected)?;
        let live = if role == "room" {
            let value = self
                .read(meta)?
                .ok_or_else(|| invalid("invalid live prerequisite"))?;
            self.old.room.as_ref().is_none_or(|room| {
                room.covers_frame(value.source, value.channel, &value.body)
                    != CoverageDecision::Covered
            })
        } else if role == "app" {
            let value = self
                .read(meta)?
                .ok_or_else(|| invalid("invalid live application"))?;
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
    fn note_staged(&mut self, value: &Frame) {
        if self.incoming
            == Some(input_identity(
                value.source,
                value.channel,
                value.kind == RoomInput::Checkpoint,
                &value.body,
            ))
        {
            self.incoming_staged = true
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
            format!(
                "{}-{}",
                hex::encode(value.source),
                hex::encode(value.channel)
            )
            .into(),
            format!("{seq:016x}").into(),
        ];
        write_meta(&mut self.extras, &extra)
    }
    fn covered(&mut self, meta: &Meta) -> Result<(), StoreError> {
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
    pub(crate) fn step(&mut self, limit: WorkLimit) -> Result<Step<Publication>, StoreError> {
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
                            let lineage = path
                                .parent()
                                .unwrap()
                                .parent()
                                .unwrap()
                                .file_name()
                                .unwrap()
                                .to_os_string();
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
                        self.child = None
                    }
                } else if let Some(entry) = self.outer.as_mut().unwrap().next() {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        if let Some(active) = retained::current(&entry.path())? {
                            self.child = Some(fs::read_dir(active)?);
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
                    }
                    self.restart(Phase::Check)?;
                }
            }
            Phase::Check => {
                let Some(meta) = read_meta(self.input.as_mut().unwrap())? else {
                    if self.blocked && !self.fallback {
                        self.basis = self.old.clone();
                        self.fallback = true;
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
                    self.extras_sorter = Some(Sorter::new(self.scratch.join("extras-sort"))?);
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
                        self.covered(&meta)?
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
                        self.note_staged(&value)
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
                    self.covered(&meta)?;
                    return Ok(Step::More);
                }
                if matches!(snapshot_result, Some(Err(ProofError::Unpaired))) {
                    if self
                        .basis
                        .routes
                        .get(&key)
                        .is_none_or(|r| seq > r.coverage.sequence)
                    {
                        self.note_staged(&value);
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
                            self.note_staged(&value);
                            return Ok(Step::More);
                        }
                    }
                }
                if let Some(snapshot) = self.candidate_snapshot(&key)? {
                    let decision = snapshot.classify_tail(value.source, value.channel, &value.body);
                    if decision.coverage == CoverageDecision::Covered {
                        self.covered(&meta)?;
                        return Ok(Step::More);
                    }
                    // Individually ready tails still need exact author sequence
                    // ordering; the bounded extras sorter establishes it below.
                    if snapshot.tail_ready_after_snapshot(value.source, value.channel, &value.body)
                    {
                        self.tail(&meta, &value, seq)?;
                        return Ok(Step::More);
                    }
                    if decision.floor != FloorSafety::Unknown
                        && (self.fallback || self.old.room.is_none())
                    {
                        return Err(invalid(
                            "unresolved shell tail floor or dependencies prevent coherent selection",
                        ));
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
                        self.covered(&meta)?;
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
                    self.sorted_extras = Some(path);
                    self.input = if self.fallback {
                        retained::current(&self.store.root.join(DIRECTORY))?
                            .map(|p| File::open(p.join("basis.manifest")))
                            .transpose()?
                    } else {
                        Some(File::open(self.scratch.join("room.manifest"))?)
                    };
                    self.phase = Phase::StageRoom;
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
                    if tail {
                        if let Some(prior) = self.last_tail.clone() {
                            if prior.key == meta.key {
                                let before = self
                                    .read(&prior)?
                                    .ok_or_else(|| invalid("missing prior tail"))?;
                                let after =
                                    self.read(&meta)?.ok_or_else(|| invalid("missing tail"))?;
                                if before.body != after.body {
                                    return Err(invalid(
                                        "conflicting signed tail sequence prevents coherent shell selection",
                                    ));
                                }
                            }
                        }
                        self.last_tail = Some(meta.clone());
                    }
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
                        fs::hard_link(self.store.root.join(&meta.source), &pin)?;
                        File::open(&pin)?.sync_all()?;
                        write_meta(&mut self.gc_output, &meta)?;
                        return Ok(Step::More);
                    }
                }
                self.input = None;
                self.phase = Phase::Commit;
            }
            Phase::Commit => {
                self.output.sync_all()?;
                self.live_output.sync_all()?;
                self.gc_output.sync_all()?;
                retained::sync_dir(&self.staged)?;
                #[cfg(test)]
                super::test_barrier::pause(self.store.root(), "shell-select-switch");
                retained::durable_write(
                    &self.store.root.join(DIRECTORY).join("CURRENT"),
                    self.staged.file_name().unwrap().as_encoded_bytes(),
                )?;
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
                return Ok(Step::Done(if self.changed {
                    Publication::Activate(self.staged.clone())
                } else if self.incoming_staged {
                    Publication::Suppress
                } else {
                    Publication::Original
                }));
            }
        }
        Ok(Step::More)
    }
}
