//! Hydration: bringing a room record this device lacks from a device of the
//! room that keeps it.
//!
//! A record id names a document only inside its room, and a document minted
//! on one device exists on another only once something brings it there.
//! Opening an id this device has never held would mint a lookalike empty
//! record whose genesis conflicts with the real one. So instead this device
//! asks the room's other devices, one at a time, over the authenticated
//! device-to-device route remote actions use
//! ([`tilo_actions::remote::RemoteActionMessage::RecordRequest`]):
//!
//! - **Asking** names only the room, the app and the record id, pieces at a
//!   time ([`tilo_actions::remote::MAX_RECORD_PIECE_BYTES`]).
//! - **Answering** is the holder's desktop, from what the app last made
//!   durable in its profile on disk, so the app need not be running. It
//!   answers only a person the room lets read that app
//!   (reading the app, under the room's policy), and never with a
//!   record that left the room.
//! - **Keeping** trusts the bytes only as far as they prove to be that live
//!   record of that room ([`ui::record::transfer::adopt_fetched`]); a record
//!   this device already holds is merged, never replaced.
//!
//! Anything that needs a record by id uses this: a collaborator's window
//! following a document someone moved to another workspace, and a window
//! continuing on another of the person's devices.
//!
//! The frame loop keeps records and presentations without waiting for the
//! disk, and the app that opens them next reads them from it. So a record,
//! or a View naming it, is [`landing`] until its writes have reached the
//! disk, and nothing opens a window on it before then: hydration says
//! [`Hydration::Waiting`], and callers that point a window at it wait too.
//!
//! Nor does the frame loop read the disk: a profile's store answers it
//! Opening while the store's own thread opens or reads a document, and the
//! loop asks the same store again on a later round ([`open_profile`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tilo_actions::remote::{
    ActionHostError, Caller, CallerPrincipal, MAX_FETCHED_RECORD_BYTES, MAX_RECORD_PIECE_BYTES,
    Pending, PendingPoll, RecordPiece, RecordQuery, RemoteActionMessage,
};

use ui::application::NativeProfile;
use ui::record::transfer::{self, Landing};
use ui::record::{RecordCheckout, RecordId};

use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{RoomId, Workspace};
#[cfg(test)]
use tilo_lock::MutexRecover as _;

/// Where the launcher keeps `app_id`'s profile in `room`: the directory the
/// app sees as `XDG_DATA_HOME`.
pub(crate) fn profile_root(data_home: &Path, room: RoomId, app_id: &str) -> PathBuf {
    let room = room.to_string();
    ::launcher::appdir::host_app_dir(
        data_home,
        &::launcher::appdir::room_slug(Some(&room)),
        app_id,
    )
    .join(".local/share")
}

/// One record, by where it lives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordAddress {
    pub room: RoomId,
    pub app_id: String,
    pub record: RecordId,
}

