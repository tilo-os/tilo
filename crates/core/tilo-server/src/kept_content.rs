//! The content the records of the workspaces this host governs reference,
//! kept so that a member which comes up later reads it from this host alone.
//!
//! A record never carries its binaries: a picture, an attachment, a file's
//! bytes live in the content store beside it, and the record names them by
//! reference — a `Bytes` value as `blake3:<hash>:<len>` (sealed, with the
//! ciphertext's reference), an action's output as `tilo.content:<reference>`,
//! a Files node by its hash. Only these name content: a value that merely
//! looks like an address, as a device key or an id does, names nothing.
//! The device whose member stored them answers whoever asks the workspace,
//! while it runs; a device that leaves — a page that reloads, a VM whose host
//! was replaced, a laptop gone offline — takes the only copy with it.
//!
//! So the host keeps them. It reads each record as a write reaches this host
//! ([`Workspaces::document_writes`]), and every record of a workspace when it
//! starts governing it ([`Workspaces::workspace_document_routes`]), with the
//! keys this host holds as a member of its channels
//! ([`Workspaces::resource_document`]). Only the written record is read, so a
//! write costs one record, and a member who joins costs only the records the
//! host could not open yet.
//!
//! For each address it lacks the host asks the workspace's members on its own
//! presence ([`Workspaces::ask_members`]), tagged by the record that named it
//! so a page keeping it for that app answers. A file's content is its manifest
//! and the chunks it lists, so a file over a megabyte arrives too. Each blob
//! is kept only when its bytes hash to the address asked for
//! ([`ContentWants`]).
//!
//! Each blob is asked for once. A member that keeps none of it says so and
//! hands it over once it keeps it ([`Workspaces::content_kept`]): a record
//! names content a moment before its writer keeps the bytes where it serves
//! them from, and asking it again brings no second word that it lacks it.
//! So what members said they lack is asked for again at once only when a
//! member that may hold it is reached or joins, at a pace the links carry
//! ([`ASKS_PER_TICK`]), and otherwise after a long silence, in case a
//! holder's answer was lost ([`neighbour_wire::content::LACKED_PATIENCE_MS`]).
//! An ask nothing answers at all, the ask or its answer lost on a link that
//! fell behind, lapses: the record that named it is read again, further apart
//! each time, until the host keeps all it names ([`LAPSED_LONGEST`]). The host
//! keeps at most [`OPEN_WANTS`] asks open and asks for the rest of a tree as
//! answers make room, rather than dropping an ask whose answer is on its way.
//!
//! Sealed content is asked by its ciphertext's address,
//! which the store keys it under: the host keeps and serves the ciphertext,
//! and only a record's readers hold its key.
//!
//! A record the host cannot open yet — its workspace's channels hand it the
//! keys after it joins them — is read again in a moment, further apart each
//! time. A person's private record is theirs: the host keeps its ciphertext and
//! never reads it. The shared desktop's layout is left to the keeper that owns
//! it: it names no content and is written with every window moved.
//!
//! A record of any app is read alike: a file it names is kept whoever posted it.
//! A file a member posts in Chat is named there like another record's picture,
//! by its `tilo-blob://` address or as a picture paragraph's content, and it
//! stays in the workspace after the member's device leaves, as a feedback
//! report's picture does.
//!
//! What it keeps goes into the workspace's store ([`crate::content`]), from
//! where the host answers members asking for content
//! ([`Workspaces::content_answer`]): a member that joins after the writer left
//! hydrates the same bytes from this host. A blob a record dropped stays until
//! nothing in the workspace references it ([`crate::content::collection`]).
//!
//! What a host keeps is bounded by policy, not by a constant
//! ([`tilo_action_policy::KeptLimits`]). The workspace's owners say how
//! large one file may be and what one member's files may take, now and in any
//! hour ([`tilo_action_policy::ContentPolicy`], read from the workspace's
//! governance); the host's person says what the host keeps for one workspace
//! together, on its server's entry among their servers. Both are watched as a
//! view watches a resource — the policy as its workspace's governance moves,
//! the entry as its record is written — so a raised limit takes effect at once
//! and nothing polls. Past a limit the member who posted the content is
//! refused, in a record of the workspace naming the limit and who can raise it
//! ([`tilo_workspace_service::workspaces::ContentRefusalRecord`]): the record
//! itself is untouched and syncs between the members' devices as any does,
//! while the host keeps the extra content no more, nor counts it as lacking.
//! The refusal is kept in the workspace's store, so a host that restarts
//! refuses the same content no second time. Raising a limit asks for what it
//! now allows again, and the refusal is withdrawn once that content is kept.
//! What the host keeps is counted by the member whose answer brought it and the
//! hour it came, in a ledger beside the content ([`ledger`]), read back when
//! the host opens a workspace: a restart never frees a member's share again.
//! The disk bounds it too ([`tilo_content::ContentError::DiskLow`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _, ScalarValueRef, Value, ValueRef};
use fs_content::{MANIFEST_PREFIX_LEN, begins_manifest};
use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::{Arrived, ContentWants};
use tilo_fs::content::Manifest;
use tilo_lock::MutexRecover as _;

use tilo_action_policy::content::{ContentLimit, ContentPolicy, KeptLimits};
use tilo_actions::{Actor, MemberId, PersonId};

use crate::workspaces::{ContentKeeper, ContentRefusal, ResourceWrite, Workspaces};
use tilo_content::{ContentStore, WorkspaceStores};
use tilo_workspace_service::workspaces::ContentRefusalRecord;

mod ledger;

/// A record of a governed workspace: the workspace's id, its app's and the
/// record's wire name (`""` for the app's primary).
type Route = (String, String, String);

/// How often the limits are looked at; the records written to since the last
/// read are read then too, though a write wakes their read itself.
const READ_EVERY: Duration = Duration::from_secs(2);
/// How often the content being gathered is looked at for the pieces a lost
/// one left missing, so a member's answer arrives whole without waiting for
/// the next write to the record that named it.
const MISSING_EVERY: Duration = Duration::from_millis(100);
/// The most asks one look at the content being gathered sends: a holder
/// reached asks again for all the members said they lack, which for a large
/// tree is thousands of blobs, and each ask is answered by every member.
const ASKS_PER_TICK: usize = 64;
/// The content requests the host keeps open at once: enough to keep a link to
/// a member busy, few enough that each is answered well within its patience
/// ([`neighbour_wire::content::PIECE_PATIENCE_MS`]) rather than asked again
/// while it waits behind the others. What a tree names past it waits here,
/// and is asked for as each answer makes room ([`State::unasked`]).
pub const OPEN_WANTS: usize = 1_024;
/// How long a record that could not be read waits before it is read again, at
/// first: each failed read waits twice as long, up to [`RETRY_LONGEST`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
/// The longest wait between two reads of a record that does not open.
const RETRY_LONGEST: Duration = Duration::from_secs(60);
/// The longest wait between two reads of a record whose asks lapsed
/// unanswered while it still names content the host lacks: the first read
/// again comes [`READ_EVERY`] after the ask lapsed, each one after that
/// twice as late, so content no member serves costs a read every ten
/// minutes, not a stream of asks.
const LAPSED_LONGEST: Duration = Duration::from_secs(600);
/// The window a member's hourly intake bounds what the host takes in of their
/// files over.
const HOUR_MS: u64 = 3_600_000;

/// The limits the host keeps content within: each workspace's own policy, as
/// its governance last said it, and what its person lets it keep for one
/// workspace. Both are read when the host opens a workspace and again as the
/// governance of its policy or the entry of its budget moves, never on a timer.
#[derive(Clone, Debug)]
struct Limits {
    /// Bytes of kept content each workspace may take; `None` when its person
    /// set no budget, which is no limit of its own.
    workspace_bytes: Option<u64>,
    /// The policy each governed workspace set, by workspace; the default for
    /// one whose owners set none.
    policies: BTreeMap<String, ContentPolicy>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            workspace_bytes: None,
            policies: BTreeMap::new(),
        }
    }
}

impl Limits {
    /// The limits `workspace` keeps its content within.
    fn of(&self, workspace: &str) -> KeptLimits {
        KeptLimits::of(
            self.policies
                .get(workspace)
                .unwrap_or(&ContentPolicy::default()),
            self.workspace_bytes,
        )
    }
}

/// The bytes a member's kept content takes, and what it took in this hour.
#[derive(Clone, Debug, Default)]
struct Taken {
    /// What the member's files the host keeps take together.
    kept: u64,
    /// What the host took in of their files within the hour: when, and how
    /// many bytes.
    this_hour: Vec<(u64, u64)>,
}

