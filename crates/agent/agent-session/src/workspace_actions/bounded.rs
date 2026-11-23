//! Callers this service acts for within bounds: a person the workspace admits
//! who is not the one its device belongs to, and an agent acting for any person,
//! that one included (`tilo mcp` on a computer `tilo login` made a device of theirs).
//!
//! Such a caller's record actions run in a confined replica of their own,
//! opened as them ([`crate::ReplicaActor::Member`]): the person's access as
//! the workspace's policy gives it now and the sandbox its governance names,
//! and for an agent that access bounded as every agent's is. What that
//! replica surveys is the one answer to both what the caller's listing shows
//! and what the host runs for them: an action it does not offer is refused,
//! as not permitted when the document publishes it to someone else.
//!
//! Nobody acting for a person confirms by their own word, and the host does
//! not take a person's word from a device that may be their agent: where an
//! action asks first, it is refused showing the person what would be done
//! ([`About`]), the dispatch keeps it as an asked invocation record, and the
//! person's approve of that record admits it again
//! ([`InvokeRequest::approval`]). A confirmation of the caller's own word is
//! refused.
//!
//! Before what such an action changed is saved, the host reads the caller's
//! permissions again and checks every field the change writes against them
//! ([`refused_writes`]): a commenter's change outside comments and reactions,
//! or anything a viewer's action wrote, is never saved.

use super::{
    ActionCallError, Arc, Caller, ConfinedReplica, DocumentMutation, DocumentRoute, HashMap,
    InvocationId, InvokeRequest, Mounted, Outcome, Seed, Worker, WorkspaceId, WorkspaceState,
    reads,
};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::ActorPlace;
use tilo_server_protocol::live::LiveAnchor;
use ui::permissions::policy::{self, ActionKind, Answer, TreePath};

/// How many times a write refused for a part nobody holds is asked again: a chain's
/// head, then its first part.
const UNHELD_ROUNDS: usize = 3;

/// Put into the caller's replica the content its document refers to, before an effect reads
/// it (a message's parts, an export's pictures). A change of a document reads none of it, also
/// one its app carries out on a part of a chain: finding that content reads every part, which
/// made each message an import delivered cost more the more the mailbox held.
fn put_what_it_reads(held: &BoundedReplica, descriptor: &tilo_actions::ActionDescriptor) {
    if descriptor.changes_only_a_document() {
        return;
    }
    if let Err(error) = held.held.app.put_held_content() {
        tracing::warn!(%error, "workspace actions: the content it refers to was not put");
    }
}

/// How long a caller's replica is kept on the member access it was opened with
/// before the host reads their access again for it.
const ACCESS_FRESHNESS_SECS: u64 = 30;

/// Who a bounded caller is: the person whose access bounds them, as that
/// person's device is admitted, and the agent acting for them, if one is.
#[derive(Clone)]
pub(super) struct Bounded {
    /// The caller as the transport admitted it: what decides whom the host
    /// acts for ([`Caller::acting`]).
    pub(super) caller: Caller,
    /// The person, on the device the request came from: what the workspace's
    /// policy is asked about.
    pub(super) person_caller: Caller,
    /// The person's policy actor id.
    pub(super) person: ui::permissions::policy::PersonId,
    /// The agent acting for the person, by its agent id.
    pub(super) agent: Option<String>,
    /// The name that agent goes by ([`Caller::agent_name`]).
    pub(super) agent_name: Option<String>,
    /// The web page the person acts through, when they act through one
    /// ([`super::pages`]): each page holds a replica of its own, which shows
    /// what that page shows.
    pub(super) page: Option<u64>,
}

/// The replica a bounded caller acts through, by the document it holds and who.
pub(super) type BoundedKey = (
    DocumentRoute,
    ui::permissions::policy::PersonId,
    Option<String>,
    Option<u64>,
);

/// A caller's replica of one document, and the permissions it was opened with.
pub(super) struct BoundedReplica {
    pub(super) held: Mounted,
    access: policy::MemberAccess,
    /// The grants its caller carried. An agent's turn and what is written for that agent outside
    /// a turn are one person and one agent under different rules: each acts on a replica opened
    /// under its own, never on the other's.
    granted: Vec<policy::Grant>,
    sandbox: Vec<collab_types::caps::CapOverride>,
    unix_time: u64,
    device: Option<tilo_actions::InvokerDevice>,
}

/// Callers' replicas whose last write is still on its way to be saved, by
/// the caller and document they act through, with what hears whether it was
/// ([`SaveWatch`](super::publish::SaveWatch)).
///
/// Such a replica is kept here rather than let go: the next action of the
/// same caller reuses it once the host holds the write, instead of opening
/// the document again over every change it holds. Nothing unsaved is ever
/// acted on: a write the host refused or never confirmed lets the replica
/// go, to be opened again from what the workspace holds.
pub(super) struct Parked<T> {
    waiting: Vec<(BoundedKey, T, Arc<super::publish::SaveWatch>)>,
}

impl<T> Default for Parked<T> {
    fn default() -> Self {
        Self {
            waiting: Vec::new(),
        }
    }
}

impl<T> Parked<T> {
    /// Set `held` aside for `key` while what it wrote is saved, which
    /// `watch` hears.
    pub(super) fn park(&mut self, key: BoundedKey, held: T, watch: Arc<super::publish::SaveWatch>) {
        // A write the host refused or never confirmed leaves nothing to reuse:
        // its replica is let go rather than kept.
        self.waiting
            .retain(|(parked, _, watch)| *parked != key && watch.outcome() != Some(false));
        while self.waiting.len() >= super::MAX_MOUNTED {
            self.waiting.remove(0);
        }
        self.waiting.push((key, held, watch));
    }

    /// Take back what was set aside for `key` once the host holds what it
    /// wrote, waiting at most `bound` for it: nothing while the save is not
    /// confirmed.
    pub(super) fn unpark(&mut self, key: &BoundedKey, bound: std::time::Duration) -> Option<T> {
        let at = self
            .waiting
            .iter()
            .position(|(parked, _, _)| parked == key)?;
        let (_, held, watch) = self.waiting.remove(at);
        watch.wait(bound).then_some(held)
    }

