//! The settings as resources of the workspace store: each section one resource
//! ([`crate::placement::address`]), each of its settings a field of the resource's content,
//! written by the setting's own verb (`set-<setting>`).
//!
//! [`WorkspaceSettings`] is the session's settings source. It reads from what it last took from
//! the store, so a read never waits; a write shows at once, and is carried to the store on the
//! session's next pass ([`WorkspaceSettings::sync`]), stamped with its key's verb, who made it and
//! one invocation for everything one change wrote. What another device or an agent wrote reaches
//! it on a later pass ([`WorkspaceSettings::refresh`]). Before the session holds a workspace,
//! writes wait, kept in a file of the device's ([`WorkspaceSettings::kept_in`]) so a device that
//! holds none yet keeps them across restarts, and are written once it does
//! ([`WorkspaceSettings::attach`]); until then a setting nobody chose reads as its default. That
//! file is written by a worker ([`WorkspaceSettings::flush_kept`] waits for it, and
//! [`SettingsSource::flush_pending`] and dropping the settings flush it too), so a change made on
//! the loop never waits on the disk and a change made before a restart is still there.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;
use tilo_resource_store::wire::{Command, Edit, Reply};
use tilo_resource_store::{Resource, Stamp, Version};

use settings_store::placement::{self, Address, Holder};
use settings_store::workspace::verb;
use settings_store::{
    ChangeOrigin, ChangeSignal, ConditionalUpdate, DeferredFailure, SettingsSource,
};
/// Where the settings' resources are kept, as the session reaches them: the person's own
/// workspace for theirs, the device owner's for the device's.
pub trait Stores: Send + Sync {
    /// Answer `command` in the workspace that keeps what `holder` holds.
    ///
    /// # Errors
    /// The workspace is not reachable, or refused the command.
    fn command(&self, holder: Holder, command: Command) -> Result<Reply, String>;

    /// The reader set of what `holder` holds: the person, or the device's owner.
    fn readers(&self, holder: Holder) -> String;

    /// This device, by its key in lowercase hex.
    fn device(&self) -> String;

    /// Ask for a pass: a write is waiting to be carried to the store.
    fn wake(&self);
}

/// One change waiting to reach the store: the sections it writes, made by one invocation.
struct Pending {
    sections: Vec<(String, Map<String, Value>)>,
    origin: ChangeOrigin,
}

#[derive(Default)]
struct State {
    /// What each resource held when last read or written, by its address.
    stored: BTreeMap<Address, Map<String, Value>>,
    /// The resources known to exist in the store.
    made: BTreeSet<Address>,
    /// Each resource's newest version as last read: a later read takes in only what moved.
    seen: BTreeMap<Address, String>,
    /// Changes not yet written to the store, oldest first.
    pending: Vec<Pending>,
    /// What every reader reads: [`Self::stored`], with [`Self::pending`] over it.
    view: Map<String, Value>,
    failures: Vec<DeferredFailure>,
}

impl State {
    fn rebuild(&mut self) {
        let mut view = Map::new();
        for (address, content) in &self.stored {
            let Some((section, app)) =
                placement::section_of(address.holder, &address.kind, &address.id)
            else {
                continue;
            };
            match app {
                Some(app) => {
                    settings_store::merge_section(
                        &mut view,
                        &section,
                        Map::from_iter([(app, Value::Object(content.clone()))]),
                    );
                }
                None => settings_store::merge_section(&mut view, &section, content.clone()),
            }
        }
        for pending in &self.pending {
            for (section, values) in &pending.sections {
                settings_store::merge_section(&mut view, section, values.clone());
            }
        }
        self.view = view;
    }
}

/// The session's settings, kept as resources of the workspace store.
pub struct WorkspaceSettings {
    state: Mutex<State>,
    stores: Mutex<Option<Arc<dyn Stores>>>,
    revision: AtomicU64,
    changes: ChangeSignal,
    /// Where what waits for a workspace is kept.
    kept: Option<PathBuf>,
    /// The worker that writes [`Self::kept`], so the pass that made a change never waits on the
    /// disk. `None` until something first waits; a write made while no worker can be started is
    /// made on the caller, as before.
    writing: Mutex<Option<mpsc::Sender<Kept>>>,
}