impl Taken {
    /// What the member took in within the hour ending at `now`, the bytes
    /// older than the hour let go.
    fn within_hour(&mut self, now: u64) -> u64 {
        self.this_hour
            .retain(|(at, _)| now.saturating_sub(*at) < HOUR_MS);
        self.this_hour
            .iter()
            .map(|(_, bytes)| *bytes)
            .fold(0, u64::saturating_add)
    }
}

/// When a record the host could not read is read again, and how long it will
/// wait after the next read that does not open either.
struct Retry {
    at: Instant,
    wait: Duration,
}

/// Who `workspace`'s devices belong to, and who owns it, as it last said: the
/// readers a refusal of a member's content is kept for.
#[derive(Clone, Debug, Default)]
struct People {
    owner: Option<[u8; 32]>,
    of_device: BTreeMap<[u8; 32], [u8; 32]>,
}

/// What the host holds of the content its workspaces' records name.
struct State {
    /// Content asked for and not whole yet, named by the record that
    /// references it.
    wants: ContentWants<Route>,
    /// Records a member's write reached since the last read: the write wakes
    /// their read, rather than the retry coming due.
    written: BTreeSet<Route>,
    /// Content that arrived whole and verified, by workspace, waiting to be
    /// written to the store with whatever else arrived meanwhile.
    arrivals: Vec<(String, Arrived<Route>)>,
    /// The addresses of the content in [`Self::arrivals`] or being written:
    /// a record read meanwhile does not ask for them again.
    storing: BTreeSet<String>,
    /// Records that did not open, and when to read them again: the keys of a
    /// record a workspace seals arrive after its host joins the channel that
    /// seals it.
    reopened: BTreeMap<Route, Retry>,
    /// Records an ask for whose content lapsed unanswered, and when to read
    /// them again: until the host keeps all they name.
    lapsed: BTreeMap<Route, Retry>,
    /// Content the records name past what the open asks had room for, in the
    /// order they named it, with the record that did: asked for as answers
    /// make room ([`KeptContent::ask_unasked`]).
    unasked: std::collections::VecDeque<(Route, String)>,
    /// The addresses in [`Self::unasked`].
    unasked_hashes: BTreeSet<String>,
    /// The workspaces whose records were all read: a change of their members
    /// reads again only what did not open.
    read: BTreeSet<String>,
    /// How many blobs of what each record names the host lacks, for the
    /// records that name some it lacks: what the workspace's copy status says
    /// this host lacks, kept as each record is read again.
    lacking: BTreeMap<Route, usize>,
    /// The addresses each workspace's host keeps all of: an address names the
    /// same bytes forever, so content kept whole is never looked over again.
    /// Without this, every blob that arrives reads the whole Files tree's
    /// manifests and checks every chunk they list.
    whole: BTreeMap<String, BTreeSet<String>>,
    /// The content refused past a limit, by workspace and hash: the record
    /// the workspace keeps of it, as the host last read or wrote it. It is not
    /// asked for again, nor counted as lacking, until the limit that refused
    /// it is raised.
    refused: BTreeMap<String, BTreeMap<String, ContentRefusalRecord>>,
    /// The content the host keeps by workspace and hash: the member whose
    /// answer it kept it from, and how many bytes it is.
    kept: BTreeMap<(String, String), ([u8; 32], u64)>,
    /// What the host took in of each member's files, by workspace and device.
    taken: BTreeMap<String, BTreeMap<[u8; 32], Taken>>,
    /// What the content the host keeps of each workspace takes together.
    kept_bytes: BTreeMap<String, u64>,
    /// The workspaces whose ledger of kept content was read back: its totals
    /// count what the store holds, not only what arrived since the start.
    counted: BTreeSet<String>,
    /// Who each workspace's devices belong to, as it last said: the poster a
    /// refusal names, and the people who read it.
    people: BTreeMap<String, People>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            wants: ContentWants::with_room(OPEN_WANTS),
            written: BTreeSet::new(),
            arrivals: Vec::new(),
            storing: BTreeSet::new(),
            reopened: BTreeMap::new(),
            lapsed: BTreeMap::new(),
            unasked: std::collections::VecDeque::new(),
            unasked_hashes: BTreeSet::new(),
            read: BTreeSet::new(),
            lacking: BTreeMap::new(),
            whole: BTreeMap::new(),
            refused: BTreeMap::new(),
            kept: BTreeMap::new(),
            taken: BTreeMap::new(),
            kept_bytes: BTreeMap::new(),
            counted: BTreeSet::new(),
            people: BTreeMap::new(),
        }
    }
}

/// The content a governed workspace's records reference, kept by the host that
/// governs it.
pub struct KeptContent {
    workspaces: Workspaces,
    /// The device the workspaces keep as: the one their certificate names, whose
    /// requests name it and whose answers are addressed to it.
    device: [u8; 32],
    store: Arc<WorkspaceStores>,
    /// What the host keeps of each workspace's content, by member and hour:
    /// the totals the limits count, read back when a workspace opens.
    ledger: ledger::Ledger,
    state: Mutex<State>,
    /// The limits the host keeps content within, as its workspaces' governance
    /// and its person's server entry last said them.
    limits: Mutex<Limits>,
    /// Poked when content arrived to be written ([`State::arrivals`]).
    arrived: tokio::sync::Notify,
    /// Poked when what is being gathered is to be asked for again at once: a
    /// link to a workspace's members started ([`ContentKeeper::relinked`]), or
    /// a large file's answer ended short of the file
    /// ([`ContentWants::take_continued`]).
    ask_now: tokio::sync::Notify,
    /// Poked when a write reached a record this host reads for its content
    /// ([`State::written`]).
    written: tokio::sync::Notify,
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

/// Whether the content a record of `app` names is kept by another keeper: the
/// shared desktop's layout, which names none and is written with every window
/// moved.
fn another_keeper(app: &str) -> bool {
    app == tilo_desktop_model::ownership::DESKTOP_APP
}

/// How many blobs of the content the records of `workspace` name its host
/// does not keep yet, or `None` while one of those records does not read here:
/// what a host can say it holds whole before the last device with a copy
/// goes away.
pub async fn lacking(
    workspaces: &Workspaces,
    store: &WorkspaceStores,
    workspace: &str,
) -> Option<usize> {
    counted(
        workspaces,
        store,
        workspace,
        Count::Lacked,
        &BTreeSet::new(),
    )
    .await
    .map(|tally| tally.blobs)
}

/// What [`counted`] counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Count {
    /// The blobs this host lacks; a resource that does not open here makes
    /// the count unknown.
    Lacked,
    /// The blobs this host lacks; a resource that does not open here counts
    /// for none, or for one while it waits for the keys that open it when
    /// `waiting` (its content then is not known yet). A member's own resource
    /// and one this host keeps only the ciphertext of never open here.
    LackedOpening { waiting: bool },
    /// The blobs the resources that open here name, kept here or not.
    Named,
}

/// What [`counted`] found.
struct Tally {
    /// The blobs counted.
    blobs: usize,
    /// The content counted as lacked here, by address.
    lacked: BTreeSet<String>,
}

/// How many blobs of the content the resources of `workspace` name, as
/// `count` says, leaving out what `beside` addresses.
async fn counted(
    workspaces: &Workspaces,
    store: &WorkspaceStores,
    workspace: &str,
    count: Count,
    beside: &BTreeSet<String>,
) -> Option<Tally> {
    let store = store.get(workspace).ok()?;
    let mut blobs = 0_usize;
    let mut lacked = BTreeSet::new();
    for (app, record) in workspaces.workspace_document_routes(workspace).await.ok()? {
        if another_keeper(&app) {
            continue;
        }
        let opened = workspaces.resource_document(workspace, &app, &record).await;
        let addresses = match opened
            .as_ref()
            .map(|bytes| bytes.as_deref().map(|bytes| content_of(&app, bytes)))
        {
            Ok(Some(Ok(addresses))) => addresses,
            Ok(Some(Err(_))) | Err(_) if count == Count::Lacked => return None,
            Ok(None | Some(Err(_))) => continue,
            Err(_) => {
                // Its keys reach this host as it is let into the resource's
                // channel: until they do, what it names is not known.
                if count == (Count::LackedOpening { waiting: true })
                    && !tilo_action_policy::roles::is_private_record(&app, &record)
                    && workspaces.waits_for_keys(workspace, &app, &record).await
                {
                    blobs = blobs.saturating_add(1);
                }
                continue;
            }
        };
        for address in addresses {
            if beside.contains(&address) {
                continue;
            }
            if count == Count::Named {
                blobs = blobs.saturating_add(1);
                continue;
            }
            let missing = missing_pieces(&store, &address).len();
            if missing > 0 {
                blobs = blobs.saturating_add(missing);
                lacked.insert(address);
            }
        }
    }
    Some(Tally { blobs, lacked })
}