    /// Keep what `keep` holds of the keys, letting the rest go: a workspace
    /// deleted takes its callers' replicas with it.
    pub(super) fn retain(&mut self, keep: impl Fn(&BoundedKey) -> bool) {
        self.waiting.retain(|(key, _, _)| keep(key));
    }
}

/// The refusal of a confirmation a bounded caller gave by its own word.
pub(super) fn own_word_refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: "only the person confirms this action, on their own device: invoke it without \
                 a confirmation and the host asks them"
            .into(),
    }
}

/// Where below a document a write at storage keys (top down) lands, as far as
/// access tells writes apart: inside its comments or reactions, wherever a
/// model keeps them, or anywhere else.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lands {
    Comments,
    Reactions,
    Elsewhere,
}

impl Lands {
    fn of(keys: &[String]) -> Self {
        let inside = |field: &str| keys.iter().any(|key| key == field);
        if inside(policy::roles::COMMENTS_FIELD) {
            Self::Comments
        } else if inside(policy::roles::REACTIONS_FIELD) {
            Self::Reactions
        } else {
            Self::Elsewhere
        }
    }

    /// The node below the workspace a write of a document of `app` that lands
    /// here acts at: its comments or reactions; else the field the action that
    /// wrote it is declared below (`#[action(field = ...)]`: filing a report is
    /// a comment, whose app keeps it where its own footprint says), else the
    /// document.
    fn node(self, app: &str, declared: Option<&str>) -> TreePath {
        let document = TreePath::root().app(app);
        match (self, declared) {
            (Self::Comments, _) => document.field(policy::roles::COMMENTS_FIELD),
            (Self::Reactions, _) => document.field(policy::roles::REACTIONS_FIELD),
            (Self::Elsewhere, Some(field)) => document.field(field),
            (Self::Elsewhere, None) => document,
        }
    }
}

fn key_of(prop: &automerge::Prop) -> String {
    match prop {
        automerge::Prop::Map(key) => key.clone(),
        automerge::Prop::Seq(index) => index.to_string(),
    }
}

/// The keys a change writes, as its document's replica reports them.
pub(super) type WrittenKeys = Result<Vec<Vec<String>>, String>;

/// The storage keys `mutation` writes at, each top down to what it put or
/// deleted, in a record whose changes so far are `before`.
fn written_keys(
    before: Option<&[u8]>,
    mutation: &DocumentMutation,
) -> Result<Vec<Vec<String>>, String> {
    use automerge::PatchAction;
    use tilo_automerge::ApplyIncoming as _;
    let mut doc = automerge::AutoCommit::new();
    if let Some(bytes) = before {
        doc.load_incoming(bytes)
            .map_err(|error| error.to_string())?;
    }
    let was = doc.get_heads();
    let (DocumentMutation::Snapshot(written) | DocumentMutation::Incremental(written)) = mutation;
    doc.load_incoming(written)
        .map_err(|error| error.to_string())?;
    let now = doc.get_heads();
    Ok(doc
        .diff(&was, &now)
        .into_iter()
        .map(|patch| {
            let mut keys: Vec<String> = patch.path.iter().map(|(_, prop)| key_of(prop)).collect();
            match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    keys.push(key.clone());
                }
                PatchAction::Increment { prop, .. } | PatchAction::Conflict { prop } => {
                    keys.push(key_of(prop));
                }
                _ => {}
            }
            keys
        })
        .collect())
}

/// What of `mutation`, a change to a record of `app` whose changes so far are
/// `before` made by an action declared below the field `declared`, if any,
/// `permissions` do not let its author write: the nodes refused, empty when
/// every write is theirs. A write the grants leave to the person counts only
/// when `confirmed` by them.
pub(super) fn refused_writes(
    permissions: &ui::permissions::PermissionContext,
    (app, declared): (&str, Option<&str>),
    before: Option<&[u8]>,
    mutation: &DocumentMutation,
    (written, confirmed): (Option<WrittenKeys>, bool),
) -> Result<Vec<TreePath>, String> {
    let mut refused = Vec::new();
    let written = written.unwrap_or_else(|| written_keys(before, mutation));
    // Each place a write lands is answered once, however many keys land there.
    let mut lands: Vec<Lands> = Vec::new();
    for keys in written? {
        let at = Lands::of(&keys);
        if !lands.contains(&at) {
            lands.push(at);
        }
    }
    for at in lands {
        let node = at.node(app, declared);
        match permissions.answer(&node, ActionKind::Undoable) {
            Answer::Allow => {}
            Answer::Ask if confirmed => {}
            Answer::Ask | Answer::Deny => refused.push(node),
        }
    }
    Ok(refused)
}

/// What the person is asked about an invocation that waits for their answer:
/// the action as its app declares it, the arguments it would run with, and
/// the record it acts on by its name.
pub(super) struct About {
    pub(super) descriptor: tilo_actions::ActionDescriptor,
    pub(super) arguments: serde_json::Map<String, serde_json::Value>,
    /// The record's name in its app's index of the workspace, else what the
    /// app calls the resource the action is on.
    pub(super) record: String,
    /// The document the action acts on.
    pub(super) route: DocumentRoute,
    /// What the call carries, where it carries its resource's other actions.
    pub(super) carries: super::carried::Carries,
}

/// `who`'s access as the host decides for them: the person's access as
/// `policy` gives it now, held to the bounds the one reading of a caller
/// gives ([`Caller::acting`]): for an agent, the grants the caller carries
/// ([`Caller::granted`]) as the broker gives them, narrowed by the bound the
/// workspace's list of its agents keeps for it. Refused when the policy is
/// someone else's.
pub(super) fn bounded_access(
    policy: &super::ViewReadPolicy,
    who: &Bounded,
) -> Result<policy::MemberAccess, ActionCallError> {
    let mut acting = who.caller.acting().clone();
    if who.caller.roster_created_at().is_some() {
        // Keep the authenticated turn's session while preserving the device cap.
        acting.foreign_device = policy.access.acting.foreign_device;
    }
    let granted = agent_grants(policy, who);
    policy
        .access
        .clone()
        .as_acting(acting)
        .map(|access| access.granted(granted))
        .ok_or(ActionCallError::NotPermitted)
}