/// What the worker that keeps what waits for a workspace is given.
enum Kept {
    /// Keep `sections` at `file`, or remove it once nothing waits.
    Write {
        file: PathBuf,
        sections: Map<String, Value>,
    },
    /// Say when everything handed to the worker before this is on the disk.
    Flush(mpsc::Sender<()>),
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceSettings {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            stores: Mutex::new(None),
            revision: AtomicU64::new(1),
            changes: ChangeSignal::default(),
            kept: None,
            writing: Mutex::new(None),
        }
    }

    /// Settings whose writes keep `file` while they wait for a workspace: what an earlier run
    /// set there and no workspace took yet is read from it, and waits again. What is kept is
    /// written by a worker, so the pass that made a change does not wait on the disk
    /// ([`Self::flush_kept`]).
    #[must_use]
    pub fn kept_in(file: PathBuf) -> Self {
        let waiting = settings_store::workspace::waiting(&file);
        let mut settings = Self::new();
        settings.kept = Some(file);
        let sections: Vec<(String, Map<String, Value>)> = waiting
            .into_iter()
            .filter_map(|(name, values)| match values {
                Value::Object(values) => Some((name, values)),
                _ => None,
            })
            .collect();
        if !sections.is_empty() {
            let mut state = settings.state.lock_recover();
            state.pending.push(Pending {
                sections,
                origin: ChangeOrigin::default(),
            });
            state.rebuild();
        }
        settings
    }

    /// Keep what waits for the store in [`Self::kept`], or remove the file once nothing does.
    ///
    /// The file is written by a worker ([`Self::flush_kept`] waits for it): a change made on the
    /// session's loop never waits on the disk.
    fn keep(&self) {
        let Some(file) = &self.kept else {
            return;
        };
        let mut waiting = Map::new();
        for pending in &self.state.lock_recover().pending {
            for (section, values) in &pending.sections {
                settings_store::merge_section(&mut waiting, section, values.clone());
            }
        }
        self.keep_elsewhere(file, waiting);
    }

    /// Hand the file's next content to the worker, or write it here when no worker runs.
    fn keep_elsewhere(&self, file: &Path, sections: Map<String, Value>) {
        let jobs = {
            let mut writing = self.writing.lock_recover();
            if writing.is_none() {
                match start_writer() {
                    Ok(started) => *writing = Some(started),
                    Err(error) => tracing::warn!(
                        %error,
                        "the settings have no worker to keep them waiting: they are kept here"
                    ),
                }
            }
            writing.clone()
        };
        let job = Kept::Write {
            file: file.to_owned(),
            sections,
        };
        // A job no worker took is written here.
        let unsent = match jobs {
            Some(jobs) => jobs.send(job).err().map(|unsent| unsent.0),
            None => Some(job),
        };
        if let Some(Kept::Write { file, sections }) = unsent {
            write_waiting(&file, &sections);
        }
    }

    /// Wait until everything the settings keep for a workspace that is not held yet is on the
    /// disk: what another run reads back ([`Self::kept_in`]), and what leaving the settings alive
    /// or dropping them must not lose.
    pub fn flush_kept(&self) {
        let jobs = self.writing.lock_recover().clone();
        let Some(jobs) = jobs else {
            return;
        };
        let (done, waited) = mpsc::channel();
        if jobs.send(Kept::Flush(done)).is_ok() {
            let _ = waited.recv();
        }
    }

    /// A write was queued: readers hear of it, it is kept until a workspace takes it, and the
    /// session is asked for a pass to carry it there.
    fn queued(&self) {
        self.moved();
        match self.stores() {
            Some(stores) => stores.wake(),
            None => self.keep(),
        }
    }

    fn stores(&self) -> Option<Arc<dyn Stores>> {
        self.stores.lock_recover().clone()
    }

    fn moved(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.changes.ring();
    }

    /// Keep the settings in `stores` from now on: what was written before is written there, and
    /// what they hold is read.
    pub fn attach(&self, stores: Arc<dyn Stores>) {
        *ATTACHED.lock_recover() = Some(Arc::clone(&stores));
        *self.stores.lock_recover() = Some(stores);
        {
            let mut state = self.state.lock_recover();
            state.stored.clear();
            state.made.clear();
            state.seen.clear();
        }
        self.refresh();
        self.sync();
    }

    /// Write every change waiting for the store. A change the store refuses is dropped, and its
    /// failure kept for whoever made it to say so ([`SettingsSource::take_deferred_failures`]).
    pub fn sync(&self) {
        let Some(stores) = self.stores() else {
            return;
        };
        let device = stores.device();
        let mut carried = false;
        loop {
            let (pending, made, kept) = {
                let mut state = self.state.lock_recover();
                if state.pending.is_empty() {
                    drop(state);
                    if carried {
                        self.keep();
                    }
                    return;
                }
                carried = true;
                let pending = state.pending.remove(0);
                (pending, state.made.clone(), state.stored.clone())
            };
            let invocation = uuid::Uuid::new_v4().to_string();
            let (commands, written) = commands_for(
                stores.as_ref(),
                &pending,
                &device,
                (&made, &kept),
                &invocation,
            );
            let mut refused = None;
            for (holder, steps) in commands {
                if let Err(error) = stores.command(holder, Command::Batch(steps)) {
                    refused = Some(error);
                }
            }
            let mut state = self.state.lock_recover();
            match refused {
                None => {
                    for (address, values) in written {
                        let content = state.stored.entry(address.clone()).or_default();
                        for (leaf, value) in values {
                            if value.is_null() {
                                content.remove(&leaf);
                            } else {
                                content.insert(leaf, value);
                            }
                        }
                        state.made.insert(address);
                    }
                    state.rebuild();
                }
                Some(error) => {
                    tracing::warn!(%error, "a settings change was refused");
                    state.failures.push(DeferredFailure {
                        sections: pending
                            .sections
                            .iter()
                            .map(|(name, _)| name.clone())
                            .collect(),
                        error,
                    });
                    // What it wrote is read again rather than guessed.
                    for (address, _) in &written {
                        state.made.remove(address);
                        state.seen.remove(address);
                    }
                    state.rebuild();
                    drop(state);
                    settings_store::note_deferred_failure();
                    self.moved();
                }
            }
        }
    }

    /// Take in what moved in the store since the last read: another device's change, or an
    /// agent's. Whether anything did.
    pub fn refresh(&self) -> bool {
        let Some(stores) = self.stores() else {
            return false;
        };
        let device = stores.device();
        let mut moved = false;
        for (holder, within) in [
            (Holder::Person, "settings".to_owned()),
            (Holder::Device, format!("devices/{device}/settings")),
        ] {
            let newest = match stores.command(
                holder,
                Command::Newest {
                    within,
                    limit: 4096,
                },
            ) {
                Ok(Reply::Newest { versions, .. }) => versions,
                Ok(_) => continue,
                Err(error) => {
                    tracing::debug!(%error, "the settings were not read");
                    continue;
                }
            };
            let changed: Vec<(Address, String)> = {
                let state = self.state.lock_recover();
                newest
                    .into_iter()
                    .map(|(kind, id, version)| (Address { holder, kind, id }, version.change))
                    .filter(|(address, change)| state.seen.get(address) != Some(change))
                    .collect()
            };
            for (address, change) in changed {
                if placement::section_of(holder, &address.kind, &address.id).is_none() {
                    continue;
                }
                let Ok(Reply::Content(content)) = stores.command(
                    holder,
                    Command::Content {
                        kind: address.kind.clone(),
                        id: address.id.clone(),
                    },
                ) else {
                    continue;
                };
                let mut state = self.state.lock_recover();
                state.made.insert(address.clone());
                state.seen.insert(address.clone(), change);
                let content = match content {
                    Some(Value::Object(content)) => content,
                    _ => Map::new(),
                };
                if state.stored.get(&address) != Some(&content) {
                    state.stored.insert(address, content);
                    moved = true;
                }
                drop(state);
            }
        }
        if moved {
            self.state.lock_recover().rebuild();
            self.moved();
        }
        moved
    }

    fn write(&self, sections: Vec<(String, Map<String, Value>)>, origin: &ChangeOrigin) {
        {
            let mut state = self.state.lock_recover();
            for (section, values) in &sections {
                settings_store::merge_section(&mut state.view, section, values.clone());
            }
            state.pending.push(Pending {
                sections,
                origin: origin.clone(),
            });
        }
        self.queued();
    }

    /// The versions of section `section`, oldest first: who changed it, by which setting.
    ///
    /// # Errors
    /// The settings are kept in no workspace yet, or it cannot be read.
    pub fn history(&self, section: &str) -> std::io::Result<Vec<Version>> {
        self.history_of(section, None)
    }

    /// [`Self::history`] of `section`, of the app `app` where the section keeps each app's
    /// preferences as a resource of its own.
    fn history_of(&self, section: &str, app: Option<&str>) -> std::io::Result<Vec<Version>> {
        let stores = self.stores().ok_or_else(unattached)?;
        let address = placement::address(section, app, &stores.device());
        match stores
            .command(
                address.holder,
                Command::Versions {
                    kind: address.kind,
                    id: address.id,
                },
            )
            .map_err(std::io::Error::other)?
        {
            Reply::Versions(versions) => Ok(versions),
            _ => Ok(Vec::new()),
        }
    }
}