impl KeptContent {
    /// A host that keeps what the records of `workspaces` name in each workspace's
    /// store of `store`, asking and answered as the device they are certified as.
    #[must_use]
    pub fn new(workspaces: Workspaces, store: Arc<WorkspaceStores>) -> Arc<Self> {
        let device = workspaces.device_cert().device_public_key;
        Arc::new(Self {
            ledger: ledger::Ledger::in_dir(store.dir().join(ledger::DIRECTORY)),
            workspaces,
            device,
            store,
            state: Mutex::default(),
            limits: Mutex::default(),
            arrived: tokio::sync::Notify::new(),
            ask_now: tokio::sync::Notify::new(),
            written: tokio::sync::Notify::new(),
        })
    }

    /// Keep what the workspaces this host governs name from now on: what their
    /// records hold already, then each record as it changes, until the
    /// workspaces are no longer governed here.
    pub async fn keep_content(self: Arc<Self>) {
        let keeper: Arc<dyn ContentKeeper> = self.clone();
        self.workspaces.keep_content(&keeper);
        let writing = tokio::spawn(Arc::clone(&self).keep_arrivals());
        let _writing = AbortOnDrop(writing);
        // Subscribed before the first read, so a write that lands while the
        // records are read is read too.
        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
        let mut reached = self.workspaces.holders_reached();
        let servers = self.servers_workspace();
        self.read_kept_bytes().await;
        self.read_all().await;
        let mut read = ticker(READ_EVERY);
        let mut reopen = ticker(RETRY_FIRST);
        let mut missing = ticker(MISSING_EVERY);
        loop {
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => {
                        // The host's person changed what it keeps for its
                        // workspaces: the entry is watched as a view watches a
                        // resource, so a raised budget is obeyed at once.
                        if write.app == tilo_workspace_service::workspaces::servers::SERVERS.0
                            && write.workspace == servers
                        {
                            self.read_kept_bytes().await;
                            self.limits_changed().await;
                        }
                        self.written(&write);
                    }
                    // Missed writes are read from the records themselves.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                // A workspace that appeared, or whose membership or governance
                // moved: its content policy may have moved with its settings.
                change = governance.recv() => match change {
                    Ok(workspace) => self.governance_moved(&workspace).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                reached = reached.recv() => match reached {
                    Ok(workspace) => self.holder_reached(&workspace).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        for summary in self.workspaces.list().await {
                            self.holder_reached(&summary.workspace).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                // A write wakes the read of what it reached; the writes that
                // land while those records are read are read together next.
                () = self.written.notified() => {
                    for route in self.written_since() {
                        self.keep(&route).await;
                    }
                }
                _ = read.tick() => {
                    for route in self.written_since() {
                        self.keep(&route).await;
                    }
                }
                _ = reopen.tick() => {
                    for route in self.reopening(Instant::now()) {
                        self.keep(&route).await;
                    }
                }
                _ = missing.tick() => self.ask_again_for_missing(ASKS_PER_TICK).await,
                () = self.ask_now.notified() => self.ask_again_for_missing(usize::MAX).await,
            }
        }
    }

    /// The content of `workspace` the host stopped keeping because a limit
    /// said so, read from the workspace's store: each refusal names the limit,
    /// what it allowed, and who can raise it, so the member who posted it
    /// reads why.
    pub async fn refusals(&self, workspace: &str) -> Vec<ContentRefusal> {
        let limits = self.limits_of(workspace);
        self.workspaces
            .refusals(workspace)
            .await
            // A limit raised since refuses nothing: its record stays until the
            // content it refused is kept, and stands for no reader meanwhile.
            .into_iter()
            .filter(|refusal| !limits.allows(refusal.limit, refusal.bytes))
            .map(|refusal| refusal.read())
            .collect()
    }

    /// The limits the host keeps `workspace`'s content within, as its owners'
    /// governance and this host's person's server entry last said them: what a
    /// person reading the workspace's settings is shown, and what a raised
    /// limit is obeyed by.
    #[must_use]
    pub fn limits_of(&self, workspace: &str) -> KeptLimits {
        self.limits.lock_recover().of(workspace)
    }

    /// The Servers workspace of this host's person: whose record says what the
    /// host keeps for one workspace.
    fn servers_workspace(&self) -> String {
        tilo_server_protocol::servers::person_workspace_id(
            &self.workspaces.device_person(),
        )
    }

    /// Read what this host's person lets it keep for one workspace from its own
    /// entry among their servers.
    async fn read_kept_bytes(&self) {
        // A host's budget is its person's alone: no entry sets none, and no
        // size is compiled in.
        let workspace_bytes = crate::servers::read(&self.workspaces)
            .await
            .ok()
            .and_then(|servers| servers.kept_bytes_of(&self.device));
        self.limits.lock_recover().workspace_bytes = workspace_bytes;
    }

    /// The content policy `workspace`'s owners set, read now and kept until
    /// their governance says it moved.
    async fn policy_of(&self, workspace: &str) -> ContentPolicy {
        if let Some(policy) = self.limits.lock_recover().policies.get(workspace) {
            return *policy;
        }
        let policy = self
            .workspaces
            .content_policy(workspace)
            .await
            .unwrap_or_default();
        self.limits
            .lock_recover()
            .policies
            .insert(workspace.to_owned(), policy);
        policy
    }

    /// `workspace` appeared, or a governance decision of it was made: one this
    /// host has not read yet is read whole, a member who joined may hold what
    /// the others said they lack, and a content policy its owners set or
    /// changed is obeyed at once — no timer is in it.
    async fn governance_moved(&self, workspace: &str) {
        self.limits.lock_recover().policies.remove(workspace);
        self.policy_of(workspace).await;
        if workspace == self.servers_workspace() {
            self.read_kept_bytes().await;
        }
        self.limits_changed().await;
        self.membership_moved(workspace).await;
    }

    /// The limits moved: content a raised limit now allows is asked for again,
    /// its refusal no longer enforced, and the record of that refusal is
    /// withdrawn once the content is kept. What the store no longer holds is
    /// not what the workspace keeps, so the totals count what stands.
    async fn limits_changed(&self) {
        let limits = self.limits.lock_recover().clone();
        let mut reconsider = Vec::new();
        {
            let mut state = self.state.lock_recover();
            for (workspace, refused) in &mut state.refused {
                for refusal in refused.values_mut() {
                    if refusal.withdrawn || !limits.of(workspace).allows(refusal.limit, refusal.bytes)
                    {
                        continue;
                    }
                    refusal.withdrawn = true;
                    reconsider.push((
                        workspace.clone(),
                        refusal.record.clone(),
                        refusal.hash.clone(),
                    ));
                }
            }
            for (workspace, record, hash) in reconsider {
                // The content is asked for again: what refused it no longer
                // stands, and it waits among the asks for room.
                if let Some((app, named)) = record.split_once('/') {
                    state.unasked_hashes.insert(hash.clone());
                    state.unasked.push_back((
                        (workspace.clone(), app.to_owned(), named.to_owned()),
                        hash,
                    ));
                }
            }
        }
        self.recount_kept();
    }

    /// The limit `bytes` of `member`'s content goes over in `workspace`, with
    /// what that limit allows: the file's size, the member's share, their
    /// hourly intake, or what the workspace keeps together.
    fn over(
        state: &mut State,
        limits: &KeptLimits,
        workspace: &str,
        member: &[u8; 32],
        bytes: u64,
        now: u64,
    ) -> Option<(ContentLimit, u64)> {
        if let Some(file_bytes) = limits.file_bytes
            && bytes > file_bytes
        {
            return Some((ContentLimit::File, file_bytes));
        }
        let member_taken = state.taken.entry(workspace.to_owned()).or_default();
        let taken = member_taken.entry(*member).or_default();
        if let Some(member_bytes) = limits.member_bytes
            && taken.kept.saturating_add(bytes) > member_bytes
        {
            return Some((ContentLimit::Member, member_bytes));
        }
        if let Some(hourly_bytes) = limits.hourly_bytes
            && taken.within_hour(now).saturating_add(bytes) > hourly_bytes
        {
            return Some((ContentLimit::Hourly, hourly_bytes));
        }
        let kept = state.kept_bytes.get(workspace).copied().unwrap_or(0);
        if let Some(workspace_bytes) = limits.workspace_bytes
            && kept.saturating_add(bytes) > workspace_bytes
        {
            return Some((ContentLimit::Workspace, workspace_bytes));
        }
        None
    }

    /// Count `bytes` of the content `hash` the host kept for `workspace` from
    /// `member`'s answer: their share, their intake this hour, what the
    /// workspace keeps together, and the ledger it is read back from.
    fn count_kept(
        &self,
        state: &mut State,
        workspace: &str,
        hash: &str,
        member: &[u8; 32],
        bytes: u64,
        now: u64,
    ) {
        let key = (workspace.to_owned(), hash.to_owned());
        if state.kept.contains_key(&key) {
            return;
        }
        state.kept.insert(key, (*member, bytes));
        let taken = state
            .taken
            .entry(workspace.to_owned())
            .or_default()
            .entry(*member)
            .or_default();
        taken.kept = taken.kept.saturating_add(bytes);
        taken.this_hour.push((now, bytes));
        let kept = state.kept_bytes.entry(workspace.to_owned()).or_default();
        *kept = kept.saturating_add(bytes);
        self.ledger.note(
            workspace,
            &ledger::Entry {
                hash: hash.to_ascii_lowercase(),
                member: hex::encode(member),
                bytes,
                at: now,
            },
        );
    }

    /// Read what the host kept of `workspace` back from its ledger, counting
    /// what its store holds still: a host that opens the workspace again counts
    /// every member's share and this hour's intake from what it has, not from
    /// nothing.
    fn open_ledger(&self, workspace: &str) {
        let Ok(store) = self.store.get(workspace) else {
            return;
        };
        let entries = self.ledger.read(workspace, |hash| store.has(hash));
        let mut state = self.state.lock_recover();
        state.kept.retain(|(held, _), _| held != workspace);
        state.taken.remove(workspace);
        state.kept_bytes.remove(workspace);
        for entry in entries {
            let Some(member) = hex::decode(&entry.member)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            else {
                continue;
            };
            let key = (workspace.to_owned(), entry.hash);
            if state.kept.contains_key(&key) {
                continue;
            }
            state.kept.insert(key, (member, entry.bytes));
            let taken = state
                .taken
                .entry(workspace.to_owned())
                .or_default()
                .entry(member)
                .or_default();
            taken.kept = taken.kept.saturating_add(entry.bytes);
            taken.this_hour.push((entry.at, entry.bytes));
            let kept = state.kept_bytes.entry(workspace.to_owned()).or_default();
            *kept = kept.saturating_add(entry.bytes);
        }
        state.counted.insert(workspace.to_owned());
    }

    /// Count again what the host keeps of each workspace it counted, letting go
    /// of what its store no longer holds: content a record dropped and the
    /// collection released is no longer what the workspace keeps, so the limits
    /// count what stands.
    fn recount_kept(&self) {
        let counted: Vec<String> = self.state.lock_recover().counted.iter().cloned().collect();
        for workspace in counted {
            self.open_ledger(&workspace);
        }
    }

    /// Refuse the content `hash` of `app`'s `record`, `bytes` of it, for
    /// `member`: it went over `limit`, which allowed `allowed` bytes. The
    /// record itself is untouched and syncs as any does; the host keeps the
    /// extra content no more, and says so in a record of the workspace.
    fn refuse(
        &self,
        state: &mut State,
        workspace: &str,
        app: &str,
        record: &str,
        hash: &str,
        member: [u8; 32],
        bytes: u64,
        limit: ContentLimit,
        allowed: u64,
    ) -> ContentRefusalRecord {
        let hash = hash.to_ascii_lowercase();
        tracing::info!(
            %workspace,
            %app,
            %record,
            %hash,
            bytes,
            allowed,
            limit = limit.name(),
            "content is over a limit: the host keeps it no more"
        );
        let people = state.people.get(workspace).cloned().unwrap_or_default();
        let poster = people.of_device.get(&member).copied();
        let actor = poster.map_or_else(
            || Actor::Visitor(hex::encode(member)),
            |person| Actor::Member(MemberId::person(PersonId::from_root(person))),
        );
        let mut readers = vec![self.workspaces.device_person()];
        readers.extend(people.owner);
        readers.extend(poster);
        readers.sort();
        readers.dedup();
        let refusal = ContentRefusalRecord {
            record: ContentRefusal::address(app, record),
            hash: hash.clone(),
            bytes,
            limit,
            allowed,
            member: actor,
            at: now_ms(),
            raise: limit.raises(),
            readers,
            withdrawn: false,
        };
        state
            .refused
            .entry(workspace.to_owned())
            .or_default()
            .insert(hash, refusal.clone());
        refusal
    }

    /// Write `refusals` into their workspace's store, one record each: the
    /// member who posted the content, the workspace's owners and this host's
    /// person read them, so a host that restarts refuses the same content no
    /// second time.
    fn record_refusals(&self, workspace: &str, refusals: Vec<ContentRefusalRecord>) {
        if refusals.is_empty() {
            return;
        }
        let workspaces = self.workspaces.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            for refusal in refusals {
                if let Err(error) = workspaces.record_refusal(&workspace, &refusal).await {
                    tracing::warn!(
                        %workspace,
                        hash = %refusal.hash,
                        %error,
                        "a refusal was not recorded in the workspace"
                    );
                }
            }
        });
    }

