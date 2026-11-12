//! The changes a record's repair left out, taken in again once the record
//! holds what they build on.
//!
//! A record whose history held writes it could not merge (built on changes
//! it never got, or writing a sequence another write already holds) was
//! repaired by compacting it into what reads. Every change the repair left
//! out is kept as it was written, under [`REPAIRED_RECORDS`] beside the host's
//! workspaces: `<workspace>/<app>/<record>/<hash>.change`, the primary
//! record's directory named [`PRIMARY`]. Those changes were folded out of the
//! record's history, so nothing else here holds them any longer.
//!
//! When the host starts, and whenever a member's write to such a record lands,
//! every kept change of it whose dependencies the record now holds is written
//! into the record as the host's own change, through the host's normal
//! authoring path ([`Workspaces::author_room_record`]): members admit it as
//! they admit any write of the workspace's host, and a kept change that waits
//! on another kept change follows it in the same write. A change the record
//! holds by then is done with too. Each change taken in, or held already,
//! leaves the archive; what still waits stays, and an empty record directory
//! goes.
//!
//! A kept change whose writer and sequence the record holds under another
//! change (the branch a repair carried forward rather than kept) can never
//! merge, and stays as kept.
//!
//! A change a bot made showing a name other than its own is admitted only as
//! that bot's write: a member holds a change that names a bot and shows a name
//! ([`collab_types::check_appearances`]) to the bot it names. A record held in
//! the clear therefore takes such a change in as the bot's own write
//! ([`Workspaces::publish_as_bot`]), signed by the device the bot runs as,
//! which the embedder hands the host with [`Workspaces::keep_bot_device`]; the
//! host's own write of it would be refused, its own admission included. A
//! record held sealed decides its writes without that check, and takes one in
//! as any kept change does.
//!
//! A record whose archive is not empty changes constantly (the shared desktop
//! does), and re-reading and re-writing it for every write of it costs more
//! than the kept changes are worth. Each record's kept changes say what they
//! wait for (`Waits`), and a write is only tried against its record when the
//! changes it carried could release one of them: its hash is one they build
//! on, or its place in its writer's sequence is the one they follow. The write
//! is read for that here, never the record.

use super::{GovernedWorkspace, RecordWrite, WeakWorkspaces, Workspaces};
use automerge::{ActorId, AutoCommit, Change, ChangeHash};
use ed25519_dalek::SigningKey;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use tilo_automerge::ApplyIncoming as _;

/// The directory beside a host's workspaces, under the relay's state root,
/// where a record's repair keeps every change it left out.
pub const REPAIRED_RECORDS: &str = "repaired-records";

/// The directory a record's kept changes are under when the record is its
/// app's primary (`""` on the wire).
pub const PRIMARY: &str = "(primary)";

/// The file extension of a kept change.
const CHANGE: &str = "change";

/// Where a record's repair keeps the changes it left out, under the relay's
/// state root `root`: `<root>/repaired-records/<workspace>/<app>/<record>`.
#[must_use]
pub fn repaired_record_dir(root: &Path, workspace: &str, app: &str, record: &str) -> PathBuf {
    root.join(REPAIRED_RECORDS)
        .join(workspace)
        .join(app)
        .join(if record.is_empty() { PRIMARY } else { record })
}

/// One kept change, read back.
struct Kept {
    path: PathBuf,
    change: Change,
}

/// A record: its workspace, its app, and its id on the wire (`""` for the
/// app's primary).
type Record = (String, String, String);

/// One change a write carried into a record, as the record holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandedChange {
    /// Its hash: what a kept change that builds on it names.
    pub hash: ChangeHash,
    /// The writer that made it.
    pub writer: ActorId,
    /// Its place in that writer's sequence.
    pub seq: u64,
}

/// What a write carried that a record's repair could take in, worked out
/// without reading the record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Carried {
    /// Nothing a record's repair keeps waits for a change like this write's.
    #[default]
    Nothing,
    /// The changes the write carried.
    Changes(Vec<LandedChange>),
    /// Whether the write releases anything is not known here: its record's
    /// archive has not been read yet, or the write does not open here. The
    /// record's kept changes are tried all the same.
    Unknown,
}

