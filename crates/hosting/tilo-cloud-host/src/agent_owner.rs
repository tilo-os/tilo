//! Agent caller authority under the existing Cloud Store lock and durability barrier.
//! No runtime endpoint, launcher, public projection or execution permit is installed here.
use crate::Store;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tilo_action_broker::peer_actions::OwnerPolicyClock;
use tilo_room::owner_commit::*;

mod policy_time;
pub(crate) use policy_time::RuntimePolicyClock;

#[derive(Clone)]
pub struct CloudAgentAuthority {
    policy: Arc<RuntimePolicyClock>,
    root: PathBuf,
    machine: String,
    /// Last health proof and membership projection, each with the exact
    /// Store files it was read from (see [`StoreReads`]).
    reads: Arc<StoreReads>,
}
/// Reuse of two read-only Store observations while the Store files are the
/// identical files (`store_hint::StoreStamp`: device, inode, length, mtime,
/// ctime of `state.json`/`state.journal`) and at most [`STORE_READ_REUSE`]
/// old. `health` was probed every owner tick (100/s) and `snapshot` (a pure
/// projection of the committed room, no time dependence; callers evaluate
/// roles `_at` their own instant) by every broker membership check: each
/// was a full Store open, together ~35% of a core with no agent running.
/// Neither is an authorization verdict.
#[derive(Default)]
struct StoreReads {
    health: std::sync::Mutex<
        Option<(
            crate::agent_runtime_adapter::store_hint::StoreStamp,
            std::time::Instant,
        )>,
    >,
    snapshot: std::sync::Mutex<
        Option<(
            crate::agent_runtime_adapter::store_hint::StoreStamp,
            std::time::Instant,
            tilo_desktop_model::DesktopTree,
        )>,
    >,
}
const STORE_READ_REUSE: std::time::Duration = std::time::Duration::from_secs(2);
impl CloudAgentAuthority {
    pub fn open(root: &Path, machine: &str) -> Result<Self, String> {
        Self::open_with_clock(
            root,
            machine,
            OwnerPolicyClock::new().map_err(|e| e.to_string())?,
        )
    }
    pub(crate) fn open_with_clock(
        root: &Path,
        machine: &str,
        clock: OwnerPolicyClock,
    ) -> Result<Self, String> {
        Self::open_with_policy(root, machine, Arc::new(RuntimePolicyClock::new(clock)))
    }
    pub(crate) fn open_with_policy(
        root: &Path,
        machine: &str,
        policy: Arc<RuntimePolicyClock>,
    ) -> Result<Self, String> {
        // Selected startup must never provision or migrate an ambient replacement
        // before the retained actual Source has checked this very locked Store.
        let store = policy.open_store(root, machine, &policy.capture()?)?;
        canonical_owner(&store.peer().map_err(|e| e.to_string())?)?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        drop(store);
        Ok(Self {
            policy,
            root,
            machine: machine.into(),
            reads: Arc::default(),
        })
    }
    pub(crate) fn policy_context(&self) -> Arc<RuntimePolicyClock> {
        self.policy.clone()
    }
    #[track_caller]
    fn store(&self) -> Result<crate::StoreGuard, String> {
        let site = std::panic::Location::caller();
        crate::agent_runtime_adapter::count_open(site);
        let time = self.policy.capture()?;
        let mut store = self.policy.open_store(&self.root, &self.machine, &time)?;
        store.mark_site(site);
        Ok(store)
    }
    /// `device` must come from completed authenticated ingress, never request JSON.
    pub fn caller(&self, device: [u8; 32]) -> Result<VerifiedAgentCaller, String> {
        self.store()?.agent_caller(device)
    }
}
impl Store {
    pub(super) fn agent_scope(&self, peer: &tilo_room::StatePeer) -> Result<[u8; 32], String> {
        self.ensure_reconciled().map_err(|e| e.to_string())?;
        Ok(owner_scope(
            &self.root.canonicalize().map_err(|e| e.to_string())?,
            &self.state.machine_id,
            peer,
        ))
    }
    pub fn agent_caller(&self, device: [u8; 32]) -> Result<VerifiedAgentCaller, String> {
        let peers = self.peer_shared().map_err(|e| e.to_string())?;
        let peer = &peers.1;
        let scope = self.agent_scope(peer)?;
        let now_ns = self.policy_now_ns().map_err(|e| e.to_string())?;
        crate::peer_memo::caller(
            &peers,
            scope,
            self.state.generation,
            &self.state.room,
            device,
            String::new(),
            now_ns,
            || {
                VerifiedAgentCaller::from_authenticated_source(
                    peer,
                    scope,
                    self.state.generation,
                    &self.state.room,
                    device,
                    crate::now_time(now_ns),
                )
            },
        )
    }
    pub fn refresh_agent_caller(
        &self,
        caller: &VerifiedAgentCaller,
    ) -> Result<VerifiedAgentCaller, String> {
        let peers = self.peer_shared().map_err(|e| e.to_string())?;
        let peer = &peers.1;
        let scope = self.agent_scope(peer)?;
        let now_ns = self.policy_now_ns().map_err(|e| e.to_string())?;
        crate::peer_memo::caller(
            &peers,
            scope,
            self.state.generation,
            caller.room(),
            caller.device(),
            // The exact prior proof `refresh` compares with (memory only).
            format!("{caller:?}"),
            now_ns,
            || caller.refresh(peer, scope, self.state.generation, crate::now_time(now_ns)),
        )
    }
}
impl OwnerCommitService for CloudAgentAuthority {
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        // The worker broker's durable Effect receipts (e.g. the desktop's
        // open-application action) are the only generic commits served here.
        if matches!(
            request.operation,
            tilo_room::owner_commit::CommitOperation::EffectReceipt { .. }
        ) {
            let this = self.clone();
            return Box::pin(async move { this.store()?.commit_effect_receipt(&request) });
        }
        Box::pin(async { Err("generic owner commit unavailable on agent adapter".into()) })
    }
    fn health(&self) -> Result<(), String> {
        use crate::agent_runtime_adapter::store_hint::StoreStamp;
        // The policy clock/source must still be current on every probe.
        self.policy.capture()?;
        let stamp = StoreStamp::of(&self.root);
        if let (Some(stamp), Ok(retained)) = (stamp, self.reads.health.lock())
            && retained.is_some_and(|(seen, at)| seen == stamp && at.elapsed() < STORE_READ_REUSE)
        {
            return Ok(());
        }
        let store = self.store()?;
        store.peer_shared().map_err(|e| e.to_string())?;
        let proved = StoreStamp::of(&self.root);
        drop(store);
        if let (Some(proved), Ok(mut retained)) = (proved, self.reads.health.lock()) {
            *retained = Some((proved, std::time::Instant::now()));
        }
        Ok(())
    }
}