    /// Withdraw the refusal records of the content a raised limit allowed and
    /// the host kept: `refusals` no longer stand, and every device reads as
    /// much.
    fn withdraw_refusals(&self, workspace: &str, mut refusals: Vec<ContentRefusalRecord>) {
        if refusals.is_empty() {
            return;
        }
        let workspaces = self.workspaces.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            for refusal in &mut refusals {
                refusal.withdrawn = true;
                if let Err(error) = workspaces.record_refusal(&workspace, refusal).await {
                    tracing::warn!(
                        %workspace,
                        hash = %refusal.hash,
                        %error,
                        "a withdrawn refusal was not kept"
                    );
                }
            }
        });
    }

    /// Note that `write` reached a record this host reads for its content, and
    /// wake the read of it.
    fn written(&self, write: &ResourceWrite) {
        if another_keeper(&write.app) {
            return;
        }
        self.state.lock_recover().written.insert((
            write.workspace.clone(),
            write.app.clone(),
            write.record.clone(),
        ));
        self.written.notify_one();
    }

    /// Ask the workspaces again for the content being gathered that a lost
    /// piece left short: an answer carries only so much of a file at a time,
    /// and a piece a session could not deliver is not sent again by itself,
    /// so the host asks within [`neighbour_wire::content::PIECE_PATIENCE_MS`]
    /// of the last piece it got, naming what it holds.
    ///
    /// At most `limit` asks go out, and then what lapsed made room for.
    async fn ask_again_for_missing(&self, limit: usize) {
        let asks = {
            let mut state = self.state.lock_recover();
            let now = now_ms();
            for route in state.wants.lapsed(now) {
                // A record already waiting to be read again keeps its wait.
                let at = Instant::now()
                    .checked_add(READ_EVERY)
                    .unwrap_or_else(Instant::now);
                state.lapsed.entry(route).or_insert(Retry {
                    at,
                    wait: READ_EVERY,
                });
            }
            state.wants.reask_at_most(&self.device, now, limit)
        };
        for ((workspace, _, _), ask) in asks {
            if let Err(error) = self.workspaces.ask_members(&workspace, ask).await {
                tracing::debug!(%workspace, %error, "could not ask a workspace for the content it is missing");
            }
        }
        self.ask_unasked().await;
    }

    /// Ask for the content waiting for room ([`State::unasked`]) while the open
    /// asks have room for it, in the order the records named it.
    async fn ask_unasked(&self) {
        let asks = {
            let mut state = self.state.lock_recover();
            let mut asks = Vec::new();
            while state.wants.has_room() {
                let Some((route, hash)) = state.unasked.pop_front() else {
                    break;
                };
                state.unasked_hashes.remove(&hash);
                let (workspace, app, _) = &route;
                // Kept meanwhile, from another record that named it too.
                let kept = state.storing.contains(&hash)
                    || self
                        .store
                        .get(workspace)
                        .is_ok_and(|store| store.has(&hash));
                if kept {
                    continue;
                }
                if let Some(ask) = state.wants.ask(
                    &self.device,
                    route.clone(),
                    (app.as_str(), workspace.as_str()),
                    &hash,
                    now_ms(),
                ) {
                    asks.push((workspace.clone(), ask));
                }
            }
            asks
        };
        for (workspace, ask) in asks {
            if let Err(error) = self.workspaces.ask_members(&workspace, ask).await {
                tracing::debug!(%workspace, %error, "could not ask a workspace for its content");
            }
        }
    }

    /// Read every record of every workspace governed here.
    async fn read_all(&self) {
        for summary in self.workspaces.list().await {
            self.keep_workspace(&summary.workspace).await;
        }
    }

    /// The records written to since the last pass, taken.
    fn written_since(&self) -> Vec<Route> {
        std::mem::take(&mut self.state.lock_recover().written)
            .into_iter()
            .collect()
    }

    /// Write the content that arrives to the store, all that arrived since
    /// the last write at once: each write waits for the disk to hold what it
    /// wrote, and one wait for every blob would bound a workspace's files to
    /// the blobs a disk flushes in a second.
    async fn keep_arrivals(self: Arc<Self>) {
        loop {
            self.arrived.notified().await;
            let arrivals = std::mem::take(&mut self.state.lock_recover().arrivals);
            let mut by_workspace: BTreeMap<String, Vec<Arrived<Route>>> = BTreeMap::new();
            for (workspace, arrived) in arrivals {
                by_workspace.entry(workspace).or_default().push(arrived);
            }
            for (workspace, arrived) in by_workspace {
                self.keep_arrived(&workspace, arrived).await;
            }
        }
    }

    /// Write `arrived` to `workspace`'s store in one write, then ask for the
    /// chunks the manifests among it list and the store lacks, for the
    /// records that named them, and hand what was kept to the members that
    /// asked this host for it.
    async fn keep_arrived(&self, workspace: &str, arrived: Vec<Arrived<Route>>) {
        let store = match self.store.get(workspace) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%workspace, %error, "a workspace's content could not be kept");
                let mut state = self.state.lock_recover();
                for blob in &arrived {
                    state.storing.remove(&blob.hash);
                }
                return;
            }
        };
        let writing = store.clone();
        let Ok((arrived, outcomes)) = tokio::task::spawn_blocking(move || {
            let items: Vec<&[u8]> = arrived.iter().map(|blob| blob.bytes.as_slice()).collect();
            let outcomes = writing.put_public_many(&items);
            (arrived, outcomes)
        })
        .await
        else {
            return;
        };
        let mut asks = Vec::new();
        let mut kept = Vec::new();
        let mut withdrawn = Vec::new();
        {
            let mut state = self.state.lock_recover();
            for (blob, outcome) in arrived.into_iter().zip(outcomes) {
                state.storing.remove(&blob.hash);
                if let Err(error) = outcome {
                    tracing::warn!(hash = %blob.hash, %error, "a record's content could not be kept");
                    continue;
                }
                // A manifest says what the file is made of: the chunks it
                // lists that the store lacks are asked for now, for the
                // record that named it. Reading that record again to find
                // them would cost every file it names for each one that
                // arrives.
                let chunks = chunks_lacking(&store, &blob.bytes);
                let route = blob.tag;
                for hash in &chunks {
                    if let Some(ask) = ask_or_hold(&mut state, &self.device, &route, hash) {
                        asks.push(ask);
                    }
                }
                // What the record lacks: this blob less, the chunks it lists
                // more.
                if let Some(lacking) = state.lacking.get_mut(&route) {
                    *lacking = lacking.saturating_sub(1).saturating_add(chunks.len());
                    if *lacking == 0 {
                        state.lacking.remove(&route);
                    }
                }
                // A refusal of this content no longer stands: its limit was
                // raised and the content is kept now.
                if let Some(held) = state.refused.get_mut(workspace)
                    && let Some(refusal) = held.remove(&blob.hash)
                    && refusal.withdrawn
                {
                    withdrawn.push(refusal);
                }
                kept.push(blob.hash);
            }
        }
        for ask in asks {
            if let Err(error) = self.workspaces.ask_members(workspace, ask).await {
                tracing::debug!(%workspace, %error, "could not ask a workspace for a file's chunks");
            }
        }
        if !withdrawn.is_empty() {
            self.withdraw_refusals(workspace, withdrawn);
        }
        // What was kept made room among the open asks.
        self.ask_unasked().await;
        self.tell_lacking(workspace).await;
        // The members that asked this host while it kept none of it.
        for hash in kept {
            self.workspaces.content_kept(workspace, &hash);
        }
    }

    /// The records due to be read again, `now`. A record whose asks lapsed is read again twice as late as the last
    /// time, until it names nothing the host lacks.
    fn reopening(&self, now: Instant) -> Vec<Route> {
        let mut state = self.state.lock_recover();
        let mut due: Vec<Route> = state
            .reopened
            .iter()
            .filter(|(_, retry)| retry.at <= now)
            .map(|(route, _)| route.clone())
            .collect();
        for (route, retry) in &mut state.lapsed {
            if retry.at <= now {
                due.push(route.clone());
                retry.wait = retry.wait.saturating_mul(2).min(LAPSED_LONGEST);
                retry.at = now.checked_add(retry.wait).unwrap_or(now);
            }
        }
        drop(state);
        due
    }

    /// `workspace` appeared, or its membership moved. One this host has not
    /// read yet is read whole. A member who joined may hold what the others
    /// said they lack: that is asked for again. For one it has, a member who joins or leaves
    /// changes no record, only which of them this host opens: each record that
    /// did not open is read again now, and one it cannot open yet is asked
    /// about before its members write to it. Reading every record again
    /// would make each admission cost the whole workspace.
    async fn membership_moved(&self, workspace: &str) {
        self.state.lock_recover().wants.wake(workspace);
        let not_opened: Option<Vec<Route>> = {
            let state = self.state.lock_recover();
            state.read.contains(workspace).then(|| {
                state
                    .reopened
                    .keys()
                    .filter(|(held, _, _)| held == workspace)
                    .cloned()
                    .collect()
            })
        };
        match not_opened {
            Some(routes) => {
                for route in routes {
                    self.keep(&route).await;
                }
            }
            None => self.keep_workspace(workspace).await,
        }
    }

    /// Read every record of `workspace` the host can read, and ask for the
    /// content they name: what a workspace that appeared, whose membership
    /// moved or whose holder was reached leaves the host to look over again.
    /// Its policy, its people, the refusals it keeps and what the host keeps of
    /// it are read back first, so a restart opens it as the host left it.
    async fn keep_workspace(&self, workspace: &str) {
        let routes = match self.workspaces.workspace_document_routes(workspace).await {
            Ok(routes) => routes,
            Err(error) => {
                tracing::debug!(%workspace, %error, "a workspace's records are not readable here");
                self.state.lock_recover().read.remove(workspace);
                return;
            }
        };
        self.policy_of(workspace).await;
        if let Some((owner, of_device)) = self.workspaces.people_of(workspace).await {
            self.state
                .lock_recover()
                .people
                .insert(workspace.to_owned(), People { owner, of_device });
        }
        let refused = self.workspaces.refusals(workspace).await;
        let refused = refused.into_iter().fold(BTreeMap::new(), |mut held, refusal| {
            held.insert(refusal.hash.clone(), refusal);
            held
        });
        {
            let mut state = self.state.lock_recover();
            state.read.insert(workspace.to_owned());
            state.refused.insert(workspace.to_owned(), refused);
        }
        self.open_ledger(workspace);
        let routes: BTreeSet<Route> = routes
            .into_iter()
            .filter(|(app, _)| !another_keeper(app))
            .map(|(app, record)| (workspace.to_owned(), app, record))
            .collect();
        for route in &routes {
            self.keep(route).await;
        }
        // A record the workspace no longer has names nothing it lacks.
        let gone: Vec<Route> = self
            .lacked(workspace)
            .into_iter()
            .filter(|route| !routes.contains(route))
            .collect();
        for route in gone {
            self.count(&route, 0).await;
        }
    }

    /// The records of `workspace` that name content this host lacks.
    fn lacked(&self, workspace: &str) -> Vec<Route> {
        let state = self.state.lock_recover();
        state
            .lacking
            .range((workspace.to_owned(), String::new(), String::new())..)
            .take_while(|((held, _, _), _)| held == workspace)
            .map(|(route, _)| route.clone())
            .collect()
    }

    /// Note that `route`'s record names `missing` blobs this host lacks, and
    /// when that changes what its workspace lacks, say so in the workspace's
    /// copy status: as content arrives, not only when the whole workspace is
    /// read over.
    async fn count(&self, route: &Route, missing: usize) {
        let workspace = &route.0;
        {
            let mut state = self.state.lock_recover();
            let before = if missing == 0 {
                state.lacking.remove(route)
            } else {
                state.lacking.insert(route.clone(), missing)
            };
            if before.unwrap_or(0) == missing {
                return;
            }
        };
        self.tell_lacking(workspace).await;
    }

    /// Say in `workspace`'s copy status how many blobs its records name that
    /// this host lacks, as the records were last counted.
    async fn tell_lacking(&self, workspace: &str) {
        let total = self
            .state
            .lock_recover()
            .lacking
            .range((workspace.to_owned(), String::new(), String::new())..)
            .take_while(|((held, _, _), _)| held == workspace)
            .map(|(_, blobs)| *blobs)
            .fold(0, usize::saturating_add);
        let blobs = u32::try_from(total).unwrap_or(u32::MAX);
        self.workspaces.lacks_content(workspace, blobs).await;
    }

    /// Ask again for all `workspace` lacks: its copy turned current with
    /// another holder, which may keep what the members said they lack. The
    /// asks go out paced ([`ASKS_PER_TICK`]), not all at once.
    async fn holder_reached(&self, workspace: &str) {
        self.state.lock_recover().wants.wake(workspace);
        self.keep_workspace(workspace).await;
    }

    /// Read `route`, note that it opened, ask for the content it names, and
    /// count what it lacks of it.
    async fn keep(&self, route: &Route) {
        let (workspace, app, record) = route;
        let bytes = match self
            .workspaces
            .resource_document(workspace, app, record)
            .await
        {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                // Nobody wrote it: it is read when its first write lands.
                self.state.lock_recover().reopened.remove(route);
                return self.count(route, 0).await;
            }
            Err(error) => {
                tracing::debug!(%workspace, %app, %record, %error, "a governed workspace's record is not readable yet");
                if tilo_action_policy::roles::is_private_record(app, record) {
                    // A record the policy keeps for one person alone: never asked to open.
                    self.state.lock_recover().reopened.remove(route);
                    return self.count(route, 0).await;
                }
                if self.workspaces.waits_for_keys(workspace, app, record).await {
                    self.workspaces.ask_to_open(workspace, app, record).await;
                }
                // What it lacks stands as it was last read.
                self.reopen(route);
                return;
            }
        };
        self.state.lock_recover().reopened.remove(route);
        let Ok(addresses) = content_of(app, &bytes) else {
            tracing::debug!(%workspace, %app, %record, "a record of a governed workspace does not read");
            return self.count(route, 0).await;
        };
        let missing = self.ask_for(route, addresses).await;
        if missing == 0 {
            self.state.lock_recover().lapsed.remove(route);
        }
        self.count(route, missing).await;
    }

    /// Note that `route` did not read, and read it again shortly: further
    /// apart each time it does not open.
    fn reopen(&self, route: &Route) {
        let mut state = self.state.lock_recover();
        let wait = state.reopened.get(route).map_or(RETRY_FIRST, |retry| {
            retry.wait.saturating_mul(2).min(RETRY_LONGEST)
        });
        state.reopened.insert(
            route.clone(),
            Retry {
                at: Instant::now()
                    .checked_add(wait)
                    .unwrap_or_else(Instant::now),
                wait,
            },
        );
    }

    /// The requests the host sends for the content of `route`'s record it
    /// lacks and has not asked for lately; the number of blobs it lacks.
    async fn ask_for(&self, route: &Route, addresses: Vec<String>) -> usize {
        let (workspace, _, _) = route;
        let Ok(store) = self.store.get(workspace) else {
            return 0;
        };
        let known = self
            .state
            .lock_recover()
            .whole
            .get(workspace)
            .cloned()
            .unwrap_or_default();
        let (now_whole, lacking) = still_missing(&store, &known, addresses);
        let (asks, missing) = {
            let mut state = self.state.lock_recover();
            state
                .whole
                .entry(workspace.clone())
                .or_default()
                .extend(now_whole);
            let refused: BTreeSet<String> = state
                .refused
                .get(workspace)
                .into_iter()
                .flatten()
                .filter(|(_, refusal)| !refusal.withdrawn)
                .map(|(hash, _)| hash.clone())
                .collect();
            let mut asks = Vec::new();
            let mut missing = 0_usize;
            for hashes in lacking {
                for hash in hashes {
                    // Content a limit refused is the host's answer already: it
                    // is neither asked for again nor counted as lacking.
                    if refused.contains(&hash) {
                        continue;
                    }
                    missing = missing.saturating_add(1);
                    if state.storing.contains(&hash) {
                        // Arrived, and being written.
                        continue;
                    }
                    if let Some(ask) = ask_or_hold(&mut state, &self.device, route, &hash) {
                        asks.push(ask);
                    }
                }
            }
            drop(state);
            (asks, missing)
        };
        for ask in asks {
            if let Err(error) = self.workspaces.ask_members(workspace, ask).await {
                tracing::debug!(%workspace, %error, "could not ask a workspace for its content");
            }
        }
        missing
    }
}