fn agent_grants(policy: &super::ViewReadPolicy, who: &Bounded) -> Vec<policy::Grant> {
    if let Some(agent) = who
        .agent
        .as_ref()
        .filter(|_| who.caller.roster_created_at().is_some())
    {
        policy::roles::agent_bound(agent, who.person.as_str(), &policy.agent_bound)
    } else {
        who.caller.granted(&policy.agent_bound)
    }
}

/// `who`'s permissions under `policy` ([`bounded_access`]).
pub(super) fn bounded_permissions(
    policy: &super::ViewReadPolicy,
    who: &Bounded,
) -> Result<ui::permissions::PermissionContext, ActionCallError> {
    Ok(super::ViewReadPolicy {
        access: bounded_access(policy, who)?,
        ..policy.clone()
    }
    .permissions())
}

/// The replica actor the host runs `who`'s actions as, on their access
/// under `policy` ([`bounded_access`]), with `own_device` wherever `who` names
/// this device: the work is admitted on it then, as a member's is on the server
/// they delegated it to.
pub(super) fn member_actor(
    policy: &super::ViewReadPolicy,
    who: &Bounded,
    own_device: &super::HostDevice,
    certified: Option<tilo_actions::InvokerDevice>,
) -> Result<crate::ReplicaActor, ActionCallError> {
    let own = own_device.key.verifying_key().to_bytes();
    let device = certified.or_else(|| {
        (who.caller.device_public_key() == Some(&own)).then(|| {
            tilo_actions::InvokerDevice::certified(
                own,
                &own_device.endpoint.server_cert.device_label,
            )
        })
    });
    Ok(crate::ReplicaActor::Member {
        access: bounded_access(policy, who)?,
        sandbox: policy.sandbox.clone(),
        unix_time: policy.unix_time,
        agent_name: who.agent_name.clone(),
        device,
    })
}

/// `caller` as the host acts for them within bounds: a person who is not
/// the host's own, an agent a person's own device handed its session to,
/// or an agent device a person certified. `None` for the host's own person, and for anyone the host acts for
/// in no person's name.
///
/// `member` is the person whose device the service is.
pub(super) fn bounded(
    member: &ui::permissions::policy::PersonId,
    caller: &Caller,
) -> Option<Bounded> {
    if let Some(person) = caller.acting().person_id() {
        if person == member.as_str() {
            return None;
        }
        return Some(Bounded {
            caller: caller.clone(),
            person_caller: caller.clone(),
            person: person.into(),
            agent: None,
            agent_name: None,
            page: None,
        });
    }
    let agent = caller.acting().agent_id()?;
    // An agent the person's device handed its session to acts on
    // that device's admission; an agent device the person
    // certified, on its own device's: either is bounded by the
    // person it acts for.
    let person_caller = caller.delegating_person().or_else(|| {
        let person =
            Caller::authenticated(*caller.user_public_key()?, *caller.device_public_key()?);
        Some(match caller.workspace() {
            Some(workspace) => person.in_workspace(workspace),
            None => person,
        })
    })?;
    // An agent device on a turn answers the person whose message caused it, whoever the agent
    // belongs to: that person is who it is decided for and what is kept from.
    let person = if caller.roster_created_at().is_some() {
        caller.acting().acted_for_person()?.into()
    } else {
        person_caller.acting().person_id()?.into()
    };
    Some(Bounded {
        caller: caller.clone(),
        person,
        agent: Some(agent.to_owned()),
        agent_name: caller.agent_name().map(str::to_owned),
        person_caller,
        page: None,
    })
}

/// The person's access in the workspace `cluster` as its policy gives it now,
/// where they may read `record` (an app and its record) when one is named:
/// refused for a person it does not admit (a removed member, a revoked
/// device). The host's own person (`member`), in a workspace no governance
/// here answers for, is its owner, as the host's own mount holds it.
pub(super) fn policy_for(
    shared: &super::Shared,
    member: &ui::permissions::policy::PersonId,
    workspace_id: WorkspaceId,
    who: &Bounded,
    cluster: &str,
    inside: Option<(&str, &str)>,
) -> Result<super::ViewReadPolicy, ActionCallError> {
    if who.caller.roster_created_at().is_some() {
        return shared
            .link
            .get()
            .and_then(|link| link.roster_policy(workspace_id, &who.caller, inside))
            .filter(|policy| policy.workspace == cluster)
            .ok_or(ActionCallError::NotPermitted);
    }
    let decided = shared.link.get().and_then(|link| match inside {
        Some((app, named)) => link.view_read_policy(workspace_id, &who.person_caller, app, named),
        None => link.workspace_policy(workspace_id, &who.person_caller),
    });
    let mut policy = match decided {
        Some(policy)
            if policy.workspace == cluster
                && policy.access.person_id().unwrap_or_default() == who.person.as_str() =>
        {
            policy
        }
        None if who.person == *member && !governed(shared, workspace_id) => {
            let owner = ui::permissions::PermissionContext::member_of(
                cluster,
                &ui::permissions::MemberId::person(who.person.clone()),
                ui::permissions::Role::Owner,
            );
            super::ViewReadPolicy {
                workspace: cluster.to_owned(),
                access: owner.access().clone(),
                sandbox: Vec::new(),
                unix_time: tilo_clock::unix_now_ns() / 1_000_000_000,
                agent_bound: Vec::new(),
            }
        }
        Some(_) | None => return Err(not_admitted(shared, workspace_id, who)),
    };
    // An agent a person's device handed its session to is held to the bound the workspace's
    // list of its agents keeps for it: the own person's agents as much as anyone's, so what
    // the person sets for it in the workspace holds on their own server too.
    if who.agent.is_some()
        && who.caller.is_session_delegated()
        && let Some(link) = shared.link.get()
    {
        policy.agent_bound = link.workspace_agent(workspace_id, &who.caller);
    }
    Ok(policy)
}

/// The refusal of a caller the workspace's policy does not admit: a key the
/// workspace keeps as revoked is told so, with the one command that approves
/// another, so every caller knows what to do; anyone else is a plain refusal.
fn not_admitted(
    shared: &super::Shared,
    workspace_id: WorkspaceId,
    who: &Bounded,
) -> ActionCallError {
    let revoked = shared
        .link
        .get()
        .is_some_and(|link| link.key_revoked(workspace_id, &who.person_caller));
    if revoked {
        ActionCallError::Refused {
            reason: format!(
                "{}: this key was revoked. {}",
                tilo_actions::remote::KEY_REVOKED,
                tilo_actions::remote::APPROVE_AGAIN
            ),
        }
    } else {
        ActionCallError::NotPermitted
    }
}

