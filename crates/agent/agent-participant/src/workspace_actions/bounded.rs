//! Callers the host acts for within bounds: a person the workspace admits who
//! is not the host's own, and an agent acting for any person, the host's own
//! included (`tilo mcp` on a computer `tilo login` made a device of theirs).
//!
//! Such a caller's record actions run in a confined replica of their own,
//! opened as them ([`crate::ReplicaActor::Member`]): the person's standing as
//! the workspace's policy gives it now and the sandbox its governance names,
//! and for an agent that standing bounded as every agent's is. What that
//! replica surveys is the one answer to both what the caller's listing shows
//! and what the host runs for them: an action it does not offer is refused,
//! as not permitted when the record publishes it to someone else.
//!
//! Nobody acting for a person confirms by their own word, and the host does
//! not take a person's word from a device that may be their agent: where an
//! action asks first, the host puts it to the person on their own devices
//! ([`WorkspaceRecordsLink::ask_person`]) and runs it on their answer, which
//! it keeps as a token bound to that one invocation, its target and its
//! arguments ([`Witnesses`]). A confirmation the host did not issue for the
//! invocation is refused.
//!
//! Before what such an action changed is saved, the host reads the caller's
//! standing again and checks every field the change writes against it
//! ([`refused_writes`]): a commenter's change outside comments and reactions,
//! or anything a viewer's action wrote, is never saved.

use super::{
    ActionHostError, Arc, Caller, CallerPrincipal, ConfinedReplica, Confirmation, DocumentMutation,
    DocumentRoute, HashMap, HashSet, Instant, InvocationId, InvokeRequest, Mounted, Mutex, Outcome,
    Seed, Worker, WorkspaceState, reads,
};
use tilo_actions::InvocationUpdate;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    ActorPlace, ConfirmationQuestion, ConfirmationReply, InvocationReceipt, InvocationState,
};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::live::LiveAnchor;
use ui::permissions::policy::{self, ActionKind, Answer, TreePath};

/// How long the person's answer to one invocation stays good for running it.
const WITNESS_LIFETIME: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How long a caller's replica is kept on the standing it was opened with
/// before the host reads their standing again for it.
const STANDING_FRESHNESS_SECS: u64 = 30;

/// Who a bounded caller is: the person whose standing bounds them, as that
/// person's device is admitted, and the agent acting for them, if one is.
#[derive(Clone)]
pub(super) struct Bounded {
    /// The caller as the transport admitted it: what decides whom the host
    /// acts for ([`Caller::invoker`]).
    pub(super) caller: Caller,
    /// The person, on the device the request came from: what the workspace's
    /// policy is asked about.
    pub(super) person_caller: Caller,
    /// The person's policy principal id.
    pub(super) person: String,
    /// The agent acting for the person, by its agent id.
    pub(super) agent: Option<String>,
    /// The name that agent goes by ([`Caller::agent_name`]).
    pub(super) agent_name: Option<String>,
}

/// The replica a bounded caller acts through, by the record it holds and who.
pub(super) type BoundedKey = (DocumentRoute, String, Option<String>);

/// A caller's replica of one record, and the standing it was opened with.
pub(super) struct BoundedReplica {
    pub(super) held: Mounted,
    standing: policy::Standing,
    sandbox: Vec<collab_types::caps::CapOverride>,
    unix_time: u64,
}

/// What a person's answer is bound to: one invocation by one caller of one
/// target with exactly these arguments ([`tilo_actions::remote::argument_digest`]),
/// as the broker binds the answers it hands an agent.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct WitnessBinding {
    caller: Caller,
    workspace_id: u64,
    invocation: InvocationId,
    provider: String,
    resource: String,
    action: String,
    argument_digest: [u8; 32],
}

impl WitnessBinding {
    pub(super) fn of(workspace_id: u64, caller: &Caller, request: &InvokeRequest) -> Self {
        Self {
            caller: caller.clone(),
            workspace_id,
            invocation: request.invocation,
            provider: request.target.provider.clone(),
            resource: request.target.address.resource.clone(),
            action: request.target.address.action.clone(),
            argument_digest: tilo_actions::remote::argument_digest(
                request.argument.to_slot().as_deref(),
            ),
        }
    }
}