/// The ask for `hash`, which `route` names, when the open asks have room for
/// it; otherwise it waits for room ([`State::unasked`]), since asking now would
/// drop an open ask whose answer is on its way. `None` too for content asked
/// for lately ([`ContentWants::ask`]).
fn ask_or_hold(state: &mut State, device: &[u8; 32], route: &Route, hash: &str) -> Option<Vec<u8>> {
    if !state.wants.has_room() {
        if state.unasked_hashes.insert(hash.to_owned()) {
            state.unasked.push_back((route.clone(), hash.to_owned()));
        }
        return None;
    }
    let (workspace, app, _) = route;
    state.wants.ask(
        device,
        route.clone(),
        (app.as_str(), workspace.as_str()),
        hash,
        now_ms(),
    )
}

impl ContentKeeper for KeptContent {
    fn relinked(&self, workspace: &str) {
        self.state.lock_recover().wants.relinked(workspace);
        self.ask_now.notify_one();
    }

    fn piece(&self, workspace: &str, from: [u8; 32], frame: &[u8]) {
        let limits = self.limits_of(workspace);
        let now = now_ms();
        let mut state = self.state.lock_recover();
        let mut refused = Vec::new();
        if let Ok(NeighbourMsg::DocumentContentPiece {
            app_id,
            hash,
            total,
            ..
        }) = NeighbourMsg::decode(frame)
        {
            let hash = hash.to_ascii_lowercase();
            if let Some(refusal) = state
                .refused
                .get(workspace)
                .and_then(|held| held.get(&hash))
            {
                if !refusal.withdrawn && !limits.allows(refusal.limit, refusal.bytes) {
                    return;
                }
                // The limit that refused it was raised: it is kept now, if its
                // poster answers again, and its refusal is withdrawn then.
                state
                    .refused
                    .get_mut(workspace)
                    .and_then(|held| held.get_mut(&hash))
                    .map(|refusal| refusal.withdrawn = true);
            }
            if let Some(file_bytes) = limits.file_bytes
                && total > file_bytes
                && state.wants.expects(&self.device, frame)
            {
                refused.push(self.refuse(
                    &mut state,
                    workspace,
                    &app_id,
                    "",
                    &hash,
                    from,
                    total,
                    ContentLimit::File,
                    file_bytes,
                ));
                drop(state);
                self.record_refusals(workspace, refused);
                return;
            }
        }
        if state.wants.lacks(&self.device, frame) {
            return;
        }
        let arrived = state.wants.piece(&self.device, frame, now);
        if state.wants.take_continued() {
            self.ask_now.notify_one();
        }
        let Some(arrived) = arrived else {
            return;
        };
        let size = u64::try_from(arrived.bytes.len()).unwrap_or(u64::MAX);
        if let Some((limit, allowed)) = Self::over(&mut state, &limits, workspace, &from, size, now)
        {
            refused.push(self.refuse(
                &mut state,
                workspace,
                &arrived.tag.1,
                &arrived.tag.2,
                &arrived.hash,
                from,
                size,
                limit,
                allowed,
            ));
            drop(state);
            self.record_refusals(workspace, refused);
            return;
        }
        // Counted as it is taken in, so what arrives while earlier content is
        // still being written stays within the limits too; a write that
        // fails is let go of when the host counts again what its store holds.
        self.count_kept(&mut state, workspace, &arrived.hash, &from, size, now);
        state.storing.insert(arrived.hash.clone());
        state.arrivals.push((workspace.to_owned(), arrived));
        drop(state);
        self.arrived.notify_one();
    }