impl Carried {
    /// This write and `later`, one that landed with it.
    fn and_later(&mut self, later: Self) {
        match (&mut *self, later) {
            (Self::Unknown, _) | (_, Self::Nothing) => {}
            (carried, Self::Unknown) => *carried = Self::Unknown,
            (Self::Nothing, later) => *self = later,
            (Self::Changes(changes), Self::Changes(more)) => changes.extend(more),
        }
    }
}

/// What the kept changes of one record wait for, as its archive says: the
/// hashes of the changes they build on, and the change of its writer each of
/// them follows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Waits {
    /// The changes a kept change builds on.
    builds_on: BTreeSet<ChangeHash>,
    /// The writer and sequence of the change a kept change follows.
    follows: BTreeSet<(ActorId, u64)>,
}

impl Waits {
    /// What `changes` waits for. A change whose dependencies the record holds
    /// already waits for nothing, and is taken in beside its record's kept
    /// changes, so this names some changes the record has: a write carrying
    /// one of them again is tried once for nothing.
    fn of<'a>(changes: impl IntoIterator<Item = &'a Change>) -> Self {
        let mut waits = Self::default();
        for change in changes {
            waits.builds_on.extend(change.deps().iter().copied());
            if let Some(previous) = change.seq().checked_sub(1) {
                waits.follows.insert((change.actor_id().clone(), previous));
            }
        }
        waits
    }

    /// Whether anything kept waits for one of `carried`.
    fn released_by(&self, carried: &[LandedChange]) -> bool {
        carried.iter().any(|change| {
            self.builds_on.contains(&change.hash)
                || self.follows.contains(&(change.writer.clone(), change.seq))
        })
    }

    /// Whether it names anything at all: what a write could release.
    fn any(&self) -> bool {
        !self.builds_on.is_empty() || !self.follows.is_empty()
    }
}

/// What the kept changes of each repaired record wait for, by record, read
/// from the archive and kept as every pass leaves it
/// ([`Workspaces::carried_into`]), and the devices of the bots this host runs
/// that make their own changes back.
#[derive(Default)]
pub(crate) struct Repairs {
    /// A record with an entry here has been read: what is left of its kept
    /// changes waits for these, and nothing any write carries releases them
    /// when this names nothing. A record without one has not been read, or
    /// keeps nothing.
    waits: std::sync::Mutex<BTreeMap<Record, Waits>>,
    /// The device of each bot this host runs, by workspace and installation
    /// ([`Workspaces::keep_bot_device`]).
    bots: std::sync::RwLock<BTreeMap<(String, String), ed25519_dalek::SigningKey>>,
}

impl Repairs {
    /// Keep `key`, the device bot installation `installation` runs as in
    /// `workspace`.
    pub(super) fn keep_bot_device(
        &self,
        workspace: &str,
        installation: &str,
        key: ed25519_dalek::SigningKey,
    ) {
        self.bots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((workspace.to_owned(), installation.to_owned()), key);
    }