/// The answers the host holds for invocations people allowed, by token.
#[derive(Default)]
pub(super) struct Witnesses(Mutex<HashMap<String, (WitnessBinding, Instant)>>);

impl Witnesses {
    /// Keep the person's answer for `binding`, naming it by a new token.
    pub(super) fn issue(&self, binding: WitnessBinding) -> Result<String, String> {
        let mut id = [0_u8; 32];
        getrandom::fill(&mut id).map_err(|error| error.to_string())?;
        let token = hex::encode(id);
        let now = Instant::now();
        let mut held = self.0.lock_recover();
        held.retain(|_, (_, until)| *until > now);
        held.insert(
            token.clone(),
            (binding, now.checked_add(WITNESS_LIFETIME).unwrap_or(now)),
        );
        drop(held);
        Ok(token)
    }

    /// Whether `token` names an answer the host issued for exactly
    /// `binding`, still good; it is spent either way it matches.
    pub(super) fn take(&self, token: &str, binding: &WitnessBinding) -> bool {
        let mut held = self.0.lock_recover();
        let matches = held
            .get(token)
            .is_some_and(|(bound, until)| *until > Instant::now() && bound == binding);
        if matches {
            held.remove(token);
        }
        matches
    }
}

/// Whether `given` is the person's own answer to this invocation: a token the
/// host issued for `binding`. Nothing else confirms for a bounded caller.
pub(super) fn witnessed(
    witnesses: &Witnesses,
    given: Option<&Confirmation>,
    binding: &WitnessBinding,
) -> bool {
    matches!(given, Some(Confirmation::Witnessed(token)) if witnesses.take(token, binding))
}

/// The refusal of a confirmation a bounded caller gave by its own word.
pub(super) fn own_word_refused() -> ActionHostError {
    ActionHostError::Refused {
        reason: "only the person confirms this action, on their own device: invoke it without \
                 a confirmation and the host asks them"
            .into(),
    }
}

/// The node below the workspace a change writing at `keys` (storage keys, top
/// down) of a record of `app` acts at: its comments or reactions when it
/// writes inside them, wherever a model keeps them; else the field the action
/// that wrote it is declared below (`#[action(field = ...)]`: filing a report
/// is a comment, whose app keeps it where its own footprint says), else the
/// record.
fn written_node(app: &str, keys: &[String], declared: Option<&str>) -> TreePath {
    let record = TreePath::root().app(app);
    let inside = |field: &str| keys.iter().any(|key| key == field);
    if inside(policy::roles::COMMENTS_FIELD) {
        record.field(policy::roles::COMMENTS_FIELD)
    } else if inside(policy::roles::REACTIONS_FIELD) {
        record.field(policy::roles::REACTIONS_FIELD)
    } else if let Some(field) = declared {
        record.field(field)
    } else {
        record
    }
}

fn key_of(prop: &automerge::Prop) -> String {
    match prop {
        automerge::Prop::Map(key) => key.clone(),
        automerge::Prop::Seq(index) => index.to_string(),
    }
}

/// The keys a change writes, as its record's replica reports them.
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
    let mut seen = HashSet::new();
    let written = written.unwrap_or_else(|| written_keys(before, mutation));
    for keys in written? {
        let node = written_node(app, &keys, declared);
        if !seen.insert(node.clone()) {
            continue;
        }
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
}