/// Answer one device's request for a record kept here.
///
/// # Errors
/// Why the piece is not given: the room is not hosted here, the caller may
/// not read the app, or the record is not kept here as a live record.
pub fn serve(
    workspace: Option<&Workspace>,
    data_home: Option<&Path>,
    caller: &Caller,
    query: &RecordQuery,
) -> Result<RecordPiece, ActionHostError> {
    let not_kept = || ActionHostError::Refused {
        reason: crate::shell::i18n::t("record-not-kept-here"),
    };
    let workspace = workspace
        .filter(|workspace| workspace.room().to_string() == query.room)
        .ok_or_else(not_kept)?;
    // Only a person the room lets read the app is answered; a room without
    // an owner is nobody else's to read.
    let CallerPrincipal::Person(person) = caller.principal() else {
        return Err(ActionHostError::NotPermitted);
    };
    if workspace.owner().is_none()
        || !workspace.person_allowed(
            person,
            &tilo_action_policy::TreePath::root().app(query.app.clone()),
            tilo_action_policy::ActionKind::Read,
        )
    {
        return Err(ActionHostError::NotPermitted);
    }
    let id = RecordId::parse(&query.record).ok_or_else(not_kept)?;
    let root = profile_root(
        data_home.ok_or_else(not_kept)?,
        workspace.room(),
        &query.app,
    );
    if !root.is_dir() {
        return Err(not_kept());
    }
    let profile = NativeProfile::open(&root, Some(&query.room)).map_err(|_| not_kept())?;
    let mut doc = transfer::read_by_id(profile.store(), &query.app, profile.workspace_id(), &id)
        .map_err(|_| not_kept())?;
    let bytes = doc.save();
    let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if total > MAX_FETCHED_RECORD_BYTES {
        return Err(ActionHostError::Refused {
            reason: crate::shell::i18n::t("record-too-large-to-bring"),
        });
    }
    let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
    let piece = bytes
        .get(start..)
        .and_then(|rest| rest.get(..rest.len().min(MAX_RECORD_PIECE_BYTES)))
        .ok_or_else(|| ActionHostError::InvalidRequest {
            reason: crate::shell::i18n::t("record-offset-past-end"),
        })?;
    Ok(RecordPiece::new(total, query.offset, piece))
}

/// Reaches the room's other devices for a record.
pub trait RecordSource: Send + Sync {
    /// The stored bytes of `address` from some device of its room that
    /// keeps it, or why none could give them.
    fn fetch(&self, address: &RecordAddress) -> Pending<Result<Vec<u8>, String>>;
}

/// Ask `holders` one at a time over `link`, piece by piece, until one of
/// them has given the whole record.
pub fn fetch_from<L>(
    link: Arc<L>,
    holders: Vec<(u64, iroh::EndpointId)>,
    address: &RecordAddress,
) -> Pending<Result<Vec<u8>, String>>
where
    L: crate::shell::remote_action_link::RemoteActionLink + ?Sized + 'static,
{
    let (completer, answer) = tilo_actions::remote::pending();
    ask(
        link,
        holders.into_iter().collect(),
        address.clone(),
        Vec::new(),
        completer,
    );
    answer
}

fn ask<L>(
    link: Arc<L>,
    mut holders: std::collections::VecDeque<(u64, iroh::EndpointId)>,
    address: RecordAddress,
    gathered: Vec<u8>,
    completer: tilo_actions::remote::Completer<Result<Vec<u8>, String>>,
) where
    L: crate::shell::remote_action_link::RemoteActionLink + ?Sized + 'static,
{
    static CORRELATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let Some((topic, holder)) = holders.front().copied() else {
        completer.complete(Err("no device of the room could give that record".into()));
        return;
    };
    let correlation = CORRELATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let query = RecordQuery {
        room: address.room.to_string(),
        app: address.app_id.clone(),
        record: address.record.as_str().to_owned(),
        offset: u64::try_from(gathered.len()).unwrap_or(u64::MAX),
    };
    let Ok(reply) = link.request(
        topic,
        holder,
        RemoteActionMessage::RecordRequest { correlation, query },
    ) else {
        holders.pop_front();
        return ask(link, holders, address, Vec::new(), completer);
    };
    reply.then(move |reply| {
        // No answer, a refusal or nonsense: the next device.
        let Some(RemoteActionMessage::RecordResponse {
            result: Ok(piece), ..
        }) = reply
        else {
            holders.pop_front();
            return ask(link, holders, address, Vec::new(), completer);
        };
        let mut gathered = gathered;
        let expected = u64::try_from(gathered.len()).unwrap_or(u64::MAX);
        match piece.bytes() {
            Some(bytes)
                if piece.offset == expected
                    && piece.total <= MAX_FETCHED_RECORD_BYTES
                    && (!bytes.is_empty() || expected == piece.total) =>
            {
                gathered.extend_from_slice(&bytes);
                let have = u64::try_from(gathered.len()).unwrap_or(u64::MAX);
                if have >= piece.total {
                    gathered.truncate(usize::try_from(piece.total).unwrap_or(usize::MAX));
                    completer.complete(Ok(gathered));
                } else {
                    ask(link, holders, address, gathered, completer);
                }
            }
            _ => {
                holders.pop_front();
                ask(link, holders, address, Vec::new(), completer);
            }
        }
    });
}