    /// The device of bot `installation` in `workspace`, when this host runs it.
    fn bot_device(&self, workspace: &str, installation: &str) -> Option<ed25519_dalek::SigningKey> {
        self.bots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(workspace.to_owned(), installation.to_owned()))
            .cloned()
    }

    /// What the kept changes of `record` under `root`'s archive wait for;
    /// `None` when the record keeps nothing, or its archive does not read
    /// here.
    fn of(&self, root: &Path, (workspace, app, record): (&str, &str, &str)) -> Option<Waits> {
        let key = record_key(workspace, app, record);
        if let Some(read) = self.lock().get(&key) {
            return Some(read.clone());
        }
        let kept = read_kept(&repaired_record_dir(root, workspace, app, record)).ok()?;
        if kept.is_empty() {
            return None;
        }
        let read = Waits::of(kept.iter().map(|kept| &kept.change));
        self.lock().insert(key, read.clone());
        Some(read)
    }

    /// Read `record`'s archive afresh, as a pass over its kept changes left
    /// it.
    fn keep(&self, root: &Path, (workspace, app, record): (&str, &str, &str)) {
        let key = record_key(workspace, app, record);
        let kept =
            read_kept(&repaired_record_dir(root, workspace, app, record)).unwrap_or_default();
        let mut waits = self.lock();
        if kept.is_empty() {
            waits.remove(&key);
        } else {
            let read = Waits::of(kept.iter().map(|kept| &kept.change));
            waits.insert(key, read);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<Record, Waits>> {
        self.waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn record_key(workspace: &str, app: &str, record: &str) -> Record {
    (workspace.into(), app.into(), record.into())
}

/// How many times each record's kept changes were tried here, for the tests
/// that hold a record to being read only for the writes that can release
/// something its repair keeps.
#[cfg(test)]
static TRIED: std::sync::LazyLock<std::sync::Mutex<BTreeMap<Record, usize>>> =
    std::sync::LazyLock::new(std::sync::Mutex::default);

/// How many times the kept changes of `record` of `app` in `workspace` were
/// tried.
#[cfg(test)]
pub(crate) fn tried(workspace: &str, app: &str, record: &str) -> usize {
    TRIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&record_key(workspace, app, record))
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "one map holds every record's count, for the whole test binary"
)]
fn count_try(workspace: &str, app: &str, record: &str) {
    let mut tried = TRIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let times = tried.entry(record_key(workspace, app, record)).or_default();
    *times = times.saturating_add(1);
}

/// What taking a record's kept changes in did.
#[derive(Debug, Default, PartialEq, Eq)]
struct TakenIn {
    /// Kept changes written into the record now.
    merged: usize,
    /// Kept changes the record held already.
    held: usize,
    /// Kept changes that still wait, and stay.
    waiting: usize,
}

impl Workspaces {
    /// Where the changes this host's records' repairs left out are kept: beside
    /// its workspaces, under the relay's state root.
    fn repaired_records_root(&self) -> Option<PathBuf> {
        self.inner.dir.parent().map(Path::to_path_buf)
    }

    /// Take in every kept change of every repaired record now, then again for a
    /// record each time a member's write to it lands, for as long as the
    /// workspaces are governed here.
    pub(super) fn follow_repaired_records(&self) {
        let Some(root) = self.repaired_records_root() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // Subscribed before the first pass: a write that lands during it is
        // followed after.
        let mut writes = self.record_writes();
        let workspaces = self.downgrade();
        runtime.spawn(async move {
            for (workspace, app, record) in kept_records(&root) {
                let Some(live) = workspaces.upgrade() else {
                    return;
                };
                live.take_in_kept(&root, &workspace, &app, &record).await;
            }
            follow_writes(&workspaces, &root, &mut writes).await;
        });
    }

    /// Keep `key`, the device bot installation `installation` runs as in
    /// `workspace`, for as long as this host runs it: what a change that bot
    /// made showing another name is published with. A host that keeps no key
    /// for it leaves such a change in its archive.
    #[cfg(feature = "agents")]
    pub fn keep_bot_device(&self, workspace: &str, installation: &str, key: SigningKey) {
        self.inner
            .repairs
            .keep_bot_device(workspace, installation, key);
        // A record's repair has read what it keeps by now, and left what this
        // bot made waiting for a device that was not here yet: it goes in now.
        self.retry_kept_records(workspace);
    }

    /// Try every record of `workspace` whose repair keeps changes again, for a
    /// bot's device that came after the host read them
    /// ([`Workspaces::keep_bot_device`]).
    pub(super) fn retry_kept_records(&self, workspace: &str) {
        let Some(root) = self.repaired_records_root() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let workspaces = self.downgrade();
        let wanted = workspace.to_owned();
        runtime.spawn(async move {
            for (workspace, app, record) in kept_records(&root) {
                if workspace != wanted {
                    continue;
                }
                let Some(live) = workspaces.upgrade() else {
                    return;
                };
                live.take_in_kept(&root, &workspace, &app, &record).await;
            }
        });
    }

    /// What `message`, a member's write of `record` of `app` in `workspace`,
    /// carried that the record's repair could take in, decided from what its
    /// kept changes wait for and the write alone: the record itself is not
    /// read ([`Repairs`]).
    pub(super) async fn carried_into(
        &self,
        governed: &GovernedWorkspace,
        (workspace, app, record): (&str, &str, &str),
        message: &neighbour_wire::NeighbourMsg,
    ) -> Carried {
        let Some(root) = self.repaired_records_root() else {
            return Carried::Nothing;
        };
        let Some(waits) = self.inner.repairs.of(&root, (workspace, app, record)) else {
            // Its archive has not been read here yet, or keeps nothing: the
            // write is tried, which reads what it keeps now, and after this
            // only writes like it.
            return if repaired_record_dir(&root, workspace, app, record).is_dir() {
                Carried::Unknown
            } else {
                Carried::Nothing
            };
        };
        if !waits.any() {
            // Read here, and nothing of what it keeps is waiting for a change
            // any write carries.
            return Carried::Nothing;
        }
        let Some(carried) = carried_changes(governed, (workspace, app, record), message).await
        else {
            return Carried::Unknown;
        };
        if waits.released_by(&carried) {
            Carried::Changes(carried)
        } else {
            Carried::Nothing
        }
    }

    /// Write every kept change of `record` of `app` in `workspace` whose
    /// dependencies the record holds into it as the host's own change, and
    /// take it out of the archive under `root`, with any the record holds
    /// already. Logs what it could not do; the kept changes stay then.
    async fn take_in_kept(&self, root: &Path, workspace: &str, app: &str, record: &str) {
        #[cfg(test)]
        count_try(workspace, app, record);
        let dir = repaired_record_dir(root, workspace, app, record);
        let taken = self.take_in_repaired(&dir, workspace, app, record).await;
        // What the record keeps from now on is what a write to it is tested
        // against. Kept as it was when the pass failed: the write that brought
        // the pass is the one to try, and the next one it can release is.
        match &taken {
            Ok(_) => self.inner.repairs.keep(root, (workspace, app, record)),
            Err(error) => tracing::warn!(
                %workspace,
                %app,
                %record,
                %error,
                "changes a record's repair left out were not taken in; they stay kept"
            ),
        }
        if let Ok(taken) = taken
            && (taken.merged > 0 || taken.held > 0)
        {
            tracing::info!(
                %workspace,
                %app,
                %record,
                merged = taken.merged,
                held = taken.held,
                waiting = taken.waiting,
                "changes a record's repair left out are in the record again"
            );
        }
    }

    /// [`Self::take_in_kept`] for the changes kept in `dir`.
    ///
    /// # Errors
    ///
    /// A kept change does not read, the record does not read, or the host's
    /// write of it was refused.
    async fn take_in_repaired(
        &self,
        dir: &Path,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<TakenIn, String> {
        let kept = read_kept(dir)?;
        if kept.is_empty() {
            return Ok(TakenIn::default());
        }
        // A change a bot made showing a name other than its own that the
        // record held in the clear goes in as that bot's own write: a member
        // holds such a change to the bot it names, so the host's write of it
        // is refused, its own admission included. One the record held sealed
        // is decided without that check, and goes in as any kept change does.
        let sealed = self.record_is_sealed(workspace, app, record).await;
        let (shown, own): (Vec<Kept>, Vec<Kept>) = kept
            .into_iter()
            .partition(|kept| !sealed && shown_by(&kept.change).is_some());
        let mut taken = TakenIn::default();
        // What the record holds by now, whichever write took it in.
        let mut done: BTreeSet<ChangeHash> = BTreeSet::new();
        if !own.is_empty() {
            let governor = self.serving_certificate().clone();
            let changes: Vec<Change> = own.iter().map(|kept| kept.change.clone()).collect();
            let decided = self
                .author_room_record(workspace, &governor, (app, record), move |document| {
                    merge_ready(document, changes)
                })
                .await
                .map_err(|error| error.to_string())?;
            for kept in &own {
                let hash = kept.change.hash();
                if decided.merged.contains(&hash) {
                    taken.merged = taken.merged.saturating_add(1);
                } else if decided.held.contains(&hash) {
                    taken.held = taken.held.saturating_add(1);
                } else {
                    continue;
                }
                done.insert(hash);
            }
        }
        for (installation, changes) in by_bot(&shown) {
            let hashes: Vec<ChangeHash> = changes.iter().map(|kept| kept.change.hash()).collect();
            match self
                .take_in_as_bot(workspace, (app, record), &installation, &changes)
                .await
            {
                Ok(()) => {
                    taken.merged = taken.merged.saturating_add(hashes.len());
                    done.extend(hashes);
                }
                Err(error) => tracing::warn!(
                    %workspace,
                    %app,
                    %record,
                    bot = %installation,
                    %error,
                    "changes a record's repair left out that a bot showed another name in stay kept"
                ),
            }
        }
        for kept in own.iter().chain(&shown) {
            if !done.contains(&kept.change.hash()) {
                taken.waiting = taken.waiting.saturating_add(1);
                continue;
            }
            std::fs::remove_file(&kept.path)
                .map_err(|error| format!("{}: {error}", kept.path.display()))?;
        }
        if taken.waiting == 0 {
            // Nothing of the record is kept any more: its directory goes, and
            // the ones above it that it left empty.
            let mut empty = Some(dir);
            while let Some(at) = empty
                && std::fs::remove_dir(at).is_ok()
            {
                empty = at
                    .parent()
                    .filter(|parent| !parent.ends_with(REPAIRED_RECORDS));
            }
        }
        Ok(taken)
    }

    /// Whether `record` of `app` is written sealed in `workspace`: its writes
    /// are ciphertext, and a member decides what they say only once it opens
    /// them.
    async fn record_is_sealed(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        let governed = state.lock().await;
        governed.peer().publication().is_sealed_record(app, record)
    }

    /// Publish `changes`, kept changes a bot showed a name other than its own
    /// in, as the write of the bot `installation` that made them.
    ///
    /// # Errors
    ///
    /// This host does not run that bot, so it holds no key to sign them with,
    /// or the workspace refused them as the bot's.
    async fn take_in_as_bot(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        installation: &str,
        changes: &[Kept],
    ) -> Result<(), String> {
        let Some(key) = self.inner.repairs.bot_device(workspace, installation) else {
            return Err("this host runs no such bot".into());
        };
        let governor = self.serving_certificate().clone();
        let bytes: Vec<u8> = changes
            .iter()
            .flat_map(|kept| kept.change.raw_bytes().to_vec())
            .collect();
        self.publish_as_bot(
            workspace,
            &governor,
            &key,
            &tilo_agent_participant::AppCatalogue::native(),
            app,
            record,
            &bytes,
        )
        .await
        .map_err(|error| error.to_string())
    }
}

/// The bot installation a kept change says made it showing a name other than
/// its own, which a member holds the change's author to as the bot it names.
fn shown_by(change: &Change) -> Option<String> {
    let invoker = tilo_actions::ActionContext::from_change_message(change.message()?)?.invoker?;
    if invoker.kind != tilo_actions::ActorKind::Bot || invoker.appearance.is_none() {
        return None;
    }
    Some(
        invoker
            .actor
            .strip_prefix("bot:")
            .unwrap_or(&invoker.actor)
            .to_owned(),
    )
}

/// `kept` by the bot installation each change says made it showing a name
/// other than its own.
fn by_bot(kept: &[Kept]) -> BTreeMap<String, Vec<Kept>> {
    let mut bots: BTreeMap<String, Vec<Kept>> = BTreeMap::new();
    for kept in kept {
        let Some(installation) = shown_by(&kept.change) else {
            continue;
        };
        let change = Kept {
            path: kept.path.clone(),
            change: kept.change.clone(),
        };
        bots.entry(installation).or_default().push(change);
    }
    bots
}

/// Take in, for each member write that lands, its record's kept changes, once
/// per record however many writes of it came at once, and only for the records
/// whose kept changes a write could release something from.
async fn follow_writes(
    workspaces: &WeakWorkspaces,
    root: &Path,
    writes: &mut tokio::sync::broadcast::Receiver<RecordWrite>,
) {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    loop {
        // What each record's writes landed together carried, in one try each.
        let mut tried = BTreeMap::new();
        match writes.recv().await {
            Ok(write) => landed(&mut tried, write),
            // Writes went by unread: every record kept is tried.
            Err(RecvError::Lagged(_)) => every_kept(&mut tried, root),
            Err(RecvError::Closed) => return,
        }
        loop {
            match writes.try_recv() {
                Ok(write) => landed(&mut tried, write),
                Err(TryRecvError::Lagged(_)) => every_kept(&mut tried, root),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return,
            }
        }
        for ((workspace, app, record), carried) in tried {
            if carried == Carried::Nothing {
                continue;
            }
            let Some(live) = workspaces.upgrade() else {
                return;
            };
            live.take_in_kept(root, &workspace, &app, &record).await;
        }
    }
}

/// What `write` adds to what `tried` holds for its record.
fn landed(tried: &mut BTreeMap<Record, Carried>, write: RecordWrite) {
    let key = record_key(&write.workspace, &write.app, &write.record);
    match tried.get_mut(&key) {
        Some(carried) => carried.and_later(write.carried),
        None => {
            tried.insert(key, write.carried);
        }
    }
}

/// Every record with changes kept under `root`'s archive, tried.
fn every_kept(tried: &mut BTreeMap<Record, Carried>, root: &Path) {
    for (workspace, app, record) in kept_records(root) {
        tried
            .entry(record_key(&workspace, &app, &record))
            .and_modify(|carried| carried.and_later(Carried::Unknown))
            .or_insert(Carried::Unknown);
    }
}

/// The changes `message`, a member's write of `(app, record)` in `workspace`,
/// carried, read from the write itself where it is in the clear and opened
/// with the record's channel keys where it is sealed; `None` when they cannot
/// be read here.
async fn carried_changes(
    governed: &GovernedWorkspace,
    (workspace, app, record): (&str, &str, &str),
    message: &neighbour_wire::NeighbourMsg,
) -> Option<Vec<LandedChange>> {
    let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = message else {
        return None;
    };
    let mutation =
        match collab_types::validate_whole_document_payload_route(app, workspace, record, envelope)
            .ok()?
        {
            collab_types::WholeDocumentPayload::Cleartext(mutation) => mutation,
            collab_types::WholeDocumentPayload::Encrypted { .. } => {
                // Sealed for the record's channel: opened here where this host
                // holds its keys, and read nowhere else.
                let opened = governed
                    .open_write(app, record, envelope)
                    .await
                    .ok()
                    .flatten()?;
                collab_types::DocumentMutation::decode(&opened).ok()?
            }
        };
    let changes = collab_types::parse_changes(mutation.data()).ok()?;
    Some(
        changes
            .iter()
            .map(|change| LandedChange {
                hash: change.hash(),
                writer: change.actor_id().clone(),
                seq: change.seq(),
            })
            .collect(),
    )
}

/// Every record with changes kept under `root`'s archive, as
/// `(workspace, app, record)`.
fn kept_records(root: &Path) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for workspace in directories(&root.join(REPAIRED_RECORDS)) {
        for app in directories(&workspace) {
            let mut records = Vec::new();
            holding_changes(&app, &mut records);
            for dir in records {
                let Ok(relative) = dir.strip_prefix(&app) else {
                    continue;
                };
                let record = relative.to_string_lossy().into_owned();
                let record = if record == PRIMARY {
                    String::new()
                } else {
                    record
                };
                found.push((name(&workspace), name(&app), record));
            }
        }
    }
    found
}

/// The directories under `dir`, `dir` included, that hold a kept change: a
/// record's name may hold a `/`, which nests its directory.
fn holding_changes(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut holds = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            holding_changes(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension == CHANGE)
        {
            holds = true;
        }
    }
    if holds {
        found.push(dir.to_path_buf());
    }
}

