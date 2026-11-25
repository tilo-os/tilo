//! The content the members of the workspaces this host serves push to it,
//! kept so that a member which comes up later reads it from this host alone.
//!
//! A record never carries its binaries: a picture, an attachment, a file's
//! bytes live in the content store beside it, and the record names them by
//! reference. The device that writes them pushes them here as it keeps them
//! ([`Workspaces::push_content`]), unasked, in pieces addressed to this host.
//! The host never reads a record to learn what it names: it could not read a
//! sealed one anyway, and reading every record each write lands cost a
//! workspace with many writes more than they did.
//!
//! What a member pushes is taken only from a device of the workspace's
//! members, kept only when its bytes hash to the address it was pushed as,
//! and only when the device may write the record's app. The host then says it
//! keeps it ([`NeighbourMsg::DocumentContentKept`]), and until then the
//! pushing device keeps the push going.
//!
//! Each push is its member's hold on the content. A member whose own change
//! dropped its record's last reference to it releases its hold
//! ([`NeighbourMsg::DocumentContentReleased`]); a member removed from the
//! workspace holds nothing any more. The host lets the content go once no
//! member holds it. A file kept as chunks is held by its manifest: the chunks
//! go when the manifest does.
//!
//! Sealed content is pushed by its ciphertext's address, which the store keys
//! it under: the host keeps and serves the ciphertext, and only a record's
//! readers hold its key.
//!
//! What it keeps goes into the workspace's store ([`crate::content`]), from
//! where the host answers members asking for content
//! ([`Workspaces::content_answer`]): a member that joins after the writer left
//! hydrates the same bytes from this host.
//!
//! What a host keeps is bounded by policy, not by a constant
//! ([`tilo_action_policy::KeptLimits`]). The workspace's owners say how
//! large one file may be and what one member's files may take, now and in any
//! hour ([`tilo_action_policy::ContentPolicy`], read from the workspace's
//! governance); the host's person says what the host keeps for one workspace
//! together, on its server's entry among their servers. Both are watched as a
//! view watches a resource — the policy as its workspace's governance moves,
//! the entry as its record is written — so a raised limit takes effect at once
//! and nothing polls. Past a limit the member who pushed the content is
//! refused, in a record of the workspace naming the limit and who can raise it
//! ([`tilo_workspace_service::workspaces::ContentRefusalRecord`]): the record
//! itself is untouched and syncs between the members' devices as any does,
//! while the host keeps the extra content no more. The refusal is kept in the
//! workspace's store, so a host that restarts refuses the same content no
//! second time. Once a limit is raised, the push the member keeps going is
//! kept, and the refusal withdrawn. What the host keeps is counted by the
//! member whose push brought it and the hour it came, in a ledger beside the
//! content ([`ledger`]) that holds who holds it too, read back when the host
//! opens a workspace: a restart never frees a member's share again, nor
//! forgets a hold. The disk bounds it too
//! ([`tilo_content::ContentError::DiskLow`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs_content::{MANIFEST_PREFIX_LEN, begins_manifest};
use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::{MAX_CONTENT_BYTES, PIECE_BYTES, Piece};
use tilo_fs::content::Manifest;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::peer_handle;

use tilo_action_policy::content::{ContentLimit, ContentPolicy, KeptLimits};
use tilo_actions::{Actor, MemberId, PersonId};

use crate::workspaces::{ContentHeld, ContentKeeper, ContentRefusal, Workspaces};
use tilo_content::WorkspaceStores;
use tilo_workspace_service::workspaces::ContentRefusalRecord;

mod ledger;

/// How often content being gathered is looked at for pushes that stopped.
const STALE_EVERY: Duration = Duration::from_secs(10);
/// How long a push gathered here waits for its next piece before it is let
/// go: its writer pushes it again, whole, while it is going.
const STALE_AFTER_MS: u64 = 60_000;
/// The bytes the pushes being gathered take together at most: a push past it
/// is dropped, and its writer pushes it again later.
const GATHERING_BYTES: u64 = 1 << 30;
/// The window a member's hourly intake bounds what the host takes in of their
/// files over.
const HOUR_MS: u64 = 3_600_000;
/// What a chunk of a file is held by: the file's manifest, by its address.
const MANIFEST_HOLDER: &str = "manifest:";

/// The limits the host keeps content within: each workspace's own policy, as
/// its governance last said it, and what its person lets it keep for one
/// workspace. Both are read when the host opens a workspace and again as the
/// governance of its policy or the entry of its budget moves, never on a timer.
#[derive(Clone, Debug, Default)]
struct Limits {
    /// Bytes of kept content each workspace may take; `None` when its person
    /// set no budget, which is no limit of its own.
    workspace_bytes: Option<u64>,
    /// The policy each hosted workspace set, by workspace; the default for
    /// one whose owners set none.
    policies: BTreeMap<String, ContentPolicy>,
}