    fn expects(&self, _workspace: &str, frame: &[u8]) -> bool {
        // A file over the cap is taken in here too, so `piece` answers its
        // poster with the refusal: it is refused before any byte is kept.
        let state = self.state.lock_recover();
        state.wants.expects(&self.device, frame) || state.wants.awaits(&self.device, frame)
    }

    fn refused<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<ContentRefusal>> + Send + 'a>> {
        Box::pin(async move { self.refusals(workspace).await })
    }

    fn lacks<'a>(
        &'a self,
        workspace: &'a str,
        waiting: bool,
        beside: &'a BTreeSet<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + 'a>> {
        Box::pin(async move {
            let mut beside = beside.clone();
            beside.extend(self.refusals(workspace).await.into_iter().map(|r| r.hash));
            counted(
                &self.workspaces,
                &self.store,
                workspace,
                Count::LackedOpening { waiting },
                &beside,
            )
            .await
            .map_or(0, |tally| tally.blobs)
        })
    }

    fn lacked<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = BTreeSet<String>> + Send + 'a>> {
        Box::pin(async move {
            let beside: BTreeSet<String> = self
                .refusals(workspace)
                .await
                .into_iter()
                .map(|r| r.hash)
                .collect();
            counted(
                &self.workspaces,
                &self.store,
                workspace,
                Count::LackedOpening { waiting: false },
                &beside,
            )
            .await
            .map(|tally| tally.lacked)
            .unwrap_or_default()
        })
    }

    fn names<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + 'a>> {
        Box::pin(async move {
            counted(
                &self.workspaces,
                &self.store,
                workspace,
                Count::Named,
                &BTreeSet::new(),
            )
            .await
            .map_or(0, |tally| tally.blobs)
        })
    }
}