/// Whether `caller` may do `kind` across the workspace `workspace_id`
/// (its content, a new record of it): the host's own person and their
/// agents may; anyone else as the workspace's policy gives them now, an
/// agent held to what an agent of its person may do.
pub(super) fn may_across(
    shared: &super::Shared,
    member: &ui::permissions::policy::PersonId,
    workspace_id: WorkspaceId,
    caller: &Caller,
    kind: ActionKind,
) -> bool {
    if caller.acting().person_id() == Some(member.as_str()) {
        return true;
    }
    let Some(who) = bounded(member, caller) else {
        return false;
    };
    policy_for(
        shared,
        member,
        workspace_id,
        &who,
        &workspace_id.to_string(),
        None,
    )
    .ok()
    .and_then(|policy| bounded_permissions(&policy, &who).ok())
    .is_some_and(|permissions| permissions.answer(&TreePath::root(), kind) == Answer::Allow)
}

/// Whether a governance on this device answers for who is in `workspace_id`.
pub(super) fn governed(shared: &super::Shared, workspace_id: WorkspaceId) -> bool {
    let Some(device) = shared.own_device.get() else {
        return false;
    };
    shared.link.get().is_some_and(|link| {
        link.admits(
            workspace_id,
            &Caller::authenticated(
                device.endpoint.server_cert.user_public_key,
                device.endpoint.server_cert.device_public_key,
            ),
        )
    })
}

impl Worker {
    /// [`bounded`], for the person whose device this service is.
    pub(super) fn bounded(&self, caller: &Caller) -> Option<Bounded> {
        bounded(&self.member, caller)
    }

    /// [`policy_for`], for the person whose device this service is.
    pub(super) fn policy_for(
        &self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        cluster: &str,
        inside: Option<(&str, &str)>,
    ) -> Result<super::ViewReadPolicy, ActionCallError> {
        policy_for(
            &self.shared,
            &self.member,
            workspace_id,
            who,
            cluster,
            inside,
        )
    }

    /// [`Self::policy_for`] the record at `route`.
    fn policy_at(
        &self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        route: &DocumentRoute,
    ) -> Result<super::ViewReadPolicy, ActionCallError> {
        let record = Some((route.app_id.as_str(), route.document.as_str()));
        self.policy_for(workspace_id, who, &route.cluster_id, record)
    }

    /// [`may_across`], for the person whose device this service is.
    pub(super) fn may_across(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        kind: ActionKind,
    ) -> bool {
        may_across(&self.shared, &self.member, workspace_id, caller, kind)
    }

    /// [`governed`], for the device this service is.
    fn governed(&self, workspace_id: WorkspaceId) -> bool {
        governed(&self.shared, workspace_id)
    }

    /// Whether `who` may see the workspace's records at all.
    pub(super) fn admits_bounded(&self, workspace_id: WorkspaceId, who: &Bounded) -> bool {
        if who.caller.roster_created_at().is_some() {
            return self.shared.link.get().is_some_and(|link| {
                link.roster_policy(workspace_id, &who.caller, None)
                    .is_some()
            });
        }
        (who.person == self.member && !self.governed(workspace_id))
            || self
                .shared
                .link
                .get()
                .is_some_and(|link| link.admits(workspace_id, &who.person_caller))
    }

    /// What `who` may do with the resource at `route`, as their listing's replica of it is
    /// opened: the person's access as the workspace's policy gives it now, held to what bounds
    /// an agent acting for them. `None` where the workspace does not let that person read it.
    pub(super) fn permissions_at(
        &self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        route: &DocumentRoute,
    ) -> Option<ui::permissions::PermissionContext> {
        let policy = self.policy_at(workspace_id, who, route).ok()?;
        bounded_permissions(&policy, who).ok()
    }

    /// What `who` may do across the workspace `cluster`, with no one resource named
    /// ([`Self::permissions_at`]): what its files are decided by. `None` for a person the
    /// workspace does not admit.
    pub(super) fn permissions_across(
        &self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        cluster: &str,
    ) -> Option<ui::permissions::PermissionContext> {
        let policy = self.policy_for(workspace_id, who, cluster, None).ok()?;
        bounded_permissions(&policy, who).ok()
    }