impl Limits {
    /// The limits `workspace` keeps its content within.
    fn of(&self, workspace: &str) -> KeptLimits {
        match self.policies.get(workspace) {
            Some(policy) => KeptLimits::of(policy, self.workspace_bytes),
            None => KeptLimits::of(&ContentPolicy::default(), self.workspace_bytes),
        }
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

/// Who `workspace`'s devices belong to, and who owns it, as it last said: the
/// readers a refusal of a member's content is kept for.
#[derive(Clone, Debug, Default)]
struct People {
    owner: Option<[u8; 32]>,
    of_device: BTreeMap<[u8; 32], [u8; 32]>,
}

/// A push being gathered: who pushed it, for which app, and its bytes so far.
struct Gathering {
    from: [u8; 32],
    app: String,
    total: u64,
    bytes: Vec<u8>,
    pieces: BTreeSet<u64>,
    received: u64,
    touched_ms: u64,
}

/// A push that arrived whole, its bytes hashing to its address, on its way
/// to the store.
struct Arrival {
    app: String,
    hash: String,
    from: [u8; 32],
    bytes: Vec<u8>,
}

/// Content the host keeps: its app, and who holds it.
#[derive(Clone, Debug, Default)]
struct Holding {
    app: String,
    holders: BTreeSet<String>,
}

/// What the host holds of the content its workspaces' members pushed.
#[derive(Default)]
struct State {
    /// Pushes not whole yet, by workspace and hash.
    gathering: BTreeMap<(String, String), Gathering>,
    /// The bytes [`Self::gathering`] takes together.
    gathering_bytes: u64,
    /// Content that arrived whole and verified, by workspace, waiting to be
    /// written to the store with whatever else arrived meanwhile.
    arrivals: Vec<(String, Arrival)>,
    /// The addresses of the content in [`Self::arrivals`] or being written,
    /// with whoever else pushed it meanwhile: answered once it is kept.
    storing: BTreeMap<(String, String), Vec<([u8; 32], String)>>,
    /// The content each workspace keeps, by hash: its app and who holds it.
    holds: BTreeMap<String, BTreeMap<String, Holding>>,
    /// The content refused past a limit, by workspace and hash: the record
    /// the workspace keeps of it, as the host last read or wrote it.
    refused: BTreeMap<String, BTreeMap<String, ContentRefusalRecord>>,
    /// The content the host keeps by workspace and hash: the member whose
    /// push it kept it from, and how many bytes it is.
    kept: BTreeMap<(String, String), ([u8; 32], u64)>,
    /// What the host took in of each member's files, by workspace and member
    /// ([`KeptContent::member_of`]): the person a device belongs to, or the
    /// device for one that belongs to nobody the workspace knows.
    taken: BTreeMap<String, BTreeMap<[u8; 32], Taken>>,
    /// What the content the host keeps of each workspace takes together.
    kept_bytes: BTreeMap<String, u64>,
    /// The workspaces whose ledger was read back: its totals and holds count
    /// what the store holds, not only what arrived since the start.
    counted: BTreeSet<String>,
    /// Who each workspace's devices belong to, as it last said: who a push
    /// is taken from, the poster a refusal names, and the people who read it.
    people: BTreeMap<String, People>,
}

/// The content a hosted workspace's members push to the host that serves
/// it.
pub struct KeptContent {
    workspaces: Workspaces,
    /// The device the workspaces keep as: the one their certificate names, to
    /// which members address what they push.
    device: [u8; 32],
    store: Arc<WorkspaceStores>,
    /// What the host keeps of each workspace's content, by member and hour,
    /// and who holds it: read back when a workspace opens.
    ledger: ledger::Ledger,
    state: Mutex<State>,
    /// The limits the host keeps content within, as its workspaces' governance
    /// and its person's server entry last said them.
    limits: Mutex<Limits>,
    /// Poked when content arrived to be written ([`State::arrivals`]).
    arrived: tokio::sync::Notify,
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

/// One piece of content a limit refused: where it belongs and what it is.
#[derive(Clone, Copy)]
struct RefusedContent<'a> {
    workspace: &'a str,
    app: &'a str,
    record: &'a str,
    hash: &'a str,
    member: [u8; 32],
    bytes: u64,
}

impl KeptContent {
    /// A host that keeps what the members of `workspaces` push in each
    /// workspace's store of `store`, addressed as the device they are
    /// certified as.
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
        })
    }

    /// Keep what the members of the workspaces this host serves push, until
    /// the workspaces are no longer hosted here.
    pub async fn keep_content(self: Arc<Self>) {
        let keeper: Arc<dyn ContentKeeper> = self.clone();
        self.workspaces.keep_content(&keeper);
        let writing = tokio::spawn(Arc::clone(&self).keep_arrivals());
        let _writing = AbortOnDrop(writing);
        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
        let servers = self.servers_workspace();
        self.read_kept_bytes().await;
        for summary in self.workspaces.list().await {
            self.open_workspace(&summary.workspace).await;
        }
        let mut stale = ticker(STALE_EVERY);
        loop {
            tokio::select! {
                write = writes.recv() => match write {
                    // The host's person changed what it keeps for its
                    // workspaces: the entry is watched as a view watches a
                    // resource, so a raised budget is obeyed at once.
                    Ok(write) => {
                        if write.app == tilo_workspace_service::workspaces::servers::SERVERS.0
                            && write.workspace == servers
                        {
                            self.read_kept_bytes().await;
                            self.limits_changed();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_kept_bytes().await;
                        self.limits_changed();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                // A workspace that appeared, or whose membership or governance
                // moved: its content policy may have moved with its settings,
                // and a member removed holds nothing any more.
                change = governance.recv() => match change {
                    Ok(workspace) => self.governance_moved(&workspace).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        for summary in self.workspaces.list().await {
                            self.governance_moved(&summary.workspace).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = stale.tick() => self.let_stale_go(now_ms()),
            }
        }
    }

    /// The content of `workspace` the host stopped keeping because a limit
    /// said so, read from the workspace's store: each refusal names the limit,
    /// what it allowed, and who can raise it, so the member who posted it
    /// reads why.
    pub async fn refusals(&self, workspace: &str) -> Vec<ContentRefusal> {
        let kept = self.workspaces.refusals(workspace).await;
        let limits = self.limits_of(workspace);
        let now = now_ms();
        let mut state = self.state.lock_recover();
        kept.into_iter()
            // A limit raised since refuses nothing: its record stays until the
            // content it refused is kept, and stands for no reader meanwhile.
            .filter(|refusal| Self::standing(&mut state, &limits, workspace, refusal, now))
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
        tilo_server_protocol::servers::person_workspace_id(&self.workspaces.device_person())
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

    /// `workspace` appeared, or a governance decision of it was made: a
    /// content policy its owners set or changed is obeyed at once, and the
    /// holds of a member it removed are let go.
    async fn governance_moved(&self, workspace: &str) {
        self.limits.lock_recover().policies.remove(workspace);
        self.policy_of(workspace).await;
        if workspace == self.servers_workspace() {
            self.read_kept_bytes().await;
        }
        self.limits_changed();
        self.open_workspace(workspace).await;
    }

    /// Read `workspace`'s policy, its people, the refusals it keeps and what
    /// the host keeps of it and who holds it, so a restart opens it as the
    /// host left it; then let go of the holds of whoever is no member any
    /// more.
    async fn open_workspace(&self, workspace: &str) {
        self.policy_of(workspace).await;
        if let Some((owner, of_device)) = self.workspaces.people_of(workspace).await {
            self.state
                .lock_recover()
                .people
                .insert(workspace.to_owned(), People { owner, of_device });
        }
        let refused = self
            .workspaces
            .refusals(workspace)
            .await
            .into_iter()
            .map(|refusal| (refusal.hash.clone(), refusal))
            .collect();
        self.state
            .lock_recover()
            .refused
            .insert(workspace.to_owned(), refused);
        self.open_ledger(workspace);
        if let Some(members) = self.workspaces.member_people(workspace).await {
            self.drop_departed(workspace, &members);
        }
    }

    /// Let go of the holds in `workspace` of the people no longer among its
    /// `members`, and of the content no one holds then. A holder this host
    /// knows no person of is a device of its own and stays.
    fn drop_departed(&self, workspace: &str, members: &BTreeSet<[u8; 32]>) {
        let known: BTreeSet<String> = {
            let state = self.state.lock_recover();
            state
                .people
                .get(workspace)
                .map(|people| people.of_device.values().map(hex::encode).collect())
                .unwrap_or_default()
        };
        let members: BTreeSet<String> = members.iter().map(hex::encode).collect();
        let departed = |holder: &String| known.contains(holder) && !members.contains(holder);
        let changed: Vec<String> = {
            let mut state = self.state.lock_recover();
            let Some(holds) = state.holds.get_mut(workspace) else {
                return;
            };
            holds
                .iter_mut()
                .filter_map(|(hash, holding)| {
                    let before = holding.holders.len();
                    holding.holders.retain(|holder| !departed(holder));
                    (holding.holders.len() != before).then(|| hash.clone())
                })
                .collect()
        };
        for hash in changed {
            self.held_changed(workspace, &hash);
        }
    }

    /// The limits moved: a refusal a raised limit no longer stands by is
    /// withdrawn once the content it refused is kept, which its member keeps
    /// pushing. What the store no longer holds is not what the workspace
    /// keeps, so the totals count what stands.
    fn limits_changed(&self) {
        self.recount_kept();
        let limits = self.limits.lock_recover().clone();
        let now = now_ms();
        let mut state = self.state.lock_recover();
        let recorded: Vec<(String, ContentRefusalRecord)> = state
            .refused
            .iter()
            .flat_map(|(workspace, refused)| {
                refused
                    .values()
                    .filter(|refusal| !refusal.withdrawn)
                    .map(|refusal| (workspace.clone(), refusal.clone()))
            })
            .collect();
        for (workspace, refusal) in recorded {
            if Self::standing(
                &mut state,
                &limits.of(&workspace),
                &workspace,
                &refusal,
                now,
            ) {
                continue;
            }
            if let Some(held) = state
                .refused
                .get_mut(&workspace)
                .and_then(|refused| refused.get_mut(&refusal.hash))
            {
                held.withdrawn = true;
            }
        }
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

    /// The member whose share and hourly intake `device`'s answer counts
    /// against in `workspace`: the person it belongs to, as the workspace's
    /// people last said, or the device itself for one that belongs to nobody it
    /// knows. A member's devices post as one member.
    fn member_of(state: &State, workspace: &str, device: &[u8; 32]) -> [u8; 32] {
        state
            .people
            .get(workspace)
            .and_then(|people| people.of_device.get(device).copied())
            .unwrap_or(*device)
    }

    /// The member a refusal names, as the key [`Self::over`] counts against:
    /// the person who posted it, or the visitor's own device.
    fn refusal_member(actor: &Actor) -> Option<[u8; 32]> {
        let key = match actor {
            Actor::Member(member) => member.person_id()?,
            Actor::Visitor(key) => key.as_str(),
            Actor::Automation(_) | Actor::App(_) => return None,
        };
        <[u8; 32]>::try_from(hex::decode(key).ok()?).ok()
    }

    /// Whether the refusal `refusal` records still stands in `workspace` under
    /// `limits`, which is whether the host would refuse the same content now. A
    /// member's share and their hourly intake are about their files together,
    /// and the workspace's total about every file it keeps, so only the totals
    /// the host holds say whether a raised limit lets the content through. A
    /// refusal that no longer stands is asked for again, and its record
    /// withdrawn once that content is kept.
    fn standing(
        state: &mut State,
        limits: &KeptLimits,
        workspace: &str,
        refusal: &ContentRefusalRecord,
        now: u64,
    ) -> bool {
        // A poster this host counts against no share is one it cannot weigh:
        // the refusal stands until the host keeps the content and withdraws it.
        Self::refusal_member(&refusal.member).is_none_or(|member| {
            Self::over(state, limits, workspace, &member, refusal.bytes, now).is_some()
        })
    }

    /// Whether `hash`'s content of `workspace` is refused by a refusal that
    /// still stands. One whose limit was raised is withdrawn, so its poster's
    /// next answer is kept.
    fn already_refused(
        state: &mut State,
        limits: &KeptLimits,
        workspace: &str,
        hash: &str,
        now: u64,
    ) -> bool {
        let recorded = state
            .refused
            .get(workspace)
            .and_then(|held| held.get(hash))
            .cloned();
        let Some(refusal) = recorded else {
            return false;
        };
        if !refusal.withdrawn && Self::standing(state, limits, workspace, &refusal, now) {
            return true;
        }
        if let Some(held) = state
            .refused
            .get_mut(workspace)
            .and_then(|held| held.get_mut(hash))
        {
            held.withdrawn = true;
        }
        false
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
        if !Self::count_stored(state, workspace, hash, *member, bytes, now) {
            return;
        }
        self.ledger.note(
            workspace,
            &ledger::Entry {
                hash: hash.to_ascii_lowercase(),
                member: hex::encode(member),
                bytes,
                at: now,
                app: String::new(),
                held: Vec::new(),
            },
        );
    }

    /// Count `bytes` of the content `hash` as what the host keeps for
    /// `workspace` from `member`, taken in at `at`; whether it was counted
    /// already. The share, the intake this hour and the workspace's total all
    /// follow from what is counted here.
    fn count_stored(
        state: &mut State,
        workspace: &str,
        hash: &str,
        member: [u8; 32],
        bytes: u64,
        at: u64,
    ) -> bool {
        let key = (workspace.to_owned(), hash.to_owned());
        if state.kept.contains_key(&key) {
            return false;
        }
        state.kept.insert(key, (member, bytes));
        let taken = state
            .taken
            .entry(workspace.to_owned())
            .or_default()
            .entry(member)
            .or_default();
        taken.kept = taken.kept.saturating_add(bytes);
        taken.this_hour.push((at, bytes));
        let kept = state.kept_bytes.entry(workspace.to_owned()).or_default();
        *kept = kept.saturating_add(bytes);
        true
    }

    /// Read what the host kept of `workspace` back from its ledger, counting
    /// what its store holds still: a host that opens the workspace again counts
    /// every member's share and this hour's intake from what it has, not from
    /// nothing. What the keeper counted while the ledger was read is in no line
    /// of it yet, and stays counted: a piece arrives as the keeper reads the
    /// workspace, not before.
    fn open_ledger(&self, workspace: &str) {
        let Ok(store) = self.store.get(workspace) else {
            return;
        };
        let entries = self.ledger.read(workspace, |hash| store.has(hash));
        let now = now_ms();
        let mut state = self.state.lock_recover();
        let ledgered: std::collections::BTreeSet<String> =
            entries.iter().map(|entry| entry.hash.clone()).collect();
        let arrived: Vec<(String, ([u8; 32], u64))> = state
            .kept
            .iter()
            .filter(|((held, hash), _)| held == workspace && !ledgered.contains(hash))
            .map(|((_, hash), counted)| (hash.clone(), *counted))
            .collect();
        state.kept.retain(|(held, _), _| held != workspace);
        state.taken.remove(workspace);
        state.kept_bytes.remove(workspace);
        state.holds.remove(workspace);
        for entry in entries {
            if !entry.held.is_empty() {
                state.holds.entry(workspace.to_owned()).or_default().insert(
                    entry.hash.clone(),
                    Holding {
                        app: entry.app.clone(),
                        holders: entry.held.iter().cloned().collect(),
                    },
                );
            }
            let Some(member) = hex::decode(&entry.member)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            else {
                continue;
            };
            Self::count_stored(
                &mut state,
                workspace,
                &entry.hash,
                member,
                entry.bytes,
                entry.at,
            );
        }
        for (hash, (member, bytes)) in arrived {
            // Content the store no longer holds, and that is being written no
            // more, is not what the workspace keeps either.
            if !store.has(&hash)
                && !state
                    .storing
                    .contains_key(&(workspace.to_owned(), hash.clone()))
            {
                continue;
            }
            Self::count_stored(&mut state, workspace, &hash, member, bytes, now);
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
        content: &RefusedContent<'_>,
        limit: ContentLimit,
        allowed: u64,
    ) -> ContentRefusalRecord {
        let RefusedContent {
            workspace,
            app,
            record,
            hash,
            member,
            bytes,
        } = *content;
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
        readers.sort_unstable();
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
                // The workspace service's write path keeps its draft and seal
                // state on the future, so it is heap-bound here rather than
                // carried by this task's frame.
                if let Err(error) = Box::pin(workspaces.record_refusal(&workspace, &refusal)).await
                {
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
                // As [`Self::record_refusals`]: the write's future is large.
                if let Err(error) = Box::pin(workspaces.record_refusal(&workspace, refusal)).await {
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

    /// Take `pieces`, which the member device `from` pushed to this host in
    /// `workspace`: gathered until whole, checked against their address, and
    /// kept within the limits. A content kept here already is the device's
    /// member's hold on it, and answered at once.
    fn take_pushed(&self, workspace: &str, from: [u8; 32], pieces: Vec<Piece>) {
        let limits = self.limits_of(workspace);
        let now = now_ms();
        let store = self.store.get(workspace).ok();
        let mut refused = Vec::new();
        let mut held = Vec::new();
        let mut whole = Vec::new();
        {
            let mut state = self.state.lock_recover();
            // Only a device of the workspace's members pushes its content.
            if !state
                .people
                .get(workspace)
                .is_some_and(|people| people.of_device.contains_key(&from))
            {
                tracing::warn!(%workspace, from = %hex::encode(from), "content pushed by a device that is no member's is refused");
                return;
            }
            for piece in pieces {
                let hash = piece.hash.to_ascii_lowercase();
                if !tilo_content::valid_hash(&hash) {
                    continue;
                }
                let key = (workspace.to_owned(), hash.clone());
                if let Some(pushers) = state.storing.get_mut(&key) {
                    pushers.push((from, piece.app_id));
                    continue;
                }
                if store.as_ref().is_some_and(|store| store.has(&hash)) {
                    held.push((piece.app_id, hash));
                    continue;
                }
                if Self::already_refused(&mut state, &limits, workspace, &hash, now) {
                    continue;
                }
                if let Some(file_bytes) = limits.file_bytes
                    && piece.total > file_bytes
                {
                    // Refused before any byte of it is kept, so its poster reads why.
                    refused.push(self.refuse(
                        &mut state,
                        &RefusedContent {
                            workspace,
                            app: &piece.app_id,
                            record: "",
                            hash: &hash,
                            member: from,
                            bytes: piece.total,
                        },
                        ContentLimit::File,
                        file_bytes,
                    ));
                    continue;
                }
                if let Some(bytes) = Self::gather(&mut state, &key, from, piece, now) {
                    whole.push(bytes);
                }
            }
        }
        for (app, hash) in held {
            self.hold(workspace, &hash, &app, from);
            self.answer(workspace, from, &app, &hash);
        }
        // Checked against their addresses with the lock let go: hashing a
        // large file takes a while, and every other piece waits on the lock.
        let arrived: Vec<Arrival> = whole
            .into_iter()
            .filter(|arrival| {
                let verified = tilo_content::hash_bytes(&arrival.bytes) == arrival.hash;
                if !verified {
                    tracing::warn!(%workspace, hash = %arrival.hash, "pushed content does not hash to its address");
                }
                verified
            })
            .collect();
        let mut state = self.state.lock_recover();
        let mut taken = false;
        for arrived in arrived {
            let size = u64::try_from(arrived.bytes.len()).unwrap_or(u64::MAX);
            let member = Self::member_of(&state, workspace, &from);
            if let Some((limit, allowed)) =
                Self::over(&mut state, &limits, workspace, &member, size, now)
            {
                refused.push(self.refuse(
                    &mut state,
                    &RefusedContent {
                        workspace,
                        app: &arrived.app,
                        record: "",
                        hash: &arrived.hash,
                        member: from,
                        bytes: size,
                    },
                    limit,
                    allowed,
                ));
                continue;
            }
            // Counted as it is taken in, so what arrives while earlier content is
            // still being written stays within the limits too; a write that
            // fails is let go of when the host counts again what its store holds.
            self.count_kept(&mut state, workspace, &arrived.hash, &member, size, now);
            state
                .storing
                .insert((workspace.to_owned(), arrived.hash.clone()), Vec::new());
            state.arrivals.push((workspace.to_owned(), arrived));
            taken = true;
        }
        drop(state);
        if !refused.is_empty() {
            self.record_refusals(workspace, refused);
        }
        if taken {
            self.arrived.notify_one();
        }
    }

    /// Fold `piece` into the push of `key` from `from`; the push, whole, once
    /// its last piece is in. A piece that does not fit the push spoils it, and
    /// its writer pushes it again.
    fn gather(
        state: &mut State,
        key: &(String, String),
        from: [u8; 32],
        piece: Piece,
        now: u64,
    ) -> Option<Arrival> {
        if piece.total > MAX_CONTENT_BYTES
            || piece.bytes.len() > PIECE_BYTES
            || !piece.offset.is_multiple_of(PIECE_BYTES as u64)
        {
            return None;
        }
        if !state.gathering.contains_key(key) {
            if state.gathering_bytes.saturating_add(piece.total) > GATHERING_BYTES {
                return None;
            }
            let total = usize::try_from(piece.total).ok()?;
            state.gathering_bytes = state.gathering_bytes.saturating_add(piece.total);
            state.gathering.insert(
                key.clone(),
                Gathering {
                    from,
                    app: piece.app_id.clone(),
                    total: piece.total,
                    bytes: vec![0; total],
                    pieces: BTreeSet::new(),
                    received: 0,
                    touched_ms: now,
                },
            );
        }
        let gathering = state.gathering.get_mut(key)?;
        let start = usize::try_from(piece.offset).ok()?;
        let end = start.checked_add(piece.bytes.len());
        let fits = gathering.from == from
            && gathering.total == piece.total
            && end.is_some_and(|end| end <= gathering.bytes.len());
        if !fits {
            let spoiled = state.gathering.remove(key)?;
            state.gathering_bytes = state.gathering_bytes.saturating_sub(spoiled.total);
            return None;
        }
        gathering.touched_ms = now;
        if gathering.pieces.insert(piece.offset) {
            let end = start.saturating_add(piece.bytes.len());
            gathering
                .bytes
                .get_mut(start..end)?
                .copy_from_slice(&piece.bytes);
            gathering.received = gathering.received.saturating_add(piece.bytes.len() as u64);
        }
        if gathering.received < gathering.total {
            return None;
        }
        let whole = state.gathering.remove(key)?;
        state.gathering_bytes = state.gathering_bytes.saturating_sub(whole.total);
        Some(Arrival {
            app: whole.app,
            hash: key.1.clone(),
            from,
            bytes: whole.bytes,
        })
    }

    /// Let go of the pushes no piece came for since long before `now`: their
    /// writers push them again, whole.
    fn let_stale_go(&self, now: u64) {
        let mut state = self.state.lock_recover();
        let stale: Vec<(String, String)> = state
            .gathering
            .iter()
            .filter(|(_, gathering)| now.saturating_sub(gathering.touched_ms) >= STALE_AFTER_MS)
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if let Some(gone) = state.gathering.remove(&key) {
                state.gathering_bytes = state.gathering_bytes.saturating_sub(gone.total);
            }
        }
    }

    /// Write the content that arrives to the store, all that arrived since
    /// the last write at once: each write waits for the disk to hold what it
    /// wrote, and one wait for every blob would bound a workspace's files to
    /// the blobs a disk flushes in a second.
    async fn keep_arrivals(self: Arc<Self>) {
        loop {
            self.arrived.notified().await;
            let arrivals = std::mem::take(&mut self.state.lock_recover().arrivals);
            let mut by_workspace: BTreeMap<String, Vec<Arrival>> = BTreeMap::new();
            for (workspace, arrived) in arrivals {
                by_workspace.entry(workspace).or_default().push(arrived);
            }
            for (workspace, arrived) in by_workspace {
                self.keep_arrived(&workspace, arrived).await;
            }
        }
    }

    /// Write `arrived` to `workspace`'s store in one write, each from a device
    /// that may write its record's app, then hold each for the member that
    /// pushed it and answer every device that pushed it.
    async fn keep_arrived(&self, workspace: &str, arrived: Vec<Arrival>) {
        let mut allowed = Vec::new();
        for arrival in arrived {
            if self
                .workspaces
                .may_push_content(workspace, arrival.from, &arrival.app)
                .await
            {
                allowed.push(arrival);
            } else {
                tracing::warn!(%workspace, app = %arrival.app, from = %hex::encode(arrival.from), "content pushed for an app its device may not write is refused");
                self.state
                    .lock_recover()
                    .storing
                    .remove(&(workspace.to_owned(), arrival.hash));
            }
        }
        let store = match self.store.get(workspace) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%workspace, %error, "a workspace's content could not be kept");
                let mut state = self.state.lock_recover();
                for arrival in &allowed {
                    state
                        .storing
                        .remove(&(workspace.to_owned(), arrival.hash.clone()));
                }
                return;
            }
        };
        let writing = store.clone();
        let Ok((arrived, outcomes)) = tokio::task::spawn_blocking(move || {
            let items: Vec<&[u8]> = allowed.iter().map(|blob| blob.bytes.as_slice()).collect();
            let outcomes = writing.put_public_many(&items);
            (allowed, outcomes)
        })
        .await
        else {
            return;
        };
        let mut withdrawn = Vec::new();
        for (blob, outcome) in arrived.into_iter().zip(outcomes) {
            let pushers = self
                .state
                .lock_recover()
                .storing
                .remove(&(workspace.to_owned(), blob.hash.clone()))
                .unwrap_or_default();
            if let Err(error) = outcome {
                tracing::warn!(hash = %blob.hash, %error, "pushed content could not be kept");
                continue;
            }
            {
                let mut state = self.state.lock_recover();
                // A refusal of this content no longer stands: its limit was
                // raised and the content is kept now.
                if let Some(held) = state.refused.get_mut(workspace)
                    && let Some(refusal) = held.remove(&blob.hash)
                    && refusal.withdrawn
                {
                    withdrawn.push(refusal);
                }
            }
            self.hold(workspace, &blob.hash, &blob.app, blob.from);
            if let Some(chunks) = chunks_of(&blob.bytes) {
                self.held_by_manifest(workspace, &blob.hash, &blob.app, chunks);
            }
            self.answer(workspace, blob.from, &blob.app, &blob.hash);
            let mut pushed_by = vec![blob.from];
            for (from, app) in pushers {
                self.hold(workspace, &blob.hash, &app, from);
                self.answer(workspace, from, &app, &blob.hash);
                pushed_by.push(from);
            }
            // The members that asked this host while it kept none of it.
            self.workspaces.content_kept(workspace, &blob.hash);
            // The workspace's other holders keep it too, whichever of them a
            // member reaches.
            let (workspaces, held, app, hash) = (
                self.workspaces.clone(),
                workspace.to_owned(),
                blob.app.clone(),
                blob.hash.clone(),
            );
            tokio::spawn(async move {
                workspaces
                    .hand_on_content(&held, &app, &hash, &pushed_by)
                    .await;
            });
        }
        if !withdrawn.is_empty() {
            self.withdraw_refusals(workspace, withdrawn);
        }
    }

    /// Hold `hash` of `app` in `workspace` for the member of the device
    /// `from`: unless it is a chunk a kept file's manifest holds, which goes
    /// with that file.
    fn hold(&self, workspace: &str, hash: &str, app: &str, from: [u8; 32]) {
        let added = {
            let mut state = self.state.lock_recover();
            let member = hex::encode(Self::member_of(&state, workspace, &from));
            let holding = state
                .holds
                .entry(workspace.to_owned())
                .or_default()
                .entry(hash.to_owned())
                .or_insert_with(|| Holding {
                    app: app.to_owned(),
                    holders: BTreeSet::new(),
                });
            let by_file = holding
                .holders
                .iter()
                .any(|holder| holder.starts_with(MANIFEST_HOLDER));
            !by_file && holding.holders.insert(member)
        };
        if added {
            self.held_changed(workspace, hash);
        }
    }

    /// `manifest`, a file of `app` kept in `workspace`, lists `chunks`: each
    /// is held by the file from now on, not by whoever pushed it, and goes
    /// when the file does.
    fn held_by_manifest(&self, workspace: &str, manifest: &str, app: &str, chunks: Vec<String>) {
        let holder = format!("{MANIFEST_HOLDER}{manifest}");
        {
            let mut state = self.state.lock_recover();
            let holds = state.holds.entry(workspace.to_owned()).or_default();
            for chunk in &chunks {
                let holding = holds.entry(chunk.clone()).or_insert_with(|| Holding {
                    app: app.to_owned(),
                    holders: BTreeSet::new(),
                });
                holding
                    .holders
                    .retain(|held| held.starts_with(MANIFEST_HOLDER));
                holding.holders.insert(holder.clone());
            }
        }
        for chunk in chunks {
            self.held_changed(workspace, &chunk);
        }
    }

    /// The member of the device `from` let go of its hold on `hash` in
    /// `workspace`: the content goes when no one holds it any more.
    fn release(&self, workspace: &str, from: [u8; 32], hash: &str) {
        let hash = hash.to_ascii_lowercase();
        let released = {
            let mut state = self.state.lock_recover();
            let member = hex::encode(Self::member_of(&state, workspace, &from));
            state
                .holds
                .get_mut(workspace)
                .and_then(|holds| holds.get_mut(&hash))
                .is_some_and(|holding| holding.holders.remove(&member))
        };
        if released {
            self.held_changed(workspace, &hash);
        }
    }

    /// Who holds `hash` in `workspace` changed: noted in the ledger, and the
    /// content let go when no one holds it, with the chunks only its file
    /// held.
    fn held_changed(&self, workspace: &str, hash: &str) {
        let holding = self
            .state
            .lock_recover()
            .holds
            .get(workspace)
            .and_then(|holds| holds.get(hash))
            .cloned()
            .unwrap_or_default();
        let (member, bytes, at) = self
            .state
            .lock_recover()
            .kept
            .get(&(workspace.to_owned(), hash.to_owned()))
            .map_or((String::new(), 0, now_ms()), |(member, bytes)| {
                (hex::encode(member), *bytes, now_ms())
            });
        self.ledger.note(
            workspace,
            &ledger::Entry {
                hash: hash.to_owned(),
                member,
                bytes,
                at,
                app: holding.app.clone(),
                held: holding.holders.iter().cloned().collect(),
            },
        );
        if !holding.holders.is_empty() {
            return;
        }
        self.collect(workspace, hash);
    }

    /// Let go of `hash` in `workspace`, which no one holds: from the store,
    /// from what the workspace's totals count, and, for a file's manifest,
    /// the hold it had on its chunks.
    fn collect(&self, workspace: &str, hash: &str) {
        let Ok(store) = self.store.get(workspace) else {
            return;
        };
        let chunks = store.read(hash).as_deref().and_then(chunks_of);
        if let Err(error) = store.release(hash) {
            tracing::warn!(%workspace, %hash, %error, "content no one holds was not let go");
            return;
        }
        {
            let mut state = self.state.lock_recover();
            let app = state
                .holds
                .get_mut(workspace)
                .and_then(|holds| holds.remove(hash))
                .map(|holding| holding.app);
            if let Some(app) = app {
                self.workspaces.content_let_go(workspace, &app, hash);
            }
            if let Some((member, bytes)) =
                state.kept.remove(&(workspace.to_owned(), hash.to_owned()))
            {
                if let Some(taken) = state
                    .taken
                    .get_mut(workspace)
                    .and_then(|taken| taken.get_mut(&member))
                {
                    taken.kept = taken.kept.saturating_sub(bytes);
                }
                if let Some(kept) = state.kept_bytes.get_mut(workspace) {
                    *kept = kept.saturating_sub(bytes);
                }
            }
        }
        let holder = format!("{MANIFEST_HOLDER}{hash}");
        for chunk in chunks.unwrap_or_default() {
            let released = self
                .state
                .lock_recover()
                .holds
                .get_mut(workspace)
                .and_then(|holds| holds.get_mut(&chunk))
                .is_some_and(|holding| holding.holders.remove(&holder));
            if released {
                self.held_changed(workspace, &chunk);
            }
        }
    }

    /// Say to the device `from` that this host keeps `hash` of `app` of
    /// `workspace`, which it pushed: its push goes no more.
    fn answer(&self, workspace: &str, from: [u8; 32], app: &str, hash: &str) {
        let Some(workspace_id) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
            return;
        };
        let kept = NeighbourMsg::DocumentContentKept {
            responder: peer_handle(&self.device),
            requester: peer_handle(&from),
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            hash: hash.to_owned(),
        };
        let workspaces = self.workspaces.clone();
        tokio::spawn(async move {
            if let Err(error) = workspaces.tell_members(workspace_id, kept).await {
                tracing::debug!(%error, "the host's word that it keeps pushed content did not go out");
            }
        });
    }
}

impl ContentKeeper for KeptContent {
    fn take(
        &self,
        workspace: &str,
        from: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), Box<NeighbourMsg>> {
        match message {
            NeighbourMsg::DocumentContentPieces {
                responder,
                requester,
                cluster_id,
                pieces,
            } if requester == peer_handle(&self.device)
                && responder == peer_handle(&from)
                && cluster_id == workspace =>
            {
                self.take_pushed(workspace, from, pieces);
                Ok(())
            }
            NeighbourMsg::DocumentContentReleased {
                requester,
                cluster_id,
                hash,
                ..
            } if requester == peer_handle(&from) && cluster_id == workspace => {
                self.release(workspace, from, &hash);
                Ok(())
            }
            other => Err(Box::new(other)),
        }
    }

    fn refused<'a>(
        &'a self,
        workspace: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<ContentRefusal>> + Send + 'a>> {
        Box::pin(async move { self.refusals(workspace).await })
    }

    fn held(&self, workspace: &str) -> ContentHeld {
        let state = self.state.lock_recover();
        ContentHeld {
            held: state.holds.get(workspace).map_or(0, |holds| {
                holds
                    .values()
                    .filter(|holding| !holding.holders.is_empty())
                    .count()
            }),
            going: state
                .gathering
                .keys()
                .filter(|(held, _)| held == workspace)
                .count()
                .saturating_add(
                    state
                        .storing
                        .keys()
                        .filter(|(held, _)| held == workspace)
                        .count(),
                ),
        }
    }

    fn holdings(&self, workspace: &str) -> Vec<(String, String)> {
        self.state
            .lock_recover()
            .holds
            .get(workspace)
            .into_iter()
            .flatten()
            .map(|(hash, holding)| (holding.app.clone(), hash.clone()))
            .collect()
    }
}

/// The chunks `bytes` lists, when it is a file's manifest.
fn chunks_of(bytes: &[u8]) -> Option<Vec<String>> {
    if !bytes
        .get(..MANIFEST_PREFIX_LEN)
        .is_some_and(begins_manifest)
    {
        return None;
    }
    let Ok(manifest) = Manifest::decode(bytes) else {
        tracing::debug!("a kept manifest does not read as one");
        return None;
    };
    Some(
        manifest
            .chunks()
            .iter()
            .map(|chunk| hex::encode(chunk.hash))
            .collect(),
    )
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
    use super::*;
    use tilo_content::ContentStore;

    /// A file kept as chunks names them in its manifest, which a workspace
    /// whose files are sealed writes in its sealed layout: the file holds the
    /// chunks of either, or they would stay when the file goes.
    #[test]
    fn a_kept_manifest_lists_its_chunks_sealed_or_not() {
        use fs_content::{ChunkRef, ChunkTier, SealedChunks};
        let chunks = vec![
            ChunkRef {
                hash: [0x3c_u8; 32],
                len: 30,
            },
            ChunkRef {
                hash: [0x7e_u8; 32],
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
            Manifest::new_sealed(ChunkTier::Small, chunks.clone(), sealed),
        ] {
            assert_eq!(
                chunks_of(&manifest.encode()),
                Some(vec!["3c".repeat(32), "7e".repeat(32)]),
                "sealed: {}",
                manifest.sealed().is_some()
            );
        }
        let store = ContentStore::memory();
        let other = store.put(b"a picture, no file's manifest").unwrap();
        assert_eq!(chunks_of(&store.read(&other).unwrap()), None);
    }

    /// A person's own folder is the workspace's storage: with no policy and no
    /// host budget, a 3 GiB file is over no limit, nor is one member's share.
    #[test]
    fn with_no_policy_a_three_gibibyte_file_is_over_no_limit() {
        use tilo_action_policy::content::MIB;
        let mut state = State::default();
        let limits = KeptLimits::default();
        assert_eq!(
            KeptContent::over(&mut state, &limits, "w", &[1_u8; 32], 3 * 1024 * MIB, 0),
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