/// The question the person is put about `request`, which `caller` asks the
/// host to run in `workspace`: who asks, in which run, and exactly what
/// would happen ([`About`]), as the host knows it rather than as the caller
/// words it.
pub(super) fn question(
    caller: &Caller,
    workspace: &str,
    request: &InvokeRequest,
    about: &About,
) -> Result<ConfirmationQuestion, String> {
    let mut id = [0_u8; 32];
    getrandom::fill(&mut id).map_err(|error| error.to_string())?;
    let agent = match caller.principal() {
        // An agent by the name its key goes by, as History names its
        // changes; else its id.
        CallerPrincipal::Agent(agent) => caller.agent_name().unwrap_or(agent).to_owned(),
        _ => "One of your devices".into(),
    };
    // The run as the agent titled it (`_meta` run.title); a run that goes
    // by the agent's own name says nothing more.
    let run = super::said_run_title(request)
        .filter(|run| *run != agent)
        .unwrap_or_default();
    let contract = &about.descriptor.contract;
    Ok(ConfirmationQuestion {
        id,
        device_label: agent,
        workspace: workspace.to_owned(),
        run,
        action_label: about.descriptor.action_label.clone(),
        resource_label: about.record.clone(),
        arguments: tilo_actions::Draft::render(&about.descriptor, &about.arguments).summary(),
        risk: contract.risk.name().to_owned(),
        kind: contract.kind.to_string(),
        undoable: contract.can_be_undone(),
    })
}

/// `who`'s standing as the host decides for them: the person's standing as
/// `policy` gives it now, held to the bounds the one reading of a caller
/// gives ([`Caller::invoker`]): for an agent, the delegation a device gives
/// its agents. Refused when the policy is someone else's.
pub(super) fn bounded_standing(
    policy: &super::ViewReadPolicy,
    who: &Bounded,
) -> Result<policy::Standing, ActionHostError> {
    policy
        .standing
        .clone()
        .bounded_as(&who.caller.invoker(&policy.workspace))
        .ok_or(ActionHostError::NotPermitted)
}

/// `who`'s permissions under `policy` ([`bounded_standing`]).
pub(super) fn bounded_permissions(
    policy: &super::ViewReadPolicy,
    who: &Bounded,
) -> Result<ui::permissions::PermissionContext, ActionHostError> {
    Ok(super::ViewReadPolicy {
        standing: bounded_standing(policy, who)?,
        ..policy.clone()
    }
    .permissions())
}

/// The replica actor the host runs `who`'s actions as, on their standing
/// under `policy` ([`bounded_standing`]).
pub(super) fn member_actor(
    policy: &super::ViewReadPolicy,
    who: &Bounded,
) -> Result<crate::ReplicaActor, ActionHostError> {
    Ok(crate::ReplicaActor::Member {
        standing: bounded_standing(policy, who)?,
        sandbox: policy.sandbox.clone(),
        unix_time: policy.unix_time,
        agent: who.agent.clone(),
        agent_name: who.agent_name.clone(),
    })
}

impl Worker {
    /// `caller` as the host acts for them within bounds: a person who is not
    /// the host's own, an agent a person's own device handed its session to,
    /// or an agent device a person certified. `None` for the host's own person, and for anyone the host acts for
    /// in no person's name.
    pub(super) fn bounded(&self, caller: &Caller) -> Option<Bounded> {
        match caller.principal() {
            CallerPrincipal::Person(person) if *person == self.participant => None,
            CallerPrincipal::Person(person) => Some(Bounded {
                caller: caller.clone(),
                person_caller: caller.clone(),
                person: person.clone(),
                agent: None,
                agent_name: None,
            }),
            CallerPrincipal::Agent(agent) => {
                // An agent the person's device handed its session to acts on
                // that device's admission; an agent device the person
                // certified, on its own device's: either is bounded by the
                // person it acts for, as `Caller::invoker` reads it.
                let person_caller = caller.delegating_person().or_else(|| {
                    let person = Caller::authenticated(
                        *caller.user_public_key()?,
                        *caller.device_public_key()?,
                    );
                    Some(match caller.workspace() {
                        Some(workspace) => person.in_workspace(workspace),
                        None => person,
                    })
                })?;
                let CallerPrincipal::Person(person) = person_caller.principal() else {
                    return None;
                };
                Some(Bounded {
                    caller: caller.clone(),
                    person: person.clone(),
                    agent: Some(agent.clone()),
                    agent_name: caller.agent_name().map(str::to_owned),
                    person_caller,
                })
            }
            CallerPrincipal::App(_) | CallerPrincipal::Bot(_) => None,
        }
    }