fn directories(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The changes kept in `dir`, each as it was written.
fn read_kept(dir: &Path) -> Result<Vec<Kept>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|extension| extension != CHANGE) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let change =
            Change::from_bytes(bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        kept.push(Kept { path, change });
    }
    Ok(kept)
}

/// Which kept changes [`merge_ready`] merged, and which the record held.
#[derive(Default)]
struct Decided {
    merged: BTreeSet<ChangeHash>,
    held: BTreeSet<ChangeHash>,
}

/// Merge into `document` every change of `changes` whose dependencies it
/// holds, round by round, so one that builds on another kept change follows
/// it. A change the document holds is named held; one whose writer and
/// sequence the document holds under another change, or that waits for one
/// it lacks, is left out.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the authoring closure owns the kept changes it is handed"
)]
fn merge_ready(document: &mut AutoCommit, changes: Vec<Change>) -> Result<Decided, String> {
    let mut decided = Decided::default();
    let mut written: HashMap<ActorId, u64> = HashMap::new();
    for change in document.get_changes(&[]) {
        let seq = written.entry(change.actor_id().clone()).or_default();
        *seq = (*seq).max(change.seq());
    }
    let mut waiting: BTreeMap<ChangeHash, Change> = BTreeMap::new();
    for change in changes {
        if document.get_change_by_hash(&change.hash()).is_some() {
            decided.held.insert(change.hash());
        } else {
            waiting.insert(change.hash(), change);
        }
    }
    loop {
        let ready: Vec<ChangeHash> = waiting
            .iter()
            .filter(|(_, change)| {
                let next = written
                    .get(change.actor_id())
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
                change.seq() == next
                    && change
                        .deps()
                        .iter()
                        .all(|dep| document.get_change_by_hash(dep).is_some())
            })
            .map(|(hash, _)| *hash)
            .collect();
        if ready.is_empty() {
            return Ok(decided);
        }
        for hash in ready {
            let Some(change) = waiting.remove(&hash) else {
                continue;
            };
            written.insert(change.actor_id().clone(), change.seq());
            document
                .apply_incoming([change])
                .map_err(|error| error.to_string())?;
            decided.merged.insert(hash);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{ROOT, ReadDoc as _};

    fn edit(document: &mut AutoCommit, key: &str) -> Change {
        document.put(ROOT, key, key).unwrap();
        document.commit();
        document.get_last_local_change().unwrap()
    }

    #[test]
    fn a_kept_change_merges_once_what_it_builds_on_is_in_the_record() {
        let mut writer = AutoCommit::new();
        let base = edit(&mut writer, "base");
        let missing = edit(&mut writer, "missing");
        let waiting = edit(&mut writer, "waiting");
        let after = edit(&mut writer, "after");
        let mut record = AutoCommit::new();
        record.apply_incoming([base]).unwrap();

        let decided = merge_ready(&mut record, vec![after.clone(), waiting.clone()]).unwrap();
        assert!(decided.merged.is_empty() && decided.held.is_empty());
        assert!(record.get(ROOT, "waiting").unwrap().is_none());

        record.apply_incoming([missing]).unwrap();
        let decided = merge_ready(&mut record, vec![after.clone(), waiting.clone()]).unwrap();
        assert_eq!(
            decided.merged,
            BTreeSet::from([waiting.hash(), after.hash()]),
            "the kept change that builds on another kept one follows it"
        );
        assert!(record.get(ROOT, "after").unwrap().is_some());
        let again = merge_ready(&mut record, vec![after.clone()]).unwrap();
        assert_eq!(again.held, BTreeSet::from([after.hash()]));
    }

    /// `document` with a change made by `invoker`, named in its message.
    fn edit_by(document: &mut AutoCommit, invoker: tilo_actions::ActionInvoker) -> Change {
        document.put(ROOT, "shown", 1).unwrap();
        let context = tilo_actions::ActionContext::default().with_invoker(invoker);
        document.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(context.change_message().unwrap()),
        );
        document.get_last_local_change().unwrap()
    }

    #[test]
    fn only_a_bot_change_that_shows_a_name_is_the_bots_to_publish() {
        use tilo_actions::{ActionInvoker, ActorKind, Appearance};

        let shown = Appearance::new("Ada", None, None).unwrap();
        let bot_showing = edit_by(
            &mut AutoCommit::new(),
            ActionInvoker::actor("bot:7f1c")
                .with_kind(ActorKind::Bot)
                .with_appearance(Some(shown.clone())),
        );
        assert_eq!(
            shown_by(&bot_showing).as_deref(),
            Some("7f1c"),
            "the bot a change shows another name for is the one that publishes it"
        );
        let bot_itself = edit_by(
            &mut AutoCommit::new(),
            ActionInvoker::actor("bot:7f1c").with_kind(ActorKind::Bot),
        );
        assert_eq!(shown_by(&bot_itself), None, "a bot posting as itself");
        let person = edit_by(
            &mut AutoCommit::new(),
            ActionInvoker::actor("person:ada").with_appearance(Some(shown)),
        );
        assert_eq!(shown_by(&person), None, "only a bot shows another name");
        let unnamed = edit_by(&mut AutoCommit::new(), ActionInvoker::actor("person:ada"));
        assert_eq!(shown_by(&unnamed), None);
    }

    #[test]
    fn a_kept_change_of_a_sequence_the_record_holds_otherwise_stays() {
        let mut writer = AutoCommit::new();
        let base = edit(&mut writer, "base");
        let mut fork = writer.fork();
        fork.set_actor(writer.get_actor().clone());
        let kept = edit(&mut writer, "kept");
        let other = edit(&mut fork, "other");
        assert_eq!(
            (kept.actor_id(), kept.seq()),
            (other.actor_id(), other.seq())
        );
        let mut record = AutoCommit::new();
        record.apply_incoming([base, kept]).unwrap();
        let decided = merge_ready(&mut record, vec![other]).unwrap();
        assert!(decided.merged.is_empty() && decided.held.is_empty());
        assert!(record.get(ROOT, "other").unwrap().is_none());
    }
}