/// The session network as a [`RecordSource`]: the room's other devices, by
/// the endpoints its document names, on the room's topic. It holds the
/// network weakly: the source is the process's, and a session that ends
/// (a headless runtime session dropped and started again) takes its network,
/// and the state file that network holds, with it.
pub struct NetworkSource(pub std::sync::Weak<crate::shell::network::ShellNetwork>);

impl RecordSource for NetworkSource {
    fn fetch(&self, address: &RecordAddress) -> Pending<Result<Vec<u8>, String>> {
        let Some(network) = self.0.upgrade() else {
            let (completer, answer) = tilo_actions::remote::pending();
            completer.complete(Err("this session's network has ended".into()));
            return answer;
        };
        let topic = crate::shell::network::room_topic_id(address.room);
        let holders = network
            .room_devices(address.room)
            .into_iter()
            .map(|device| (topic, device))
            .collect();
        fetch_from(network, holders, address)
    }
}

fn source_slot() -> &'static Mutex<Option<Arc<dyn RecordSource>>> {
    static SOURCE: Mutex<Option<Arc<dyn RecordSource>>> = Mutex::new(None);
    &SOURCE
}

/// Reach other devices through `source` from now on.
pub fn set_source(source: Arc<dyn RecordSource>) {
    if let Ok(mut slot) = source_slot().lock() {
        *slot = Some(source);
    }
}

/// Writes for one record still on their way to the disk, with the profile
/// whose store accepted them: only that store knows how they went.
struct Arriving {
    address: RecordAddress,
    profile: Rc<NativeProfile>,
    landings: Vec<Landing>,
}

/// A profile the frame loop opened, kept while it comes back to it.
struct OpenProfile {
    root: PathBuf,
    room: RoomId,
    profile: Rc<NativeProfile>,
    used: Instant,
}

/// How long a profile nobody asked for stays open.
const PROFILE_IDLE: Duration = Duration::from_secs(60);

thread_local! {
    /// Kept on the frame loop that wrote them; a profile's store is not
    /// shared between threads.
    static ARRIVING: RefCell<Vec<Arriving>> = const { RefCell::new(Vec::new()) };
    static PROFILES: RefCell<Vec<OpenProfile>> = const { RefCell::new(Vec::new()) };
}

/// The profile of an app in `room` at `dir`, the same store each time this
/// thread asks within a minute. A thread that must not wait on the disk is
/// answered Opening while the store's own thread reads for it, and only
/// that store remembers the work: asking again must reach it.
pub(crate) fn open_profile(dir: &Path, room: RoomId) -> Option<Rc<NativeProfile>> {
    PROFILES.with(|profiles| {
        let mut profiles = profiles.borrow_mut();
        profiles.retain(|open| open.used.elapsed() < PROFILE_IDLE);
        if let Some(open) = profiles
            .iter_mut()
            .find(|open| open.room == room && open.root == dir)
        {
            open.used = Instant::now();
            return Some(Rc::clone(&open.profile));
        }
        let profile = Rc::new(NativeProfile::open(dir, Some(&room.to_string())).ok()?);
        profiles.push(OpenProfile {
            root: dir.to_path_buf(),
            room,
            profile: Rc::clone(&profile),
            used: Instant::now(),
        });
        Some(profile)
    })
}

/// Wait for `landings` in `profile` before anything opens `address`.
pub(crate) fn hold_until_landed(
    address: &RecordAddress,
    profile: Rc<NativeProfile>,
    landings: impl IntoIterator<Item = Option<Landing>>,
) {
    let landings: Vec<Landing> = landings.into_iter().flatten().collect();
    if Landing::all(profile.store().as_ref(), &landings) == durable_store::Durability::Durable {
        return;
    }
    ARRIVING.with(|arriving| {
        arriving.borrow_mut().push(Arriving {
            address: address.clone(),
            profile,
            landings,
        });
    });
}