/// The public room projection of one committed peer: workspace, owner and
/// governed capabilities. Never projects private admissions or tasks.
pub(crate) fn workspace_tree(
    peer: &tilo_room::StatePeer,
) -> Result<tilo_desktop_model::DesktopTree, String> {
    let owner = canonical_owner(peer)?;
    let doc = peer.admitted_room().ok_or("admitted room missing")?;
    let mut tree = tilo_desktop_model::DesktopTree::default();
    tilo_desktop_model::projection::apply_admitted_room(
        &mut tree,
        tilo_desktop_model::RoomId::parse(peer.room()).ok_or("invalid room")?,
        doc,
    )?;
    let workspace = tree.workspaces.first_mut().ok_or("workspace missing")?;
    workspace.owner = Some(owner);
    workspace.capabilities = tilo_room::room::extract_governed_fields(doc).2;
    Ok(tree)
}
impl tilo_action_broker::services::WorkspaceAuthority for CloudAgentAuthority {
    fn snapshot(&self) -> Result<tilo_desktop_model::DesktopTree, String> {
        use crate::agent_runtime_adapter::store_hint::StoreStamp;
        self.policy.capture()?;
        if let (Some(stamp), Ok(retained)) =
            (StoreStamp::of(&self.root), self.reads.snapshot.lock())
            && let Some((seen, at, tree)) = retained.as_ref()
            && *seen == stamp
            && at.elapsed() < STORE_READ_REUSE
        {
            return Ok(tree.clone());
        }
        let store = self.store()?;
        let peers = store.peer_shared().map_err(|e| e.to_string())?;
        let tree = workspace_tree(&peers.1)?;
        // Stamped while the Store lock is still held: exactly these files.
        if let (Some(stamp), Ok(mut retained)) =
            (StoreStamp::of(&self.root), self.reads.snapshot.lock())
        {
            *retained = Some((stamp, std::time::Instant::now(), tree.clone()));
        }
        drop(store);
        Ok(tree)
    }
}