/// The content `document`, a saved Automerge record of `app`, names now, by
/// the references records name content with: each value written as a kept
/// `Bytes` value ([`tilo_resource::Bytes::stored_address`]) or as a
/// `tilo.content:` address ([`tilo_actions::content_hash`]), a file's
/// `tilo-blob://` address, a rich text's picture under its `blob` attribute,
/// and, in a record of the Files tree, the content of each file not in its
/// trash and each version a branch kept. A value that only looks like an
/// address (a device key, an id) names nothing: no content stands behind it to
/// keep.
///
/// # Errors
///
/// Why it does not load.
pub(crate) fn content_of(app: &str, document: &[u8]) -> Result<Vec<String>, String> {
    let doc = AutoCommit::load(document).map_err(|error| error.to_string())?;
    let mut named = BTreeSet::new();
    references_in(&doc, &ROOT, &mut named);
    if app == tilo_app_id::apps::FILE_EXPLORER {
        files_content(&doc, document, &mut named)?;
    }
    Ok(named.into_iter().collect())
}

/// Add the content every value under `obj` of `doc` names by reference to
/// `named`.
fn references_in(doc: &AutoCommit, obj: &ObjId, named: &mut BTreeSet<String>) {
    let values: Vec<(Option<ObjId>, Option<String>)> = match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => doc
            .map_range(obj, ..)
            .map(|item| value_of(item.id(), &item.key, &item.value))
            .collect(),
        Ok(ObjType::List) => doc
            .list_range(obj, ..)
            .map(|item| value_of(item.id(), "", &item.value))
            .collect(),
        Ok(ObjType::Text) | Err(_) => return,
    };
    for (child, text) in values {
        if let Some(address) = text {
            named.insert(address);
        }
        if let Some(child) = child {
            references_in(doc, &child, named);
        }
    }
}

/// An item's object, to read under it, or the content it names, kept under
/// `key` ([`tilo_automerge::rich_text::ATTR_BLOB`] names a picture's content).
fn value_of(id: ObjId, key: &str, value: &ValueRef<'_>) -> (Option<ObjId>, Option<String>) {
    match value {
        ValueRef::Object(_) => (Some(id), None),
        ValueRef::Scalar(ScalarValueRef::Str(text)) => (None, reference_address(key, text)),
        ValueRef::Scalar(_) => (None, None),
    }
}

/// The address the reference `text`, kept under `key`, names its content by,
/// as the content store keeps it: a `Bytes` reference, a `tilo.content:`
/// address, a file's `tilo-blob://` address, or, under a rich text's picture
/// attribute, the hash alone.
fn reference_address(key: &str, text: &str) -> Option<String> {
    tilo_resource::Bytes::stored_address(text)
        .or_else(|| tilo_actions::content_hash(text).and_then(tilo_content::content_address))
        .or_else(|| {
            text.strip_prefix(tilo_content::BLOB_SCHEME)
                .and_then(tilo_content::content_address)
        })
        .or_else(|| {
            (key == tilo_automerge::rich_text::ATTR_BLOB)
                .then(|| tilo_content::content_address(text))
                .flatten()
        })
}

/// Add the content the Files tree `document` (its index or one of its chunks)
/// names to `named`: each file's content, unless it is in the trash, and each
/// earlier version a branch of the workspace kept.
///
/// # Errors
///
/// The tree's nodes are malformed.
fn files_content(
    doc: &AutoCommit,
    document: &[u8],
    named: &mut BTreeSet<String>,
) -> Result<(), String> {
    let tree = fs_protocol::tree::Tree::of_document(document).map_err(|error| error.to_string())?;
    for (id, node) in &tree.nodes {
        if tree.trashed(id) {
            continue;
        }
        if let Some(address) = node.hash.as_deref().and_then(tilo_content::content_address) {
            named.insert(address);
        }
    }
    if let Ok(Some((Value::Object(ObjType::Map), aliases))) =
        doc.get(ROOT, fs_protocol::tree::FILE_VERSION_ALIASES_KEY)
    {
        for item in doc.map_range(&aliases, ..) {
            if let ValueRef::Scalar(ScalarValueRef::Str(version)) = item.value
                && fs_protocol::tree::is_file_content_hash(&version)
            {
                named.insert(version.to_string());
            }
        }
    }
    Ok(())
}

/// Of `addresses`, those `store` now keeps all of, and for each other one the
/// blobs it lacks; an address in `whole` was kept whole before and is not read.
fn still_missing(
    store: &ContentStore,
    whole: &BTreeSet<String>,
    addresses: Vec<String>,
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut now_whole = Vec::new();
    let mut lacking = Vec::new();
    for address in addresses {
        if whole.contains(&address) {
            continue;
        }
        let missing = missing_pieces(store, &address);
        if missing.is_empty() {
            now_whole.push(address);
        } else {
            lacking.push(missing);
        }
    }
    (now_whole, lacking)
}

/// The blobs of the content `address` names that `store` lacks: the content
/// itself, or, when it keeps a file's manifest, the chunks the manifest lists.
pub(crate) fn missing_pieces(store: &ContentStore, address: &str) -> Vec<String> {
    let Some(mut item) = store.open(address) else {
        return vec![address.to_owned()];
    };
    // Content kept whole that is not a file's manifest is never read past its
    // first bytes.
    let mut header = [0_u8; MANIFEST_PREFIX_LEN];
    if item.read_exact(&mut header).is_err() || !begins_manifest(&header) {
        return Vec::new();
    }
    let Some(bytes) = store.read(address) else {
        return Vec::new();
    };
    chunks_lacking(store, &bytes)
}

/// The chunks `bytes` lists that `store` lacks, when `bytes` is a file's
/// manifest; none for any other content.
fn chunks_lacking(store: &ContentStore, bytes: &[u8]) -> Vec<String> {
    if !bytes
        .get(..MANIFEST_PREFIX_LEN)
        .is_some_and(begins_manifest)
    {
        return Vec::new();
    }
    let Ok(manifest) = Manifest::decode(bytes) else {
        // A manifest that does not read names no chunk to ask for; the file
        // stays with the members that wrote it.
        tracing::debug!("a kept manifest does not read as one");
        return Vec::new();
    };
    manifest
        .chunks()
        .iter()
        .map(|chunk| hex::encode(chunk.hash))
        .filter(|chunk| !store.has(chunk))
        .collect()
}

/// A timer that keeps no backlog: a pass that came late is not made twice.
fn ticker(every: Duration) -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker
}