/// Whether writes for `address` are still on their way to the disk: `Err`
/// when one failed, and nothing should open it as kept.
pub(crate) fn landing(address: &RecordAddress) -> Result<bool, String> {
    ARRIVING.with(|arriving| {
        let mut arriving = arriving.borrow_mut();
        let mut outcome = Ok(false);
        arriving.retain(|entry| {
            if &entry.address != address {
                return true;
            }
            match Landing::all(entry.profile.store().as_ref(), &entry.landings) {
                durable_store::Durability::Durable => false,
                durable_store::Durability::Pending => {
                    if outcome.is_ok() {
                        outcome = Ok(true);
                    }
                    true
                }
                durable_store::Durability::Failed(reason) => {
                    outcome = Err(reason);
                    false
                }
            }
        });
        outcome
    })
}

fn any_landing() -> bool {
    ARRIVING.with(|arriving| !arriving.borrow().is_empty())
}

/// Where a hydration stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hydration {
    /// Asked; no answer yet.
    Waiting,
    /// Kept here now, as this record.
    Kept(RecordCheckout),
    /// No device could give it, or what came was not it.
    Failed(String),
}

struct Entry {
    address: RecordAddress,
    answer: Option<Pending<Result<Vec<u8>, String>>>,
    /// An answer this device has yet to keep: its profile's store is still
    /// opening the record for a thread that may not wait.
    unkept: Option<Vec<u8>>,
    state: Hydration,
}

fn entries() -> &'static Mutex<Vec<Entry>> {
    static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    &ENTRIES
}

/// How many [`entries`] are [`Hydration::Waiting`], so the frame loop can ask
/// [`any_waiting`] every round without taking the entries' lock. Changed only
/// under that lock, in step with the entry it counts.
static WAITING: AtomicUsize = AtomicUsize::new(0);

/// Bring `address` here if it is not already being brought, and say where
/// that stands. Settled answers are kept in `data_home`'s profile for the
/// app in that room the first time they are looked at.
pub fn hydrate(address: &RecordAddress, data_home: &Path) -> Hydration {
    hydrate_from(address, data_home, || {
        source_slot().lock().ok().and_then(|slot| slot.clone())
    })
}

/// [`hydrate`], asking `source` for a record not already being brought.
fn hydrate_from(
    address: &RecordAddress,
    data_home: &Path,
    source: impl FnOnce() -> Option<Arc<dyn RecordSource>>,
) -> Hydration {
    let Ok(mut entries) = entries().lock() else {
        return Hydration::Failed("hydration is unavailable".into());
    };
    if let Some(entry) = entries.iter_mut().find(|entry| &entry.address == address) {
        let was_waiting = entry.state == Hydration::Waiting;
        settle(entry, data_home);
        if was_waiting && entry.state != Hydration::Waiting {
            WAITING.fetch_sub(1, Ordering::Release);
        }
        return once_landed(address, entry.state.clone());
    }
    let Some(source) = source() else {
        return Hydration::Failed("no other device of the room is reachable".into());
    };
    let mut entry = Entry {
        address: address.clone(),
        answer: Some(source.fetch(address)),
        unkept: None,
        state: Hydration::Waiting,
    };
    settle(&mut entry, data_home);
    let state = entry.state.clone();
    if state == Hydration::Waiting {
        WAITING.fetch_add(1, Ordering::Release);
    }
    entries.push(entry);
    once_landed(address, state)
}

/// A kept record is kept only once it has reached the disk.
fn once_landed(address: &RecordAddress, state: Hydration) -> Hydration {
    match (&state, landing(address)) {
        (Hydration::Kept(_), Ok(true)) => Hydration::Waiting,
        (Hydration::Kept(_), Err(reason)) => Hydration::Failed(reason),
        _ => state,
    }
}