    /// The person's standing in the workspace `cluster` as its policy gives
    /// it now, where they may read `record` (an app and its record) when one
    /// is named: refused for a person it does not admit (a removed member, a
    /// revoked device). The host's own person, in a workspace no governance
    /// here answers for, is its owner, as the host's own mount holds it.
    pub(super) fn policy_for(
        &self,
        workspace_id: u64,
        who: &Bounded,
        cluster: &str,
        record: Option<(&str, &str)>,
    ) -> Result<super::ViewReadPolicy, ActionHostError> {
        let governed = self.shared.link.get().and_then(|link| match record {
            Some((app, record)) => {
                link.view_read_policy(workspace_id, &who.person_caller, app, record)
            }
            None => link.workspace_policy(workspace_id, &who.person_caller),
        });
        match governed {
            Some(policy) if policy.workspace == cluster && policy.standing.person == who.person => {
                Ok(policy)
            }
            None if who.person == self.participant && !self.governed(workspace_id) => {
                let owner = ui::permissions::PermissionContext::member_of(
                    cluster,
                    who.person.clone(),
                    ui::permissions::Role::Owner,
                );
                Ok(super::ViewReadPolicy {
                    workspace: cluster.to_owned(),
                    standing: owner.standing().clone(),
                    sandbox: Vec::new(),
                    unix_time: tilo_clock::unix_now_ns() / 1_000_000_000,
                })
            }
            Some(_) | None => Err(ActionHostError::NotPermitted),
        }
    }

    /// [`Self::policy_for`] the record at `route`.
    fn policy_at(
        &self,
        workspace_id: u64,
        who: &Bounded,
        route: &DocumentRoute,
    ) -> Result<super::ViewReadPolicy, ActionHostError> {
        let record = Some((route.app_id.as_str(), route.record.as_str()));
        self.policy_for(workspace_id, who, &route.cluster_id, record)
    }

    /// Whether `caller` may do `kind` across the workspace `workspace_id`
    /// (its content, a new record of it): the host's own person and their
    /// agents may; anyone else as the workspace's policy gives them now, an
    /// agent held to what an agent of its person may do.
    pub(super) fn may_across(&self, workspace_id: u64, caller: &Caller, kind: ActionKind) -> bool {
        if super::acts_for(caller, &self.participant) {
            return true;
        }
        let Some(who) = self.bounded(caller) else {
            return false;
        };
        self.shared
            .link
            .get()
            .and_then(|link| link.workspace_policy(workspace_id, &who.person_caller))
            .and_then(|policy| bounded_permissions(&policy, &who).ok())
            .is_some_and(|permissions| permissions.answer(&TreePath::root(), kind) == Answer::Allow)
    }

    /// Whether a governance on this host answers for who is in `workspace_id`.
    fn governed(&self, workspace_id: u64) -> bool {
        self.shared.link.get().is_some_and(|link| {
            link.admits(
                workspace_id,
                &Caller::authenticated(
                    self.member.endpoint.host_cert.user_public_key,
                    self.member.endpoint.host_cert.device_public_key,
                ),
            )
        })
    }

    /// Whether `who` may see the workspace's records at all.
    pub(super) fn admits_bounded(&self, workspace_id: u64, who: &Bounded) -> bool {
        (who.person == self.participant && !self.governed(workspace_id))
            || self
                .shared
                .link
                .get()
                .is_some_and(|link| link.admits(workspace_id, &who.person_caller))
    }