impl Drop for WorkspaceSettings {
    fn drop(&mut self) {
        // What waits for a workspace outlives these settings: another run reads it
        // ([`Self::kept_in`]), so nothing a person set may be lost as they go.
        self.flush_kept();
    }
}

/// The worker that keeps what waits for a workspace, and the channel to it.
fn start_writer() -> std::io::Result<mpsc::Sender<Kept>> {
    let (jobs, work) = mpsc::channel::<Kept>();
    std::thread::Builder::new()
        .name("tilo-settings-waiting".into())
        .spawn(move || {
            while let Ok(job) = work.recv() {
                match job {
                    Kept::Write { file, sections } => write_waiting(&file, &sections),
                    Kept::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        })?;
    Ok(jobs)
}

/// [`settings_store::workspace::keep_waiting`], saying so when it fails.
fn write_waiting(file: &Path, waiting: &Map<String, Value>) {
    if let Err(error) = settings_store::workspace::keep_waiting(file, waiting) {
        tracing::warn!(%error, file = %file.display(), "the settings waiting for a workspace were not kept");
    }
}

/// The workspaces the session's settings were last kept in ([`WorkspaceSettings::attach`]): where
/// [`set_on_device`] writes another device's settings.
static ATTACHED: Mutex<Option<Arc<dyn Stores>>> = Mutex::new(None);

/// Set `field` of the section `section` of the device `device` (its key in lowercase hex) to
/// `value`, as `origin` made the change: written straight to that device's resource in its
/// owner's workspace, stamped with the field's setter, as any change of the settings is. What
/// sets another of the owner's devices' settings from this one, a device without a session (a
/// server, a daemon) among them; this device's own section is left as it is.
///
/// # Errors
/// The settings are kept in no workspace yet, `section` is none of a device's, or the workspace
/// refused the change.
pub fn set_on_device(
    device: &str,
    section: &str,
    field: &str,
    value: &Value,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    let stores = ATTACHED.lock_recover().clone().ok_or_else(unattached)?;
    set_on(stores.as_ref(), device, section, field, value, origin)
}

/// The commands to run in each workspace, and what each resource holds once they ran.
type Written = (
    BTreeMap<Holder, Vec<Command>>,
    Vec<(Address, Map<String, Value>)>,
);

/// The commands that write `pending` to the store on `device`, by the workspace that keeps each,
/// given the resources already `made` and what each `stored` holds; and what each resource holds
/// once they ran.
fn commands_for(
    workspaces: &dyn Stores,
    pending: &Pending,
    device: &str,
    (made, stored): (&BTreeSet<Address>, &BTreeMap<Address, Map<String, Value>>),
    invocation: &str,
) -> Written {
    let mut commands: BTreeMap<Holder, Vec<Command>> = BTreeMap::new();
    let mut written: Vec<(Address, Map<String, Value>)> = Vec::new();
    for (section, values) in &pending.sections {
        for (address, values) in units(section, values, device, stored) {
            let steps = commands.entry(address.holder).or_default();
            let first = values.keys().next().cloned().unwrap_or_default();
            if !made.contains(&address) {
                steps.push(Command::Put {
                    kind: address.kind.clone(),
                    id: address.id.clone(),
                    resource: Resource {
                        readers: workspaces.readers(address.holder),
                        ..Resource::default()
                    },
                    // Making the resource is nobody's step: undoing a setting never
                    // takes the section away with it.
                    stamp: stamp(&first, &pending.origin, &uuid::Uuid::new_v4().to_string()),
                });
            }
            let held = stored.get(&address);
            for (leaf, value) in &values {
                let edits = edits(leaf, value, held.and_then(|held| held.get(leaf)));
                if edits.is_empty() {
                    continue;
                }
                steps.push(Command::Edit {
                    kind: address.kind.clone(),
                    id: address.id.clone(),
                    edits,
                    stamp: stamp(leaf, &pending.origin, invocation),
                });
            }
            written.push((address, values));
        }
    }
    (commands, written)
}

/// [`set_on_device`], in `stores`.
fn set_on(
    stores: &dyn Stores,
    device: &str,
    section: &str,
    field: &str,
    value: &Value,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    let address = placement::address(section, None, device);
    if address.holder != Holder::Device || placement::per_app(section) {
        return Err(std::io::Error::other(format!(
            "`{section}` is no section of a device's"
        )));
    }
    let held = match stores.command(
        Holder::Device,
        Command::Content {
            kind: address.kind.clone(),
            id: address.id.clone(),
        },
    ) {
        Ok(Reply::Content(content)) => content,
        Ok(_) => None,
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let pending = Pending {
        sections: vec![(
            section.to_owned(),
            Map::from_iter([(field.to_owned(), value.clone())]),
        )],
        origin: origin.clone(),
    };
    let made = held.iter().map(|_| address.clone()).collect();
    let kept = match held {
        Some(Value::Object(fields)) => BTreeMap::from([(address, fields)]),
        _ => BTreeMap::new(),
    };
    let invocation = uuid::Uuid::new_v4().to_string();
    let (commands, _) = commands_for(stores, &pending, device, (&made, &kept), &invocation);
    let steps = commands.into_values().flatten().collect();
    stores
        .command(Holder::Device, Command::Batch(steps))
        .map(drop)
        .map_err(std::io::Error::other)
}

fn unattached() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the settings are kept in no workspace yet",
    )
}

/// The stamp a write of `leaf` carries: its verb, who made it and the invocation.
fn stamp(leaf: &str, origin: &ChangeOrigin, invocation: &str) -> Stamp {
    Stamp {
        verb: verb(leaf),
        invoker: origin.actor.clone().unwrap_or_default(),
        invocation: invocation.to_owned(),
        moment: tilo_clock::try_unix_now_ns().unwrap_or_default(),
        ..Stamp::default()
    }
}

/// The edits that make `leaf` hold `value` where it held `held`: a map is written entry by entry,
/// so the entries another device or an agent wrote meanwhile stay (each device's place among the
/// person's displays, each app's rule); anything else is written whole, and null removes it.
fn edits(leaf: &str, value: &Value, held: Option<&Value>) -> Vec<Edit> {
    let field = || leaf.to_owned();
    match (value, held) {
        (Value::Null, _) => vec![Edit::Remove { field: field() }],
        (Value::Object(entries), None | Some(Value::Object(_))) => {
            let held = held.and_then(Value::as_object);
            let gone = held
                .into_iter()
                .flatten()
                .filter(|(key, _)| !entries.contains_key(*key))
                .map(|(key, _)| Edit::MapDelete {
                    field: field(),
                    key: key.clone(),
                });
            let put = entries
                .iter()
                .filter(|(key, entry)| held.and_then(|held| held.get(*key)) != Some(*entry))
                .map(|(key, entry)| Edit::MapPut {
                    field: field(),
                    key: key.clone(),
                    value: entry.clone(),
                });
            gone.chain(put).collect()
        }
        _ => vec![Edit::Set {
            field: field(),
            value: value.clone(),
        }],
    }
}

/// The resources a write of `values` to section `section` changes, each with the fields it
/// sets (a null field is removed): an app section's entry is the app's own resource, its
/// preferences written whole over what `stored` holds of it.
fn units(
    section: &str,
    values: &Map<String, Value>,
    device: &str,
    stored: &BTreeMap<Address, Map<String, Value>>,
) -> Vec<(Address, Map<String, Value>)> {
    if !placement::per_app(section) {
        if values.is_empty() {
            return Vec::new();
        }
        return vec![(placement::address(section, None, device), values.clone())];
    }
    values
        .iter()
        .map(|(app, preferences)| {
            let address = placement::address(section, Some(app), device);
            let mut written: Map<String, Value> = stored
                .get(&address)
                .into_iter()
                .flat_map(|held| held.keys())
                .map(|key| (key.clone(), Value::Null))
                .collect();
            if let Value::Object(preferences) = preferences {
                written.extend(preferences.clone());
            }
            (address, written)
        })
        .filter(|(_, written)| !written.is_empty())
        .collect()
}

impl SettingsSource for WorkspaceSettings {
    fn stamps(&self, section: &str) -> std::io::Result<Vec<Stamp>> {
        let mut stamps: Vec<Stamp> = match self.history(section) {
            Ok(versions) => versions
                .into_iter()
                .filter_map(|version| version.stamp)
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Vec::new(),
            Err(error) => return Err(error),
        };
        let state = self.state.lock_recover();
        for pending in &state.pending {
            for (name, values) in &pending.sections {
                if name == section {
                    stamps.extend(values.keys().map(|leaf| stamp(leaf, &pending.origin, "")));
                }
            }
        }
        drop(state);
        Ok(stamps)
    }

    fn load(&self) -> Map<String, Value> {
        self.state.lock_recover().view.clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        self.update_section_by(name, values, &ChangeOrigin::default())
    }

    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        self.update_sections(vec![(name.to_owned(), values)], origin)
    }

    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut settings_store::SectionChange<'_>,
    ) -> std::io::Result<()> {
        // The view is changed under its lock, so a write made meanwhile is never lost.
        {
            let mut state = self.state.lock_recover();
            let current = match state.view.get(name) {
                Some(Value::Object(section)) => section.clone(),
                _ => Map::new(),
            };
            let values = change(&current);
            settings_store::merge_section(&mut state.view, name, values.clone());
            state.pending.push(Pending {
                sections: vec![(name.to_owned(), values)],
                origin: origin.clone(),
            });
        }
        self.queued();
        Ok(())
    }

    fn update_sections(
        &self,
        sections: Vec<(String, Map<String, Value>)>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        for (section, values) in &sections {
            let size = serde_json::to_vec(values).map_or(0, |bytes| bytes.len());
            if size > placement::MAX_PERSONAL_VALUE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("the {section} settings are too large to keep"),
                ));
            }
        }
        self.write(sections, origin);
        Ok(())
    }

    fn compare_update_sections(
        &self,
        expected: &[(&str, Map<String, Value>)],
        sections: Vec<(String, Map<String, Value>)>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<ConditionalUpdate> {
        {
            let mut state = self.state.lock_recover();
            let unchanged = expected
                .iter()
                .all(|(name, expected)| match state.view.get(*name) {
                    Some(Value::Object(section)) => section == expected,
                    _ => expected.is_empty(),
                });
            if !unchanged {
                return Ok(ConditionalUpdate::Superseded);
            }
            for (section, values) in &sections {
                settings_store::merge_section(&mut state.view, section, values.clone());
            }
            state.pending.push(Pending {
                sections,
                origin: origin.clone(),
            });
        }
        self.queued();
        Ok(ConditionalUpdate::Stored)
    }

    fn step(
        &self,
        section: &str,
        app: Option<&str>,
        undo: bool,
        invokers: &[String],
    ) -> std::io::Result<bool> {
        self.sync();
        let stores = self.stores().ok_or_else(unattached)?;
        let address = placement::address(section, app, &stores.device());
        let Some((invocation, verb)) = step_of(&self.history_of(section, app)?, undo, invokers)
        else {
            return Ok(false);
        };
        let own = uuid::Uuid::new_v4().to_string();
        let stamp = Stamp {
            verb,
            invoker: invokers.first().cloned().unwrap_or_default(),
            invocation: own,
            undoes: Some(invocation.clone()),
            moment: tilo_clock::try_unix_now_ns().unwrap_or_default(),
            ..Stamp::default()
        };
        stores
            .command(address.holder, Command::Undo { invocation, stamp })
            .map_err(std::io::Error::other)?;
        self.state.lock_recover().seen.remove(&address);
        self.refresh();
        Ok(true)
    }

    fn flush_pending(&self) {
        self.sync();
        self.flush_kept();
    }

    fn take_deferred_failures(&self) -> Vec<DeferredFailure> {
        std::mem::take(&mut self.state.lock_recover().failures)
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn changes(&self) -> &ChangeSignal {
        &self.changes
    }
}

/// The invocation a step of `invokers` takes back in `versions` (oldest first), and the verb it
/// takes back: to undo, their newest change not taken back; to redo, their newest undo, when
/// nothing of theirs came after it. Redo is the undo of an undo.
fn step_of(versions: &[Version], undo: bool, invokers: &[String]) -> Option<(String, String)> {
    // A step changed what a section holds; making its resource is none.
    let stamps: Vec<&Stamp> = versions
        .iter()
        .filter(|version| version.content)
        .filter_map(|version| version.stamp.as_ref())
        .collect();
    let undone: BTreeSet<&str> = stamps
        .iter()
        .filter_map(|stamp| stamp.undoes.as_deref())
        .collect();
    let mut newest_first = stamps
        .iter()
        .rev()
        .filter(|stamp| invokers.contains(&stamp.invoker))
        .filter(|stamp| !undone.contains(stamp.invocation.as_str()));
    if undo {
        // A redo is a change of theirs too: undoing it takes the undo back.
        let stamp =
            newest_first.find(|stamp| stamp.undoes.is_none() || undone_undo(&stamps, stamp))?;
        return Some((stamp.invocation.clone(), stamp.verb.clone()));
    }
    // A redo already made is passed over, so steps redone one after another bring back each
    // undo in turn, newest first; a change of theirs since the undos ends what can be redone.
    let stamp = newest_first.find(|stamp| !undone_undo(&stamps, stamp))?;
    stamp
        .undoes
        .is_some()
        .then(|| (stamp.invocation.clone(), stamp.verb.clone()))
}

/// Whether `stamp` is a redo: the undo of an undo.
fn undone_undo(stamps: &[&Stamp], stamp: &Stamp) -> bool {
    stamp.undoes.as_deref().is_some_and(|undone| {
        stamps
            .iter()
            .any(|earlier| earlier.invocation == undone && earlier.undoes.is_some())
    })
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