    /// Set aside the least recently used replicas within bounds until one more fits.
    fn evict_bounded(&mut self) {
        while self.bounded.len() >= super::MAX_MOUNTED {
            let Some(oldest) = self
                .bounded
                .iter()
                .min_by_key(|(_, held)| held.held.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.bounded.remove(&oldest);
        }
    }

    /// `who`'s replica of the record at `route` (a part: of the record it is
    /// held beside), opened as them on their access now, or the one held
    /// for them while that access is unchanged.
    #[allow(
        clippy::too_many_lines,
        reason = "one replica opened: its policy, its mount and what it takes up, in order"
    )]
    pub(super) fn bounded_replica(
        &mut self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<BoundedKey, ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let policy = self.policy_at(workspace_id, who, route)?;
        // Where the host's own mount holds it, so a part is held beside the
        // same record for everyone. Why it does not mount is the caller's to
        // read: a record this host cannot open is unavailable now, never a
        // record that does not exist.
        let holder = self.mount_held(route, state).map_err(unavailable)?;
        let key = (
            holder.clone(),
            who.person.clone(),
            who.agent.clone(),
            who.page,
        );
        // A replica set aside heard of nothing written while it was: it merges what the
        // workspace holds whole, as one opened anew does.
        let unparked = self.parked.unpark(&key, super::publish::REUSE_WAIT);
        let whole = unparked.is_some();
        if let Some(held) = unparked {
            self.bounded.insert(key.clone(), held);
        }
        // Its parts taken in off this thread: nothing else asks its worker meanwhile.
        if self.parts_taken_in(&key) {
            return Err(ActionCallError::NotReady);
        }
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        let granted = agent_grants(&policy, who);
        let device = if who.caller.roster_created_at().is_some() {
            let (peer, _) = self
                .shared
                .agent_sessions
                .get()
                .and_then(|sessions| sessions.live_roster(workspace_id, &who.caller))
                .ok_or(ActionCallError::NotPermitted)?;
            let cert = &peer.engine().identity().device_cert;
            Some(tilo_actions::InvokerDevice::certified(
                cert.device_public_key,
                &cert.device_label,
            ))
        } else {
            None
        };
        // Renew the same caller's replica in place as the workspace policy changes.
        let same = self
            .bounded
            .get(&key)
            .is_some_and(|held| held.granted == granted && held.device == device);
        let mut current = same
            && self.bounded.get(&key).is_some_and(|held| {
                held.access == policy.access
                    && held.sandbox == policy.sandbox
                    && policy.unix_time.saturating_sub(held.unix_time) < ACCESS_FRESHNESS_SECS
            });
        // Access read again later, or moved: the replica takes the new access in place rather
        // than being opened again over the whole record. A caller's access holds the
        // workspace's whole policy, so a person admitted anywhere in the workspace moved it, and
        // reopening then took every part of a tracker's chains in again before the caller's next
        // listing answered: half a minute at a few thousand issues.
        if same
            && !current
            && let Some(held) = self.bounded.get_mut(&key)
            && held
                .held
                .app
                .renew(&member_actor(
                    &policy,
                    who,
                    &self.own_device,
                    device.clone(),
                )?)
                .is_ok()
        {
            held.access.clone_from(&policy.access);
            held.sandbox.clone_from(&policy.sandbox);
            held.unix_time = policy.unix_time;
            current = true;
        }
        if !current {
            self.bounded.remove(&key);
            self.evict_bounded();
            let (bytes, heads) = state.document(&holder).map_err(unavailable)?;
            let member = ui::permissions::MemberId::Person(who.person.clone());
            let actor = member_actor(&policy, who, &self.own_device, device.clone())?;
            let seed = Seed::Existing {
                record: reads::document_id(&holder.document).map_err(unavailable)?,
                changes: vec![bytes],
            };
            let app = if who.page.is_some() {
                ConfinedReplica::open_page(
                    &self.catalogue,
                    (&holder.app_id, &holder.cluster_id),
                    &member,
                    &actor,
                    seed,
                )
            } else {
                ConfinedReplica::open(
                    &self.catalogue,
                    &holder.app_id,
                    &holder.cluster_id,
                    &member,
                    &actor,
                    seed,
                    true,
                )
            }
            .map_err(|error| unavailable(error.to_string()))?;
            let app = self.lend(app, &holder.cluster_id);
            self.bounded.insert(
                key.clone(),
                BoundedReplica {
                    held: Mounted {
                        app: std::sync::Arc::new(app),
                        merged: Some(heads),
                        parts: HashMap::new(),
                        foreign: std::collections::HashSet::new(),
                        wanted_at: None,
                        behind: None,
                        used: clock,
                    },
                    access: policy.access,
                    granted,
                    sandbox: policy.sandbox,
                    unix_time: policy.unix_time,
                    device,
                },
            );
        }
        let catching = super::timing_start();
        let untold =
            super::catch_up::kept_untold(&holder, self.bounded.get(&key).map(|held| &held.held));
        let kept = self.kept_held(untold);
        let wanted = self.kept_held(super::catch_up::wanted_untold(
            &holder,
            self.bounded.get(&key).map(|held| &held.held),
            state,
        ));
        let held = self
            .bounded
            .get_mut(&key)
            .ok_or_else(|| unavailable("the record did not mount".into()))?;
        held.held.used = clock;
        // What others wrote since it was last used is merged before it acts, only the
        // changes after the heads it holds; a replica opened anew or taken back from a save
        // that outlived the grace merges what the workspace holds of every part it has.
        let behind = held.held.behind.take();
        if whole || !current {
            super::catch_up::catch_up(&holder, &mut held.held, state, None);
        } else if let Some(behind) = behind {
            super::catch_up::catch_up(&holder, &mut held.held, state, Some(&behind));
        }
        super::catch_up::tell_kept(&mut held.held, state, kept);
        super::catch_up::tell_kept(&mut held.held, state, wanted);
        super::timing_done("replica.catch_up", catching);
        // A replica holding none of many parts takes them in off this thread.
        if self.load_parts_apart(&key, state) {
            return Err(ActionCallError::NotReady);
        }
        // The parts of its chains the workspace has still to send, waited on: off this
        // thread for a write or a listing, which is held until they arrive.
        self.hold_chains(&super::Writer::Bounded(key.clone()), &holder, state);
        if self.hold_for_chains() {
            return Err(ActionCallError::NotReady);
        }
        let held = self
            .bounded
            .get_mut(&key)
            .ok_or_else(|| unavailable("the document did not mount".into()))?;
        if holder != *route && !held.held.parts.contains_key(route) {
            let (part, heads) = state.document(route).map_err(unavailable)?;
            held.held
                .app
                .apply_document(
                    &reads::document_id(&route.document).map_err(unavailable)?,
                    &DocumentMutation::Snapshot(part),
                )
                .map_err(|error| unavailable(error.to_string()))?;
            held.held.parts.insert(route.clone(), Some(heads));
        }
        Ok(key)
    }

    /// What `who` is offered on the record at `route`: what their replica
    /// surveys.
    pub(super) fn bounded_actions(
        &mut self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, ActionCallError> {
        let key = self.bounded_replica(workspace_id, who, route, state)?;
        Ok(self
            .bounded
            .get(&key)
            .map(|held| held.held.app.survey())
            .unwrap_or_default())
    }

    /// One page of what `who` is offered on the record at `route`, as
    /// `query` lists it: [`Self::bounded_actions`] paged in their replica,
    /// each row where it stands.
    pub(super) fn bounded_page(
        &mut self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        (route, state): (&DocumentRoute, &WorkspaceState),
        query: &tilo_actions::remote::CatalogueQuery,
        window: (usize, usize),
    ) -> Result<(usize, Vec<tilo_actions::remote::CatalogueRow>), ActionCallError> {
        let key = self.bounded_replica(workspace_id, who, route, state)?;
        let holder = key.0.clone();
        match self.listed_from(
            (workspace_id, &super::Writer::Bounded(key), &holder),
            state,
            query,
            window,
        ) {
            super::part_load::Listed::Page(total, rows) => Ok((total, rows)),
            super::part_load::Listed::Held => Err(ActionCallError::NotReady),
            super::part_load::Listed::Answering => Err(ActionCallError::Unavailable {
                reason: "the app's worker is still answering".into(),
            }),
        }
    }

    /// The action at `address` as `who` is offered it on the record at
    /// `route`, resolved by its address in their replica: what one action
    /// costs, not a survey of everything the record publishes.
    pub(super) fn bounded_descriptor(
        &mut self,
        workspace_id: WorkspaceId,
        who: &Bounded,
        (route, state): (&DocumentRoute, &WorkspaceState),
        address: &ActionAddress,
    ) -> Result<Option<tilo_actions::ActionDescriptor>, ActionCallError> {
        let key = self.bounded_replica(workspace_id, who, route, state)?;
        Ok(self
            .bounded
            .get(&key)
            .and_then(|held| held.held.app.descriptor(address)))
    }

    /// `invocation` waits for the person's answer, asked as `about` says.
    pub(super) fn waits_for_person(&self, invocation: InvocationId, about: About) -> Outcome {
        if let Some(entry) = self.log().entries.get_mut(&invocation) {
            entry.about = Some(Box::new(about));
        }
        Outcome::Rejected(ActionCallError::confirmation_required())
    }

    /// What the person is asked about running `descriptor` with `arguments`
    /// on the record at `route`, which carries `carries` ([`About`]).
    pub(super) fn about(
        &self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        descriptor: &tilo_actions::ActionDescriptor,
        arguments: serde_json::Map<String, serde_json::Value>,
        carries: super::carried::Carries,
    ) -> About {
        About {
            record: self
                .document_name(workspace_id, route)
                .unwrap_or_else(|| descriptor.resource_label.clone()),
            descriptor: descriptor.clone(),
            arguments,
            route: route.clone(),
            carries,
        }
    }

    /// What a bounded caller's action changes has not arrived from the workspace (a chain's
    /// newest part, or the word that it has none): asked for, handed over, and the action
    /// asked again; a part still on its way holds the request off this thread until it
    /// arrives, as the owner's replica does ([`Worker::invoke_on`]), with the replica kept
    /// for when it is applied again.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    fn ask_again_once_arrived(
        &mut self,
        workspace_id: WorkspaceId,
        key: &BoundedKey,
        held: BoundedReplica,
        (tried, confirmed): (
            Result<tilo_actions::ActionOutput, tilo_actions::api::ApiError>,
            bool,
        ),
        asked: impl Fn(
            &mut Self,
            &BoundedReplica,
        ) -> Result<tilo_actions::ActionOutput, tilo_actions::api::ApiError>,
    ) -> Result<
        (
            BoundedReplica,
            Result<tilo_actions::ActionOutput, tilo_actions::api::ApiError>,
        ),
        Outcome,
    > {
        if !matches!(&tried, Err(error) if super::part_wait::arriving(error)) {
            return Ok((held, tried));
        }
        self.bounded.insert(key.clone(), held);
        let state = self
            .refresh(workspace_id, super::Needs::Joined)
            .map_err(Outcome::Rejected)?;
        self.hold_chains(&super::Writer::Bounded(key.clone()), &key.0, &state);
        if self.hold_for_chains() {
            return Err(super::api_outcome(&super::part_wait::not_ready()));
        }
        let held = self
            .bounded
            .remove(key)
            .ok_or(Outcome::Rejected(ActionCallError::UnknownAction))?;
        let tried = asked(self, &held);
        if super::call_wait::running(&tried) {
            self.bounded.insert(key.clone(), held);
            return Err(super::api_outcome(&super::part_wait::not_ready()));
        }
        if let Err(error) = &tried
            && super::part_wait::arriving(error)
            && self.hold_for_part(Some(confirmed))
        {
            self.bounded.insert(key.clone(), held);
            return Err(super::api_outcome(error));
        }
        Ok((held, tried))
    }

    fn prepare_bounded_action(
        &self,
        request: &InvokeRequest,
        held: &BoundedReplica,
        descriptor: &tilo_actions::ActionDescriptor,
    ) {
        if descriptor.contract.kind == super::ActionKind::Read {
            self.note_reads(request);
        }
        put_what_it_reads(held, descriptor);
    }

    /// Run `request` on the record at `route` for `who`, in their replica:
    /// only what it offers them, confirmed only by the person's answer the
    /// host holds for it, and saved only once their access, read again,
    /// lets them write every field it changed. `published` is what the record
    /// publishes to its owner, which tells an action not offered to `who`
    /// from one that does not exist.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one action through a caller's replica, each try of it in order and what it saved"
    )]
    pub(super) fn perform_bounded(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        who: &Bounded,
        (request, route, state): (&InvokeRequest, &DocumentRoute, &WorkspaceState),
        (target, arguments, withheld): (
            &tilo_actions::remote::RemoteActionTarget,
            serde_json::Map<String, serde_json::Value>,
            &super::direct::Kept,
        ),
    ) -> Result<
        (
            tilo_actions::ActionOutput,
            Option<tilo_actions::remote::ActorPlace>,
        ),
        Outcome,
    > {
        let opening = super::timing_start();
        let key = self
            .bounded_replica(workspace_id, who, route, state)
            .map_err(Outcome::Rejected)?;
        super::timing_done("bounded.open", opening);
        // Out of the map while it runs, and back in once what it changed is
        // handed over: a caller's next action reuses what they opened instead
        // of opening the record afresh, which costs more with every change
        // the record holds. An action whose change is refused leaves it
        // behind, and one not saved at once lets it go ([`Worker::saved`]),
        // so nothing unsaved is ever acted on.
        let mut held = self
            .bounded
            .remove(&key)
            .ok_or(Outcome::Rejected(ActionCallError::UnknownAction))?;
        // What the caller's own view offers, asked for by address: the record
        // publishes to its owner what the caller may not run, which tells a
        // refused action from one that does not exist. The owner's copy is
        // brought up to date for that answer alone: a caller's own actions
        // never catch this device's copy up (`Worker::perform`).
        //
        // A request applied again after handing its operation off the thread answers from the
        // action as it was published then: the operation may have removed the item it named
        // (a delete), and the resource has no such action now.
        let Some(descriptor) = held
            .held
            .app
            .descriptor(&target.address)
            .or_else(|| held.held.app.handed_descriptor(request.invocation))
        else {
            self.brought_up_to_date(&key.0, state);
            return Err(Outcome::Rejected(
                if self.publishes(&key.0, &target.address) {
                    ActionCallError::NotPermitted
                } else {
                    ActionCallError::UnknownAction
                },
            ));
        };
        self.prepare_bounded_action(request, &held, &descriptor);
        // What is said in a direct conversation the caller's person takes no part in is not
        // there for an action they are offered either ([`super::direct`]).
        let arguments = withheld.given(descriptor.bind_prompt_text(arguments))?;
        let declared = descriptor.contract.need().field;
        let given = request.confirmation.as_ref();
        let confirmed = self
            .part_waits
            .confirmed_or(|| request.approval().is_some());
        let context = super::caller_context(caller, request);
        let running = super::timing_start();
        // Each try goes on off this thread once its worker takes long to answer, the request held
        // until it has ([`super::call_wait`]): the replica is kept for it to be applied again.
        let asked = |worker: &mut Self, held: &BoundedReplica| {
            worker.invoke_handed(
                (workspace_id, &key.0),
                (&held.held.app, request.invocation),
                confirmed,
                Some(descriptor.clone()),
                || crate::worker::Operation {
                    context: Some(context.clone()),
                    appearance: None,
                    address: target.address.clone(),
                    arguments: arguments.clone(),
                    confirm: confirmed,
                },
            )
        };
        let mut tried = asked(self, &held);
        if super::call_wait::running(&tried) {
            self.bounded.insert(key.clone(), held);
            return Err(super::api_outcome(&super::part_wait::not_ready()));
        }
        // What the action named is held by a part of a chain this replica has
        // not been handed (an issue in an older part than the head): the parts
        // it wants are handed, and the action asked once more.
        if matches!(tried, Err(ref error) if !matches!(error, tilo_actions::api::ApiError::ConfirmationRequired))
        {
            let before = held.held.parts.len();
            super::catch_up::hand_all_parts(&key.0, &mut held.held, state);
            if held.held.parts.len() > before {
                tried = asked(self, &held);
                if super::call_wait::running(&tried) {
                    self.bounded.insert(key.clone(), held);
                    return Err(super::api_outcome(&super::part_wait::not_ready()));
                }
            }
        }
        // A part nobody holds yet (a chain's first, or the next one a full chain
        // starts) is told held nowhere, and the action asked again: the chain starts
        // it, where the caller would otherwise be refused for good, its replica let
        // go and the next one asking for the same part again.
        for _ in 0..UNHELD_ROUNDS {
            if !matches!(&tried, Err(tilo_actions::api::ApiError::Unavailable { code, .. }) if code == tilo_actions::api::NOT_READY)
            {
                break;
            }
            super::catch_up::answer_unheld_parts(&key.0, &held.held, state);
            tried = asked(self, &held);
            if super::call_wait::running(&tried) {
                self.bounded.insert(key.clone(), held);
                return Err(super::api_outcome(&super::part_wait::not_ready()));
            }
        }
        let (mut held, tried) =
            self.ask_again_once_arrived(workspace_id, &key, held, (tried, confirmed), asked)?;
        let output = match tried {
            Ok(output) => output,
            Err(tilo_actions::api::ApiError::ConfirmationRequired)
                if given.is_some() && request.approval().is_none() =>
            {
                return Err(Outcome::Rejected(own_word_refused()));
            }
            Err(tilo_actions::api::ApiError::ConfirmationRequired) => {
                // What it carries is read by the names its resource offers this caller.
                let carries = super::carried::read(&descriptor, &arguments, |action| {
                    held.held.app.descriptor(&tilo_actions::ActionAddress::new(
                        target.address.resource.as_str(),
                        action,
                    ))
                });
                let about = self.about(workspace_id, route, &descriptor, arguments, carries);
                return Err(self.waits_for_person(request.invocation, about));
            }
            Err(error) => return Err(super::api_outcome(&error)),
        };
        super::timing_done("bounded.call", running);
        let app = &held.held.app;
        let left_at = app.presence().map(placed);
        let holder = key.0.clone();
        let taking = super::timing_start();
        let taken = super::take_all(app, &holder)
            .map_err(|error| super::commit_failed(error.to_string(), false))?;
        super::timing_done("bounded.take", taking);
        let wrote: Vec<DocumentRoute> = taken.iter().map(|(route, _)| route.clone()).collect();
        let saving = super::timing_start();
        self.save_bounded(
            workspace_id,
            (who, route, state),
            (&held.held, taken),
            (declared.as_deref(), confirmed),
        )?;
        super::timing_done("bounded.save", saving);
        // It holds what it wrote: the next merge into it takes only what
        // others wrote since, never its own change read back.
        super::caught_up_with_own(&holder, &mut held.held, &wrote);
        self.behind_all_but(&super::Writer::Bounded(key.clone()), &wrote);
        self.bounded.insert(key, held);
        Ok((output, left_at))
    }

    /// Save what an action changed for `who` in `replica`, their replica that
    /// made and admitted it: every field of every change checked against
    /// their access read now, so that a member removed or narrowed since
    /// their replica opened writes nothing, and only then handed to the
    /// session: the action is done once the host holds it
    /// ([`Worker::saving`]). The host's own mount of the record takes it up
    /// from the workspace when it is next used, not now.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn save_bounded(
        &mut self,
        workspace_id: WorkspaceId,
        (who, route, state): (&Bounded, &DocumentRoute, &WorkspaceState),
        (replica, taken): (&super::Mounted, Vec<(DocumentRoute, DocumentMutation)>),
        (declared, confirmed): (Option<&str>, bool),
    ) -> Result<(), Outcome> {
        let reading_policy = super::timing_start();
        let policy = self
            .policy_at(workspace_id, who, route)
            .map_err(Outcome::Rejected)?;
        super::timing_done("bounded.policy", reading_policy);
        let permissions = bounded_permissions(&policy, who).map_err(Outcome::Rejected)?;
        let is_roster = who.caller.roster_created_at().is_some();
        let checking = super::timing_start();
        for (changed, mutation) in &taken {
            // An agent's approval decides every record it changes, each on its own.
            if is_roster {
                self.policy_at(workspace_id, who, changed)
                    .map_err(Outcome::Rejected)?;
            }
            // What it wrote, read from the change's operations by the replica
            // that admitted it: neither the record nor its history is read for
            // what one change touched. Only a record the replica does not hold
            // as changes (a part it started whole) is read before and diffed.
            let reading = super::timing_start();
            let written = reads::document_id(&changed.document)
                .ok()
                .and_then(|record| replica.app.written_keys(&record, mutation))
                .map(Ok);
            super::timing_done("check.written_keys", reading);
            let before = match written {
                Some(_) => None,
                None => state.document(changed).ok().map(|(bytes, _)| bytes),
            };
            let refused = refused_writes(
                &permissions,
                (&changed.app_id, declared),
                before.as_deref(),
                mutation,
                (written, confirmed),
            )
            .map_err(|reason| super::commit_failed(reason, false))?;
            if !refused.is_empty() {
                tracing::info!(record = %changed.document, ?refused,
                    "workspace actions: a change writes what its author may not");
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
        }
        super::timing_done("bounded.check", checking);
        let committing = super::timing_start();
        for (changed, mutation) in taken {
            self.commit(workspace_id, &changed, &mutation)
                .map_err(|reason| super::commit_failed(reason, true))?;
        }
        super::timing_done("bounded.commit", committing);
        Ok(())
    }

    /// The app this worker holds mounted at `holder`, if it holds one there.
    pub(super) fn app_at(&self, holder: &DocumentRoute) -> Option<&ConfinedReplica> {
        self.mounted.get(holder).map(|mounted| &*mounted.app)
    }

    /// Whether the record at `holder` publishes `address` to its owner: what
    /// tells an action a caller may not run from one that does not exist.
    pub(super) fn publishes(&self, holder: &DocumentRoute, address: &ActionAddress) -> bool {
        self.app_at(holder)
            .is_some_and(|app| app.descriptor(address).is_some())
    }
}