    /// `who`'s replica of the record at `route` (a part: of the record it is
    /// held beside), opened as them on their standing now, or the one held
    /// for them while that standing is unchanged.
    pub(super) fn bounded_replica(
        &mut self,
        workspace_id: u64,
        who: &Bounded,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<BoundedKey, ActionHostError> {
        let unavailable = |reason: String| ActionHostError::Unavailable { reason };
        let policy = self.policy_at(workspace_id, who, route)?;
        // Where the host's own mount holds it, so a part is held beside the
        // same record for everyone.
        self.mount(route, state)
            .map_err(|_| ActionHostError::UnknownAction)?;
        let holder = self.holder_of(route);
        let key = (holder.clone(), who.person.clone(), who.agent.clone());
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        let current = self.bounded.get(&key).is_some_and(|held| {
            held.standing == policy.standing
                && held.sandbox == policy.sandbox
                && policy.unix_time.saturating_sub(held.unix_time) < STANDING_FRESHNESS_SECS
        });
        if !current {
            self.bounded.remove(&key);
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
            let (bytes, heads) = state
                .document(&holder)
                .map_err(|_| ActionHostError::UnknownAction)?;
            let app = ConfinedReplica::open(
                &self.catalogue,
                &holder.app_id,
                &holder.cluster_id,
                &who.person,
                &member_actor(&policy, who)?,
                Seed::Existing {
                    record: reads::record_id(&holder.record).map_err(unavailable)?,
                    changes: vec![bytes],
                },
            )
            .map_err(|error| unavailable(error.to_string()))?;
            let app = self.lend(app, &holder.cluster_id);
            self.bounded.insert(
                key.clone(),
                BoundedReplica {
                    held: Mounted {
                        app,
                        merged: Some(heads),
                        parts: HashMap::new(),
                        used: clock,
                    },
                    standing: policy.standing,
                    sandbox: policy.sandbox,
                    unix_time: policy.unix_time,
                },
            );
        }
        let held = self
            .bounded
            .get_mut(&key)
            .ok_or_else(|| unavailable("the record did not mount".into()))?;
        held.held.used = clock;
        if holder != *route && !held.held.parts.contains_key(route) {
            let (part, heads) = state
                .document(route)
                .map_err(|_| ActionHostError::UnknownAction)?;
            held.held
                .app
                .apply_record(
                    &reads::record_id(&route.record).map_err(unavailable)?,
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
        workspace_id: u64,
        who: &Bounded,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, ActionHostError> {
        let key = self.bounded_replica(workspace_id, who, route, state)?;
        Ok(self
            .bounded
            .get(&key)
            .map(|held| held.held.app.survey())
            .unwrap_or_default())
    }

    /// Put `request`, which `caller` (served within bounds) asked for and
    /// which needs the person's confirmation, to that person on their own
    /// devices, as `about` describes it: answered accepted, it runs on their
    /// allowing it and ends refused otherwise. `None` when it cannot be
    /// asked: the caller gave a confirmation of its own, the person was
    /// asked already, or this host has nobody to ask.
    pub(super) fn ask_person(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
        about: &About,
    ) -> Option<InvocationReceipt> {
        let who = self.bounded(caller)?;
        if request.confirmation.is_some() {
            return None;
        }
        let invocation = request.invocation;
        let workspace = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.cluster_id(workspace_id))?;
        let question = question(caller, &workspace, request, about).ok()?;
        if self
            .ledger()
            .entries
            .get_mut(&invocation)
            .is_none_or(|entry| std::mem::replace(&mut entry.asked, true))
        {
            return None;
        }
        let Some(asked) = self
            .shared
            .link
            .get()
            .and_then(|link| link.ask_person(workspace_id, &who.person_caller, question))
        else {
            if let Some(entry) = self.ledger().entries.get_mut(&invocation) {
                entry.asked = false;
            }
            return None;
        };
        let shared = Arc::clone(&self.shared);
        let asking = Asking {
            workspace_id,
            invocation,
            binding: WitnessBinding::of(workspace_id, caller, request),
        };
        asked.then(move |reply| shared.answered(asking, reply.as_ref()));
        Some(InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt())
    }

    /// `invocation` waits for the person's answer, asked as `about` says.
    fn waits_for_person(&self, invocation: InvocationId, about: About) -> Outcome {
        if let Some(entry) = self.ledger().entries.get_mut(&invocation) {
            entry.about = Some(Box::new(about));
        }
        Outcome::Rejected(ActionHostError::confirmation_required())
    }

    /// What the person is asked about running `descriptor` with `arguments`
    /// on the record at `route` ([`About`]).
    pub(super) fn about(
        &self,
        workspace_id: u64,
        route: &DocumentRoute,
        descriptor: &tilo_actions::ActionDescriptor,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> About {
        About {
            record: self
                .record_name(workspace_id, route)
                .unwrap_or_else(|| descriptor.resource_label.clone()),
            descriptor: descriptor.clone(),
            arguments,
        }
    }

    /// Run `request` on the record at `route` for `who`, in their replica:
    /// only what it offers them, confirmed only by the person's answer the
    /// host holds for it, and saved only once their standing, read again,
    /// lets them write every field it changed. `published` is what the record
    /// publishes to its owner, which tells an action not offered to `who`
    /// from one that does not exist.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn perform_bounded(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        who: &Bounded,
        (request, route, state): (&InvokeRequest, &DocumentRoute, &WorkspaceState),
        (target, arguments): (
            &tilo_actions::remote::RemoteActionTarget,
            serde_json::Map<String, serde_json::Value>,
        ),
    ) -> Result<
        (
            tilo_actions::ActionOutput,
            Option<tilo_actions::remote::ActorPlace>,
        ),
        Outcome,
    > {
        let key = self
            .bounded_replica(workspace_id, who, route, state)
            .map_err(Outcome::Rejected)?;
        // Out of the map while it runs, and back in once what it changed is
        // handed over: a caller's next action reuses what they opened instead
        // of opening the record afresh, which costs more with every change
        // the record holds. An action whose change is refused leaves it
        // behind, and one not saved at once lets it go ([`Worker::saved`]),
        // so nothing unsaved is ever acted on.
        let held = self
            .bounded
            .remove(&key)
            .ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?;
        let app = &held.held.app;
        // What the caller's own view offers, asked for by address: the record
        // publishes to its owner what the caller may not run, which tells a
        // refused action from one that does not exist.
        let Some(descriptor) = app.descriptor(&target.address) else {
            return Err(Outcome::Rejected(
                if self.publishes(&key.0, &target.address) {
                    ActionHostError::NotPermitted
                } else {
                    ActionHostError::UnknownAction
                },
            ));
        };
        let arguments = descriptor.bind_prompt_text(arguments);
        let declared = descriptor.contract.need().field;
        let witness = self
            .ledger()
            .entries
            .get(&request.invocation)
            .and_then(|entry| entry.witness.clone())
            .map(Confirmation::Witnessed);
        let given = witness.as_ref().or(request.confirmation.as_ref());
        let binding = WitnessBinding::of(workspace_id, caller, request);
        let confirmed = witnessed(&self.shared.witnesses, given, &binding);
        let context = super::caller_context(caller, request);
        let output = match app.invoke_in(
            context.as_ref(),
            None,
            &target.address,
            &arguments,
            confirmed,
        ) {
            Ok(output) => output,
            Err(tilo_actions::door::DoorError::ConfirmationRequired) if given.is_some() => {
                return Err(Outcome::Rejected(own_word_refused()));
            }
            Err(tilo_actions::door::DoorError::ConfirmationRequired) => {
                let about = self.about(workspace_id, route, &descriptor, arguments);
                return Err(self.waits_for_person(request.invocation, about));
            }
            Err(error) => return Err(super::door_outcome(&error)),
        };
        let left_at = app.presence().map(placed);
        let holder = key.0.clone();
        let taken = super::take_all(app, &holder)
            .map_err(|error| super::commit_failed(error.to_string(), false))?;
        self.save_bounded(
            workspace_id,
            (who, route, state),
            (&holder, taken),
            (declared.as_deref(), confirmed),
        )?;
        self.bounded.insert(key, held);
        Ok((output, left_at))
    }

    /// Save what an action changed for `who`: every field of every change
    /// checked against their standing read now, so that a member removed or
    /// narrowed since their replica opened writes nothing, and only then
    /// applied here and handed to the session: the action is done once the
    /// host holds it ([`Worker::saving`]).
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn save_bounded(
        &mut self,
        workspace_id: u64,
        (who, route, state): (&Bounded, &DocumentRoute, &WorkspaceState),
        (holder, taken): (&DocumentRoute, Vec<(DocumentRoute, DocumentMutation)>),
        (declared, confirmed): (Option<&str>, bool),
    ) -> Result<(), Outcome> {
        let policy = self
            .policy_at(workspace_id, who, route)
            .map_err(Outcome::Rejected)?;
        let permissions = bounded_permissions(&policy, who).map_err(Outcome::Rejected)?;
        for (changed, mutation) in &taken {
            // What it wrote, from the record's own replica: neither the record
            // nor its history is read for what one change touched. Only a
            // record the replica does not hold is read before and diffed.
            let written = reads::record_id(&changed.record)
                .ok()
                .and_then(|record| self.app_at(holder)?.changed_keys(&record, mutation));
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
                tracing::info!(record = %changed.record, ?refused,
                    "workspace actions: a change writes what its author may not");
                return Err(Outcome::Rejected(ActionHostError::NotPermitted));
            }
        }
        for (changed, mutation) in taken {
            if let Some(app) = self.app_at(holder) {
                let merged = if &changed == holder {
                    app.apply(&mutation)
                } else {
                    reads::record_id(&changed.record)
                        .map_err(crate::apps::AppError::Merge)
                        .and_then(|record| app.apply_record(&record, &mutation))
                };
                if let Err(error) = merged {
                    tracing::warn!(%error, "workspace actions: a change made for someone did not merge here");
                }
            }
            self.commit(workspace_id, &changed, &mutation)
                .map_err(|reason| super::commit_failed(reason, true))?;
        }
        Ok(())
    }

    /// The app this worker holds mounted at `holder`, if it holds one there.
    pub(super) fn app_at(&self, holder: &DocumentRoute) -> Option<&ConfinedReplica> {
        self.mounted.get(holder).map(|mounted| &mounted.app)
    }

    /// Whether the record at `holder` publishes `address` to its owner: what
    /// tells an action a caller may not run from one that does not exist.
    pub(super) fn publishes(&self, holder: &DocumentRoute, address: &ActionAddress) -> bool {
        self.app_at(holder)
            .is_some_and(|app| app.descriptor(address).is_some())
    }
}

/// Where an action left its actor, for the door that shows it working.
pub(super) fn placed((anchor, head): (LiveAnchor, LiveAnchor)) -> ActorPlace {
    ActorPlace {
        anchor: hex::encode(anchor.cursor),
        head: hex::encode(head.cursor),
    }
}

/// What a bounded caller's invocation waiting on the person needs to go on.
pub(super) struct Asking {
    pub(super) workspace_id: u64,
    pub(super) invocation: InvocationId,
    pub(super) binding: WitnessBinding,
}

impl super::Shared {
    /// The person answered `asking`'s question with `reply`: on their
    /// allowing it, the invocation runs again with the token naming their
    /// answer; otherwise it ends refused.
    pub(super) fn answered(
        self: &Arc<Self>,
        asking: Asking,
        reply: Option<&Result<ConfirmationReply, ActionHostError>>,
    ) {
        let Asking {
            workspace_id,
            invocation,
            binding,
        } = asking;
        let failed = |message: &str| tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: tilo_actions::door::DoorError::ConfirmationRequired
                    .code()
                    .to_owned(),
                message: message.to_owned(),
                details: None,
                retryable: false,
            },
        };
        let update = match reply {
            Some(Ok(ConfirmationReply::AllowOnce)) => match self.witnesses.issue(binding) {
                Ok(token) => {
                    if let Some(entry) = self.ledger.lock_recover().entries.get_mut(&invocation) {
                        entry.witness = Some(token);
                    }
                    let (done, _) = tilo_actions::remote::pending();
                    let job = super::Job::Invoke {
                        workspace_id,
                        invocation,
                        done,
                    };
                    let sent = self
                        .jobs
                        .lock_recover()
                        .as_ref()
                        .is_some_and(|jobs| jobs.send((Instant::now(), job)).is_ok());
                    if sent {
                        return;
                    }
                    failed("the host's workspace actions stopped before it ran")
                }
                Err(error) => failed(&error),
            },
            Some(Ok(ConfirmationReply::Deny)) => failed("the person did not allow it"),
            Some(Err(_)) | None => failed("the person did not answer in time"),
        };
        if let Some(entry) = self.ledger.lock_recover().entries.get_mut(&invocation)
            && !entry.update.is_terminal()
        {
            entry.update = update;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    const APP: &str = "so.tilo.tasks";

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
        ui::permissions::PermissionContext::member_of(WORKSPACE, "grace", role)
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