/// Whether any hydration still waits for an answer or for the disk. One
/// atomic load and a thread-local check, so the frame loop may ask every
/// round.
pub fn any_waiting() -> bool {
    WAITING.load(Ordering::Acquire) > 0 || any_landing()
}

/// Whether [`WAITING`] agrees with the entries it counts. Both are read under
/// the entries' lock, which every change to either holds.
#[cfg(test)]
fn waiting_count_matches_entries() -> bool {
    let (scanned, counted) = {
        let entries = entries().lock_recover();
        (
            entries
                .iter()
                .filter(|entry| entry.state == Hydration::Waiting)
                .count(),
            WAITING.load(Ordering::Acquire),
        )
    };
    scanned == counted
}

/// Forget a settled hydration, so a later need asks again.
pub fn forget(address: &RecordAddress) {
    if let Ok(mut entries) = entries().lock() {
        entries.retain(|entry| &entry.address != address || entry.state == Hydration::Waiting);
    }
}

fn settle(entry: &mut Entry, data_home: &Path) {
    let bytes = match (entry.unkept.take(), entry.answer.as_mut()) {
        (Some(bytes), _) => bytes,
        (None, None) => return,
        (None, Some(answer)) => {
            let result = match answer.poll_ready() {
                PendingPoll::Waiting => return,
                PendingPoll::Ready(result) => result,
                PendingPoll::Abandoned => Err("the network went away".into()),
            };
            entry.answer = None;
            match result {
                Ok(bytes) => bytes,
                Err(reason) => {
                    entry.state = Hydration::Failed(reason);
                    return;
                }
            }
        }
    };
    entry.state = match keep(&entry.address, data_home, &bytes) {
        Ok(Some(checkout)) => Hydration::Kept(checkout),
        Ok(None) => {
            entry.unkept = Some(bytes);
            Hydration::Waiting
        }
        Err(reason) => Hydration::Failed(reason),
    };
}