/// A task stopped when its holder goes: the keeper's writer ends with it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    use super::*;

    /// The saved document holding `fields`, each a string or bytes value.
    fn record_of(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        for (key, value) in fields {
            doc.put(ROOT, *key, *value).unwrap();
        }
        doc.save()
    }

    const PHOTOS: &str = "so.tilo.photos";
    const FILES: &str = tilo_app_id::apps::FILE_EXPLORER;

    /// A Files tree record holding the node `id` under `parent` with content
    /// `hash`, written by the device `author`.
    fn files_node(doc: &mut AutoCommit, id: &str, parent: &str, hash: &str, author: &str) {
        use fs_protocol::tree::{AUTHOR_KEY, HASH_KEY, NODES_KEY, PARENT_KEY};
        let nodes = match doc.get(ROOT, NODES_KEY).unwrap() {
            Some((_, nodes)) => nodes,
            None => doc.put_object(ROOT, NODES_KEY, ObjType::Map).unwrap(),
        };
        let node = doc.put_object(&nodes, id, ObjType::Map).unwrap();
        doc.put(&node, PARENT_KEY, parent).unwrap();
        doc.put(&node, HASH_KEY, hash).unwrap();
        doc.put(&node, AUTHOR_KEY, author).unwrap();
    }

    tilo_test_env::scenario! {
        fn a_record_names_the_content_its_references_name_and_nothing_else;

        phase a_bytes_value_names_the_address_its_reference_keeps;
        {
            let plain = "a1".repeat(32);
            let stored = format!("blake3:{plain}:12");
            let doc = record_of(&[("photo", stored.as_str())]);
            assert_eq!(content_of(PHOTOS, &doc).unwrap(), [plain]);
        }

        phase a_sealed_reference_names_its_ciphertext_address;
        {
            let plain = "d4".repeat(32);
            let address = "e5".repeat(32);
            let key = tilo_seal::SymmetricKey::generate().unwrap();
            let reference = tilo_content::ContentRef::sealed(&address, key).to_string();
            let stored = format!("blake3:{plain}:4096:{reference}");
            let doc = record_of(&[("photo", stored.as_str())]);
            assert_eq!(
                content_of(PHOTOS, &doc).unwrap(),
                [address],
                "the host keeps and serves the ciphertext, which is keyed by its address"
            );
        }

        phase an_actions_content_address_names_its_content;
        {
            let hash = "b2".repeat(32);
            let mut doc = AutoCommit::new();
            let images = doc.put_object(ROOT, "images", ObjType::List).unwrap();
            doc.insert(&images, 0, format!("tilo.content:{hash}")).unwrap();
            assert_eq!(content_of(PHOTOS, &doc.save()).unwrap(), [hash]);
        }

        phase a_files_blob_address_and_a_pictures_content_are_named;
        // A file a message carries is named by the `tilo-blob://` address a
        // `File` reads at, and a picture in rich text by the hash under its
        // paragraph's `blob` attribute: what a member posts in Chat, and what a
        // feedback report's window picture is.
        {
            let sent = "6a".repeat(32);
            let pictured = "7b".repeat(32);
            let mut doc = AutoCommit::new();
            let carried = doc.put_object(ROOT, "attachments", ObjType::List).unwrap();
            let attached = doc.insert_object(&carried, 0, ObjType::Map).unwrap();
            doc.put(&attached, "blob", format!("tilo-blob://{sent}"))
                .unwrap();
            let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
            let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
            let picture = doc.put_object(&blocks, "shot", ObjType::Map).unwrap();
            doc.put(&picture, "kind", "image").unwrap();
            let attrs = doc.put_object(&picture, "attrs", ObjType::Map).unwrap();
            doc.put(&attrs, tilo_automerge::rich_text::ATTR_BLOB, pictured.as_str())
                .unwrap();
            assert_eq!(
                content_of(tilo_app_id::apps::CHAT, &doc.save()).unwrap(),
                [sent, pictured]
            );
        }

        phase a_value_that_only_looks_like_an_address_names_nothing;
        // A device key, an id, a hash a record keeps for its own reasons:
        // nothing stands behind them for the host to keep, and asking the
        // workspace for them would leave it lacking content forever.
        {
            let key = "f6".repeat(32);
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "device", key.as_str()).unwrap();
            doc.put(ROOT, "host", [7_u8; 32].to_vec()).unwrap();
            let entries = doc.put_object(ROOT, "servers", ObjType::Map).unwrap();
            doc.put_object(&entries, key.as_str(), ObjType::Map).unwrap();
            let note = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
            doc.splice_text(&note, 0, 0, &format!("see {key} and blake3:{key}:9"))
                .unwrap();
            doc.put(ROOT, "label", format!("key {key}")).unwrap();
            assert!(content_of(PHOTOS, &doc.save()).unwrap().is_empty());
        }

        phase a_files_node_names_its_content_and_not_the_device_that_wrote_it;
        {
            let hash = "39".repeat(32);
            let trashed = "28".repeat(32);
            let device = "4a".repeat(32);
            let mut doc = AutoCommit::new();
            files_node(&mut doc, "device/1", fs_protocol::tree::ROOT_ID, &hash, &device);
            files_node(&mut doc, "device/2", fs_protocol::tree::TRASH_ID, &trashed, &device);
            assert_eq!(
                content_of(FILES, &doc.save()).unwrap(),
                std::slice::from_ref(&hash),
                "a file in the trash is not the workspace's any more, and its author names no bytes"
            );
            assert!(
                content_of(PHOTOS, &doc.save()).unwrap().is_empty(),
                "a node's hash names content in the Files tree alone"
            );
        }

        phase a_version_a_branch_kept_is_named;
        {
            let kept = "5b".repeat(32);
            let mut doc = AutoCommit::new();
            let aliases = doc
                .put_object(ROOT, fs_protocol::tree::FILE_VERSION_ALIASES_KEY, ObjType::Map)
                .unwrap();
            doc.put(&aliases, "6c".repeat(32), kept.as_str()).unwrap();
            assert_eq!(content_of(FILES, &doc.save()).unwrap(), [kept]);
        }

        phase a_record_that_does_not_load_names_nothing;
        {
            assert!(content_of(PHOTOS, b"not a document").is_err());
        }
    }

    /// A file kept as chunks names them in its manifest, which a workspace
    /// whose files are sealed writes in its sealed layout: the host asks for
    /// the chunks of either, or it would keep the manifest of a large file
    /// and none of its bytes.
    #[test]
    fn the_chunks_a_kept_manifest_lists_and_the_store_lacks_are_asked_for() {
        use fs_content::{ChunkRef, ChunkTier, SealedChunks};
        let store = ContentStore::memory();
        let here = store.put(b"a chunk the host keeps already").unwrap();
        let mut here_hash = [0_u8; 32];
        hex::decode_to_slice(&here, &mut here_hash).unwrap();
        let absent = [0x7e_u8; 32];
        let chunks = vec![
            ChunkRef {
                hash: here_hash,
                len: 30,
            },
            ChunkRef {
                hash: absent,
                len: 4096,
            },
        ];
        let sealed = SealedChunks {
            group: [3; 16],
            epoch: 1,
            overhead: 28,
        };
        for manifest in [
            Manifest::new(ChunkTier::Small, chunks.clone()),
            Manifest::new_sealed(ChunkTier::Small, chunks, sealed),
        ] {
            let address = store.put(&manifest.encode()).unwrap();
            assert_eq!(
                missing_pieces(&store, &address),
                [hex::encode(absent)],
                "sealed: {}",
                manifest.sealed().is_some()
            );
        }
        assert_eq!(missing_pieces(&store, &"9d".repeat(32)), ["9d".repeat(32)]);
        assert!(missing_pieces(&store, &here).is_empty());
    }

    /// Content the host kept whole once is not read again: a pass after a blob
    /// arrives looks only at what is still missing, not at every file's
    /// manifest and chunk.
    #[test]
    fn content_kept_whole_is_not_looked_over_again() {
        use fs_content::{ChunkRef, ChunkTier};
        let store = ContentStore::memory();
        let chunk = store.put(b"every byte of a small file").unwrap();
        let mut chunk_hash = [0_u8; 32];
        hex::decode_to_slice(&chunk, &mut chunk_hash).unwrap();
        let manifest = Manifest::new(
            ChunkTier::Small,
            vec![ChunkRef {
                hash: chunk_hash,
                len: 26,
            }],
        );
        let file = store.put(&manifest.encode()).unwrap();
        let absent = "9d".repeat(32);

        let (whole, lacking) =
            still_missing(&store, &BTreeSet::new(), vec![file.clone(), absent.clone()]);
        assert_eq!(whole, [file.clone()]);
        assert_eq!(lacking, [vec![absent.clone()]]);

        // Remembered as whole, it is skipped without opening the store.
    /// A person's own folder is the workspace's storage: with no policy and no
    /// host budget, a 3 GiB file is over no limit, nor is one member's share.
    #[test]
    fn with_no_policy_a_three_gibibyte_file_is_over_no_limit() {
        use tilo_action_policy::content::MIB;
        let kept = BTreeMap::new();
        let limits = KeptLimits::default();
        assert_eq!(
            KeptContent::over(&kept, &limits, &[1_u8; 32], 3 * 1024 * MIB, 0),
            None
        );
        for limit in [
            ContentLimit::File,
            ContentLimit::Member,
            ContentLimit::Hourly,
            ContentLimit::Workspace,
        ] {
            assert!(limits.allows(limit, 3 * 1024 * MIB), "{limit:?}");
        }
    }
}