/// Where an action left its actor, for the API that shows it working.
pub(super) fn placed((anchor, head): (LiveAnchor, LiveAnchor)) -> ActorPlace {
    ActorPlace {
        anchor: hex::encode(anchor.cursor),
        head: hex::encode(head.cursor),
    }
}

impl super::Shared {
    /// The person whose device this service is: the root the service signs under.
    fn owner(&self) -> Option<ui::permissions::policy::PersonId> {
        Some(ui::permissions::policy::PersonId::from_root(
            self.link.get()?.user_public_key(),
        ))
    }

    /// Whether `caller` may do `kind` across the workspace `workspace_id`
    /// ([`may_across`]): what a content step is checked against before a byte
    /// of it is taken, which needs nothing of the service's own thread.
    pub(super) fn may_across(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        kind: ActionKind,
    ) -> bool {
        self.owner()
            .is_some_and(|member| may_across(self, &member, workspace_id, caller, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    const APP: &str = ui::apps::TASKS;

    /// A record holding a title and an issue's comments, its one change, and
    /// the document to write the next change on.
    fn record() -> (Vec<u8>, automerge::AutoCommit) {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "title", "Launch").unwrap();
        let issues = doc
            .put_object(automerge::ROOT, "issues", automerge::ObjType::Map)
            .unwrap();
        let issue = doc
            .put_object(&issues, "i1", automerge::ObjType::Map)
            .unwrap();
        doc.put_object(&issue, "comments", automerge::ObjType::Map)
            .unwrap();
        doc.commit();
        let genesis = doc.save();
        (genesis, doc)
    }

    /// The change `write` makes on `doc`, as an increment.
    fn change(
        doc: &mut automerge::AutoCommit,
        write: impl FnOnce(&mut automerge::AutoCommit),
    ) -> DocumentMutation {
        let before = doc.get_heads();
        write(doc);
        doc.commit();
        DocumentMutation::Incremental(doc.save_after(&before))
    }

    fn as_role(role: ui::permissions::Role) -> ui::permissions::PermissionContext {
        ui::permissions::PermissionContext::member_of(
            WORKSPACE,
            &ui::permissions::MemberId::person("grace"),
            role,
        )
        .at_unix_time(1_900_000_000)
    }

    fn comment(doc: &mut automerge::AutoCommit) {
        let (_, issues) = doc.get(automerge::ROOT, "issues").unwrap().unwrap();
        let (_, issue) = doc.get(&issues, "i1").unwrap().unwrap();
        let (_, comments) = doc.get(&issue, "comments").unwrap().unwrap();
        doc.put(&comments, "c1", "Looks good").unwrap();
    }

    #[test]
    fn a_commenter_writes_comments_and_nothing_else_a_viewer_nothing() {
        use ui::permissions::Role;
        let (before, mut doc) = record();
        let commented = change(&mut doc.clone(), comment);
        let retitled = change(&mut doc.clone(), |doc| {
            doc.put(automerge::ROOT, "title", "Renamed").unwrap();
        });
        let both = change(&mut doc, |doc| {
            comment(doc);
            doc.put(automerge::ROOT, "title", "Renamed").unwrap();
        });
        let refused = |role, mutation: &DocumentMutation| {
            refused_writes(
                &as_role(role),
                (APP, None),
                Some(&before),
                mutation,
                (None, false),
            )
            .unwrap()
        };
        assert!(refused(Role::Commenter, &commented).is_empty());
        assert_eq!(
            refused(Role::Commenter, &retitled),
            vec![TreePath::root().app(APP)],
            "a commenter's change outside comments is not saved"
        );
        assert_eq!(
            refused(Role::Commenter, &both),
            vec![TreePath::root().app(APP)],
            "nor is a comment carrying a change beside it"
        );
        assert!(!refused(Role::Viewer, &commented).is_empty());
        assert!(!refused(Role::Viewer, &retitled).is_empty());
        assert!(refused(Role::Editor, &both).is_empty());
        // An action declared below comments (filing a report) writes where its
        // app keeps it: a commenter's, never a viewer's.
        let filed = |role| {
            refused_writes(
                &as_role(role),
                (APP, Some(policy::roles::COMMENTS_FIELD)),
                Some(&before),
                &retitled,
                (None, false),
            )
            .unwrap()
        };
        assert!(filed(Role::Commenter).is_empty());
        assert!(!filed(Role::Viewer).is_empty());
    }
}