/// Keep bytes another device sent as `address` in this device's profile.
/// The record is [`landing`] until it has reached the disk. `None` while the
/// profile's store is still opening it for a thread that may not wait on
/// the disk: keep the same bytes again later.
///
/// # Errors
/// Bytes that are not that live record, or a profile that cannot hold it.
pub fn keep(
    address: &RecordAddress,
    data_home: &Path,
    bytes: &[u8],
) -> Result<Option<RecordCheckout>, String> {
    let root = profile_root(data_home, address.room, &address.app_id);
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let profile = open_profile(&root, address.room)
        .ok_or_else(|| "this device's profile of the app cannot be opened".to_owned())?;
    let (checkout, landing) = match transfer::adopt_fetched(
        profile.store(),
        &address.app_id,
        profile.workspace_id(),
        &address.record,
        bytes,
    ) {
        Ok(kept) => kept,
        Err(error) if error.is_opening() => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    hold_until_landed(address, profile, [landing]);
    Ok(Some(checkout))
}

/// Give `instance` of `address`'s app on this device a saved presentation
/// naming `address`'s record, unless it already names it: how a window
/// opened here reopens on a record brought from elsewhere. `record` is the
/// record as kept, when the caller has it. The presentation is [`landing`]
/// until it has reached the disk; start the window after that. `false`
/// while the profile's store is still reading for a thread that may not
/// wait on the disk: ask again later.
pub fn reopen_on(
    address: &RecordAddress,
    data_home: &Path,
    instance: u64,
    record: Option<&RecordCheckout>,
) -> bool {
    let root = profile_root(data_home, address.room, &address.app_id);
    let Some(profile) = open_profile(&root, address.room) else {
        return true;
    };
    let saved = match profile.saved_view(instance) {
        Ok(saved) => saved,
        Err(error) if error.is_opening() => return false,
        Err(_) => None,
    };
    if saved
        .as_ref()
        .is_some_and(|view| view.record_id == address.record.as_str())
    {
        return true;
    }
    let record = if let Some(record) = record {
        record.clone()
    } else {
        match transfer::read_by_id(
            profile.store(),
            &address.app_id,
            profile.workspace_id(),
            &address.record,
        ) {
            Ok(doc) => match ui::record::identity(&doc) {
                Some(found) => found,
                None => return true,
            },
            Err(error) if error.is_opening() => return false,
            Err(_) => return true,
        }
    };
    let view = saved.unwrap_or_else(|| ui::application_view::SavedInstanceView {
        version: 2,
        app_id: address.app_id.clone(),
        device_id: profile.device_id().to_owned(),
        workspace_id: profile.workspace_id().to_owned(),
        instance_id: instance,
        record_id: record.id.as_str().to_owned(),
        record_kind: record.kind.as_str().to_owned(),
        view: ui::view::View::default(),
        application: None,
    });
    match profile.adopt_view(&view, instance, &record) {
        Ok(landing) => hold_until_landed(address, profile, [landing]),
        Err(error) => {
            tracing::warn!(%error, "could not point the window at the record brought over");
        }
    }
    true
}

/// The live record this device keeps as `address` already, if it does:
/// `None` while its profile's store is still reading it for a thread that
/// may not wait on the disk. Hand what it found on to [`reopen_on`], which
/// then need not read it again.
pub fn kept_here(address: &RecordAddress, data_home: &Path) -> Option<Option<RecordCheckout>> {
    let root = profile_root(data_home, address.room, &address.app_id);
    if !root.is_dir() {
        return Some(None);
    }
    let Some(profile) = open_profile(&root, address.room) else {
        return Some(None);
    };
    match transfer::read_by_id(
        profile.store(),
        &address.app_id,
        profile.workspace_id(),
        &address.record,
    ) {
        Ok(doc) => Some(ui::record::identity(&doc)),
        Err(error) if error.is_opening() => None,
        Err(_) => Some(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use std::collections::HashMap;

    const APP: &str = "dev.tilo.hydration-test";

    /// Devices of one room, answering requests the way the broker does.
    struct Room {
        devices: HashMap<iroh::EndpointId, (Workspace, PathBuf)>,
        caller: Caller,
        asked: Mutex<Vec<iroh::EndpointId>>,
    }

    impl crate::shell::remote_action_link::RemoteActionLink for Room {
        fn request(
            &self,
            _workspace_topic: u64,
            peer: iroh::EndpointId,
            message: RemoteActionMessage,
        ) -> Result<Pending<RemoteActionMessage>, String> {
            self.asked.lock().unwrap().push(peer);
            let RemoteActionMessage::RecordRequest { correlation, query } = message else {
                return Err("only records here".into());
            };
            let (workspace, home) = self.devices.get(&peer).ok_or("unreachable")?;
            Ok(Pending::ready(RemoteActionMessage::RecordResponse {
                correlation,
                result: serve(Some(workspace), Some(home), &self.caller, &query),
            }))
        }
    }

    fn device(seed: u8) -> iroh::EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// A holder device whose profile keeps one large record of the room.
    fn holder(workspace: &Workspace) -> (tempfile::TempDir, RecordCheckout, String) {
        let home = tempfile::tempdir().unwrap();
        let root = profile_root(home.path(), workspace.room(), APP);
        std::fs::create_dir_all(&root).unwrap();
        let profile = NativeProfile::open(&root, Some(&workspace.room().to_string())).unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), ui::record::RecordKind::new("test"));
        let records = ui::record::RecordOwners::new(root)
            .open(APP, profile.workspace_id())
            .unwrap();
        records.create_record(&checkout).unwrap();
        // Bigger than one piece, and not compressible into one.
        let mut seed = 0x9e37_79b9_u32;
        let body: String = (0..MAX_RECORD_PIECE_BYTES.saturating_mul(2))
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                char::from(b'a'.saturating_add(u8::try_from(seed % 26).unwrap()))
            })
            .collect();
        records
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, "body", body.as_str()).unwrap();
            })
            .unwrap();
        records.try_flush().unwrap();
        (home, checkout, body)
    }

    fn wait(mut answer: Pending<Result<Vec<u8>, String>>) -> Option<Result<Vec<u8>, String>> {
        match answer.poll_ready() {
            PendingPoll::Ready(result) => Some(result),
            _ => None,
        }
    }

    #[test]
    fn a_record_is_brought_piece_by_piece_from_a_device_that_keeps_it() {
        let caller = Caller::authenticated([7; 32], [8; 32]);
        let mut workspace = Workspace::new(crate::shell::state::WorkspaceId(1), "Shared");
        workspace.set_owner(hex::encode([7; 32]));
        let (holder_home, checkout, body) = holder(&workspace);
        let empty_home = tempfile::tempdir().unwrap();
        let mut devices = HashMap::new();
        // One device of the room keeps nothing; the next keeps the record.
        devices.insert(
            device(1),
            (workspace.clone(), empty_home.path().to_path_buf()),
        );
        devices.insert(
            device(2),
            (workspace.clone(), holder_home.path().to_path_buf()),
        );
        let room = Arc::new(Room {
            devices,
            caller,
            asked: Mutex::new(Vec::new()),
        });
        let address = RecordAddress {
            room: workspace.room(),
            app_id: APP.into(),
            record: checkout.id.clone(),
        };
        let bytes = wait(fetch_from(
            Arc::clone(&room),
            vec![(1, device(1)), (1, device(2))],
            &address,
        ))
        .expect("the in-process room answers at once")
        .expect("the second device gives it");
        let asked = room.asked.lock().unwrap().clone();
        assert_eq!(asked.first(), Some(&device(1)));
        assert!(
            asked.iter().filter(|peer| **peer == device(2)).count() >= 2,
            "a large record comes in pieces: {asked:?}"
        );

        let here = tempfile::tempdir().unwrap();
        assert_eq!(kept_here(&address, here.path()), Some(None));
        assert!(
            keep(
                &RecordAddress {
                    record: RecordId::minted(),
                    ..address.clone()
                },
                here.path(),
                &bytes
            )
            .is_err(),
            "bytes are kept only as the record they prove to be"
        );
        assert_eq!(
            keep(&address, here.path(), &bytes).unwrap(),
            Some(checkout.clone())
        );
        assert_eq!(
            kept_here(&address, here.path()),
            Some(Some(checkout.clone()))
        );
        let profile = NativeProfile::open(
            &profile_root(here.path(), workspace.room(), APP),
            Some(&workspace.room().to_string()),
        )
        .unwrap();
        let doc = transfer::read_by_id(profile.store(), APP, profile.workspace_id(), &checkout.id)
            .unwrap();
        assert_eq!(
            match automerge::ReadDoc::get(&doc, automerge::ROOT, "body") {
                Ok(Some((automerge::Value::Scalar(value), _))) => value.as_str().map(str::len),
                _ => None,
            },
            Some(body.len())
        );
        assert!(reopen_on(&address, here.path(), 44, None));
        assert_eq!(
            profile.saved_view(44).unwrap().map(|view| view.record_id),
            Some(checkout.id.as_str().to_owned()),
            "the window opened here reopens on the record brought"
        );
    }

    #[test]
    fn a_record_is_given_only_to_someone_the_room_lets_read_the_app() {
        let mut workspace = Workspace::new(crate::shell::state::WorkspaceId(1), "Shared");
        workspace.set_owner(hex::encode([7; 32]));
        let (home, checkout, _) = holder(&workspace);
        let query = RecordQuery {
            room: workspace.room().to_string(),
            app: APP.into(),
            record: checkout.id.as_str().into(),
            offset: 0,
        };
        let member = Caller::authenticated([7; 32], [8; 32]);
        let stranger = Caller::authenticated([9; 32], [8; 32]);
        assert!(serve(Some(&workspace), Some(home.path()), &member, &query).is_ok());
        assert_eq!(
            serve(Some(&workspace), Some(home.path()), &stranger, &query),
            Err(ActionHostError::NotPermitted)
        );
        let mut unowned = workspace.clone();
        unowned.policy.owner = None;
        assert_eq!(
            serve(Some(&unowned), Some(home.path()), &member, &query),
            Err(ActionHostError::NotPermitted),
            "a room nobody governs is nobody else's to read"
        );
        assert!(
            serve(
                Some(&workspace),
                Some(home.path()),
                &member,
                &RecordQuery {
                    record: RecordId::minted().as_str().into(),
                    ..query
                }
            )
            .is_err(),
            "a record not kept here is not invented"
        );
    }

    type Fetched = Result<Vec<u8>, String>;

    /// Answers each fetch when the test says so.
    #[derive(Default)]
    struct Held(Mutex<Vec<tilo_actions::remote::Completer<Fetched>>>);

    impl RecordSource for Held {
        fn fetch(&self, _address: &RecordAddress) -> Pending<Fetched> {
            let (completer, answer) = tilo_actions::remote::pending();
            self.0.lock().unwrap().push(completer);
            answer
        }
    }

    /// Answers at once, with nothing that proves to be the record.
    struct Refusing;

    impl RecordSource for Refusing {
        fn fetch(&self, _address: &RecordAddress) -> Pending<Fetched> {
            Pending::ready(Err("not kept here".into()))
        }
    }

    #[test]
    fn record_hydration_waiting_counter_tracks_entries() {
        let home = tempfile::tempdir().unwrap();
        let room = Workspace::new(crate::shell::state::WorkspaceId(2), "Shared").room();
        let address = || RecordAddress {
            room,
            app_id: APP.into(),
            record: RecordId::minted(),
        };
        let held = Arc::new(Held::default());
        let from_held = || Some(Arc::clone(&held) as Arc<dyn RecordSource>);
        let refusing = || Some(Arc::new(Refusing) as Arc<dyn RecordSource>);
        let (first, second, refused) = (address(), address(), address());

        assert_eq!(
            hydrate_from(&first, home.path(), from_held),
            Hydration::Waiting
        );
        assert!(waiting_count_matches_entries(), "a new ask is counted");
        assert!(any_waiting(), "an unanswered ask is waiting");
        assert_eq!(
            hydrate_from(&second, home.path(), from_held),
            Hydration::Waiting
        );
        assert!(waiting_count_matches_entries());
        assert_eq!(
            hydrate_from(&first, home.path(), from_held),
            Hydration::Waiting
        );
        assert!(
            waiting_count_matches_entries(),
            "looking again while unanswered counts once"
        );
        assert!(matches!(
            hydrate_from(&refused, home.path(), refusing),
            Hydration::Failed(_)
        ));
        assert!(
            waiting_count_matches_entries(),
            "an ask answered at once is never waiting"
        );
        forget(&first);
        assert!(
            waiting_count_matches_entries(),
            "forgetting keeps an ask still waiting"
        );

        let mut completers = std::mem::take(&mut *held.0.lock().unwrap());
        assert_eq!(completers.len(), 2, "one fetch per address");
        // One answers with a refusal; the other's device goes away.
        completers.remove(0).complete(Err("gone".into()));
        drop(completers);
        for asked in [&first, &second, &first] {
            assert!(matches!(
                hydrate_from(asked, home.path(), from_held),
                Hydration::Failed(_)
            ));
            assert!(
                waiting_count_matches_entries(),
                "an answer leaves the count exactly once"
            );
        }
        for asked in [&first, &second, &refused] {
            forget(asked);
            assert!(
                waiting_count_matches_entries(),
                "forgetting keeps the count"
            );
        }
    }
}
