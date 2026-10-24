//! The toolkit's permission gate.
//!
//! Two questions, each answered by the one model that owns it:
//!
//! - **May this person (or their agent) perform this action?** Decided by
//!   `tilo-action-policy` over the room's grants: a [`Standing`] names the
//!   person, the room's policy and any delegation bounding them, and
//!   [`PermissionScope::Action`] names a node below the room and the action's
//!   kind.
//! - **May this app's code reach this path, device or host?** Decided by the
//!   sandbox capabilities of [`collab_types::caps`]
//!   ([`PermissionScope::Sandbox`]).
//!
//! The host installs both via [`PermissionContext::new`]; nothing here decides
//! anything itself.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct PolicyTimeSampler(Arc<dyn Fn() -> Result<u64, String> + Send + Sync>);

impl std::fmt::Debug for PolicyTimeSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PolicyTimeSampler(..)")
    }
}

impl PartialEq for PolicyTimeSampler {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PolicyTimeSampler {}

/// The host's revocation of the `Once` grants an in-app admission spent.
#[derive(Clone)]
struct OnceSpender(Arc<dyn Fn(&[policy::GrantId]) -> Result<(), String> + Send + Sync>);

impl std::fmt::Debug for OnceSpender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnceSpender(..)")
    }
}

impl PartialEq for OnceSpender {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for OnceSpender {}

pub use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope,
    Subject, decide_capability,
};
pub use collab_types::role::Role;
pub use tilo_action_policy as policy;
pub use tilo_action_policy::{ActionKind, Answer, KindSet, Standing, TreePath};

/// The id the local, unrestricted user acts as under
/// [`PermissionContext::permissive`].
pub const LOCAL_USER: &str = "local";

/// The room [`PermissionContext::permissive`] and [`PermissionContext::member`]
/// decide in, when no host names one.
pub const LOCAL_ROOM: &str = "local";

/// Which system app a presence key or a permission scope refers to.
pub use collab_types::presence::AppKind;

/// A person's stable id, as apps and presence spell it.
///
/// Interchangeable with [`Principal::Person`] — see
/// [`UserId::principal`]. Kept as a distinct newtype since presence and
/// app code pass a bare user string around and gain nothing from a
/// tagged enum.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

impl UserId {
    pub fn principal(&self) -> Principal {
        Principal::Person(self.0.clone())
    }
}

/// What a widget or a launch asks permission for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionScope {
    /// An action of `kind` at `node`, a path below the room (beginning at the
    /// app, or empty for the room itself).
    Action { node: TreePath, kind: ActionKind },
    /// Reaching a sandboxed resource (a path, a device, a host) with `access`.
    Sandbox {
        resource: ResourceRef,
        access: Access,
    },
}

impl PermissionScope {
    /// An action of `kind` on the room itself.
    pub fn workspace(kind: ActionKind) -> Self {
        Self::Action {
            node: TreePath::root(),
            kind,
        }
    }

    pub fn app(app_id: impl Into<String>, kind: ActionKind) -> Self {
        Self::Action {
            node: TreePath::root().app(app_id),
            kind,
        }
    }

    /// One exported operation on one resource within an app. A grant on the
    /// app or the resource kind covers it; a grant on this action of this
    /// resource covers it and nothing else.
    pub fn action(
        app_id: impl Into<String>,
        resource_kind: impl Into<String>,
        resource_id: impl Into<String>,
        action_id: impl Into<String>,
        kind: ActionKind,
    ) -> Self {
        Self::Action {
            node: action_node(
                &app_id.into(),
                &resource_kind.into(),
                &resource_id.into(),
                &action_id.into(),
            ),
            kind,
        }
    }

    /// One of the room's governance actions (inviting, granting, settings).
    pub fn governance(action: &str) -> Self {
        Self::Action {
            node: TreePath::root()
                .kind(policy::roles::ROOM_KIND)
                .action(action),
            kind: policy::roles::GOVERNANCE_KIND,
        }
    }

    pub fn app_kind(app: AppKind, kind: ActionKind) -> Self {
        Self::app(app.app_id(), kind)
    }

    /// Reaching a filesystem path. Room roles carry no authority over the
    /// filesystem, so this always needs an explicit sandbox capability.
    pub fn path(path: impl Into<String>, access: Access) -> Self {
        Self::Sandbox {
            resource: ResourceRef::FsPath(path.into()),
            access,
        }
    }
}

/// The node below the room of action `action_id` on resource `resource_id`
/// (of kind `resource_kind`) published by `app_id`.
#[must_use]
pub fn action_node(
    app_id: &str,
    resource_kind: &str,
    resource_id: &str,
    action_id: &str,
) -> TreePath {
    TreePath::root()
        .app(app_id)
        .kind(resource_kind)
        .resource(resource_id)
        .action(action_id)
}

/// A grant to `subject` of `kinds` at `relative` in [`LOCAL_ROOM`].
fn local_grant(
    subject: policy::Subject,
    relative: &TreePath,
    kinds: policy::KindSet,
    until: Option<u64>,
) -> policy::Grant {
    let grant = policy::Grant::new(
        "local-grant",
        subject,
        TreePath::room(LOCAL_ROOM).join(relative),
        kinds,
        Answer::Allow,
        policy::Subject::person(LOCAL_USER),
    );
    match until {
        Some(at) => grant.with_lifetime(policy::Lifetime::Until(at.saturating_mul(1000))),
        None => grant,
    }
}

/// A refused permission check, with the reason people and agents read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Denial {
    /// The room's grants do not allow the action; carries the explanation.
    NotGranted(String),
    /// The sandbox does not reach the resource.
    Sandbox(CapDecision),
    /// The decision could not be made (no policy time).
    Unavailable(String),
}

/// The authority and sandbox a frame's permission checks are decided against.
/// Held by `App`, consulted by `app::dispatch_gated_event` before a gated
/// widget ever sees an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionContext {
    /// The acting person's id. Empty when no person is named.
    user: UserId,
    /// Who reaches sandboxed resources.
    actor: Actor,
    sandbox: Vec<CapOverride>,
    /// Who acts in the room and what bounds them. Shared, so a frame's
    /// copies of the context and its directory witness copy no policy.
    standing: Arc<Standing>,
    /// The instant grants and capabilities are judged at, Unix seconds, for
    /// contexts without a retained sampler. `None` skips expiry and is only
    /// for deliberately unclocked local contexts.
    now: Option<u64>,
    policy_time: Option<PolicyTimeSampler>,
    action_authority: Option<ActionAuthoritySampler>,
    /// How the host revokes a `Once` grant an admission here spent. Without
    /// one, no decision here relies on a `Once` grant.
    once_spender: Option<OnceSpender>,
    /// The `Once` grants admissions here spent, never relied on again even
    /// before the host's revocation reaches this context.
    spent: std::collections::BTreeSet<policy::GrantId>,
}

/// Inert directory cache identity. It cannot make a permission decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryPermissionWitness {
    user: UserId,
    actor: Actor,
    sandbox: Vec<CapOverride>,
    standing: Arc<Standing>,
    /// Which grants (room, then each bound's own) and sandbox capabilities
    /// were live: time matters only where it changes an answer.
    live: Vec<bool>,
    action_authority: Option<(String, Box<DirectoryPermissionWitness>)>,
}

impl PermissionContext {
    /// Decide with `standing` in the room and `sandbox` for the app's code,
    /// reached as `actor`.
    pub fn new(actor: Actor, sandbox: Vec<CapOverride>, standing: Standing) -> Self {
        Self {
            user: UserId(standing.person.clone()),
            actor,
            sandbox,
            standing: Arc::new(standing),
            now: crate::time::try_unix_now(),
            policy_time: None,
            action_authority: None,
            once_spender: None,
            spent: std::collections::BTreeSet::new(),
        }
    }

    /// The **unrestricted local user**: the owner of [`LOCAL_ROOM`] holding
    /// one explicit sandbox capability over the whole filesystem.
    ///
    /// Not a bypass flag — ownership is the room's structural rule and the
    /// sandbox reach is a real [`CapOverride`], both evaluated like any other.
    /// Right for a solo install, `--shots`, and any session the compositor
    /// has not handed a narrower one. **Wrong** for anything acting on a
    /// remote peer's behalf.
    pub fn permissive() -> Self {
        Self::new(
            Actor::person(LOCAL_USER),
            [
                Scope::FsPath("/".into()),
                Scope::Device(collab_types::caps::EVERY.into()),
                Scope::NetHost(collab_types::caps::EVERY.into()),
                Scope::FsRecord(collab_types::caps::EVERY.into()),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, scope)| {
                CapOverride::grant(Subject::person(LOCAL_USER), scope, Access::Write)
                    .with_id(format!("local-user-unrestricted-{index}"))
            })
            .collect(),
            Standing::person(policy::RoomPolicy::new(LOCAL_ROOM, LOCAL_USER), LOCAL_USER),
        )
    }

    /// `person` holding `role` in [`LOCAL_ROOM`] (owning it for
    /// [`Role::Owner`]), with no sandbox reach.
    pub fn member(person: impl Into<String>, role: Role) -> Self {
        let person = person.into();
        Self::member_of(LOCAL_ROOM, person, role)
    }

    /// `person` holding `role` in `room` (owning it for [`Role::Owner`]), with
    /// no sandbox reach.
    pub fn member_of(room: &str, person: impl Into<String>, role: Role) -> Self {
        let person = person.into();
        let policy = if role == Role::Owner {
            policy::RoomPolicy::new(room, person.clone())
        } else {
            let mut policy = policy::RoomPolicy::unowned(room, &policy::Subject::Role(Role::Owner));
            policy.members.insert(person.clone(), role);
            policy
        };
        Self::new(
            Actor::person(person.clone()).in_room(room),
            Vec::new(),
            Standing::person(policy, person),
        )
    }

    /// The permissions of a whole standing a host built (an installer
    /// bounded by a bot's grants), decided exactly as that standing says.
    #[must_use]
    pub fn from_standing(room: &str, standing: Standing) -> Self {
        Self::new(
            Actor::person(standing.person.clone()).in_room(room),
            Vec::new(),
            standing,
        )
    }

    /// `person`, who is no member of [`LOCAL_ROOM`]: nothing is allowed.
    pub fn stranger(person: impl Into<String>) -> Self {
        let person = person.into();
        Self::new(
            Actor::person(person.clone()).in_room(LOCAL_ROOM),
            Vec::new(),
            Standing::person(
                policy::RoomPolicy::unowned(LOCAL_ROOM, &policy::Subject::Role(Role::Owner)),
                person,
            ),
        )
    }

    /// `person`, no member of [`LOCAL_ROOM`], granted exactly `kinds` at
    /// `relative` (a path below the room), until Unix second `until` if given.
    pub fn granted(
        person: impl Into<String>,
        relative: &TreePath,
        kinds: policy::KindSet,
        until: Option<u64>,
    ) -> Self {
        let person = person.into();
        let grant = local_grant(
            policy::Subject::Person(person.clone()),
            relative,
            kinds,
            until,
        );
        Self::stranger(person).with_grant(grant)
    }

    /// The owner of [`LOCAL_ROOM`] acting through agent `agent`, which is
    /// granted exactly `kinds` at `relative`.
    pub fn agent_granted(agent: &str, relative: &TreePath, kinds: policy::KindSet) -> Self {
        Self::agent_granted_at(agent, std::slice::from_ref(relative), kinds)
    }

    /// The owner of [`LOCAL_ROOM`] acting through agent `agent`, which is
    /// granted exactly `kinds` at each of `nodes` (paths below the room):
    /// the scope an invitation gave it.
    pub fn agent_granted_at(agent: &str, nodes: &[TreePath], kinds: policy::KindSet) -> Self {
        let subject = policy::Subject::agent(agent);
        let grants = nodes
            .iter()
            .map(|node| local_grant(subject.clone(), node, kinds, None))
            .collect();
        Self::member(LOCAL_USER, Role::Owner).acting_as(subject, grants)
    }

    #[must_use]
    pub fn with_grant(mut self, grant: policy::Grant) -> Self {
        Arc::make_mut(&mut self.standing).policy.grants.push(grant);
        self
    }

    /// The same person acting through a delegation of `subject` carrying
    /// `grants`.
    #[must_use]
    pub fn acting_as(mut self, subject: policy::Subject, grants: Vec<policy::Grant>) -> Self {
        self.standing = Arc::new(Arc::unwrap_or_clone(self.standing).bounded_by(subject, grants));
        self
    }

    /// The same person, reading only: what a retired document or a past
    /// version offers whoever holds it.
    #[must_use]
    pub fn read_only(&self) -> Self {
        let mut context = Self::member_of(self.standing.room(), self.user.0.clone(), Role::Viewer);
        context.actor = self.actor.clone();
        context.now = self.now;
        context.policy_time.clone_from(&self.policy_time);
        context
    }

    /// Retain the actual host's fallible policy-time sampler (Unix seconds).
    /// This supplies time only, not authority. Every decision resamples;
    /// failure denies without consulting the wall clock or a numeric pin.
    /// Clones retain this same sampler.
    #[must_use]
    pub fn with_policy_time_sampler(
        mut self,
        sampler: Arc<dyn Fn() -> Result<u64, String> + Send + Sync>,
    ) -> Self {
        self.policy_time = Some(PolicyTimeSampler(sampler));
        self
    }

    /// Let an admission here rely on a `Once` grant, handing what it spent to
    /// `spend`, the host's revocation of it, as part of admitting the action.
    #[must_use]
    pub fn with_once_spender(
        mut self,
        spend: Arc<dyn Fn(&[policy::GrantId]) -> Result<(), String> + Send + Sync>,
    ) -> Self {
        self.once_spender = Some(OnceSpender(spend));
        self
    }

    /// Spend the `Once` grants an admission relied on
    /// ([`Self::invocation_verdict`]): the host revokes them, and this
    /// context never relies on them again.
    ///
    /// # Errors
    /// When the host could not revoke them: the admission must not proceed.
    pub fn spend_once(&mut self, spent: &[policy::GrantId]) -> Result<(), String> {
        if spent.is_empty() {
            return Ok(());
        }
        let spender = self
            .once_spender
            .as_ref()
            .ok_or("a grant given once cannot be spent here")?;
        (spender.0)(spent)?;
        self.spent.extend(spent.iter().cloned());
        Ok(())
    }

    /// Pin a context's decision time. An installed host sampler always takes
    /// precedence, including when it fails.
    #[must_use]
    pub fn at_unix_time(mut self, now: u64) -> Self {
        self.now = Some(now);
        self
    }

    /// This launch's permissions with its participation changed: an
    /// author's (`writable`, the room's Editor) or a viewer's, as the room's
    /// grants now say ([`crate::room_sync::RoomSync::writable`]). Only for
    /// the permissions a workspace launch made from its participation, the
    /// person a Viewer or an Editor and nothing bounding them; anything else
    /// (an owner's own app, a delegation) is `None` and stays as it is. The
    /// samplers, sandbox and spent grants carry over.
    #[must_use]
    pub fn with_participation(&self, writable: bool) -> Option<Self> {
        let person = &self.standing.person;
        let current = self.standing.policy.members.get(person).copied()?;
        if !self.standing.bounds.is_empty()
            || !matches!(current, Role::Viewer | Role::Editor)
            || self.standing.policy.members.len() != 1
        {
            return None;
        }
        let role = if writable { Role::Editor } else { Role::Viewer };
        let mut standing = (*self.standing).clone();
        standing.policy.members.insert(person.clone(), role);
        let mut next = self.clone();
        next.standing = Arc::new(standing);
        Some(next)
    }

    /// The same context with the room's policy replaced, for a host that
    /// learned of a change.
    #[must_use]
    pub fn with_standing(mut self, standing: Standing) -> Self {
        self.user = UserId(standing.person.clone());
        self.standing = Arc::new(standing);
        self
    }

    /// Compare the current policy meaning, not callback allocation or elapsed
    /// seconds. Sampling is fallible; no cached/empty witness replaces failure.
    pub(crate) fn directory_witness(&self) -> Result<DirectoryPermissionWitness, String> {
        let now = match &self.policy_time {
            Some(sampler) => Some((sampler.0)()?),
            None => self.now,
        };
        let action_authority = self
            .action_authority
            .as_ref()
            .map(|authority| {
                let current = authority.sample_for(self)?;
                Ok::<_, String>((
                    authority.app_id.clone(),
                    Box::new(current.directory_witness()?),
                ))
            })
            .transpose()?;
        let cx = policy::Context::at_secs(now.unwrap_or(0));
        let live = self
            .standing
            .policy
            .grants
            .iter()
            .chain(self.standing.bounds.iter().flat_map(|bound| &bound.grants))
            .map(|grant| grant.is_live(&cx))
            .chain(self.sandbox.iter().map(|cap| cap.expiry.is_live_at(now)))
            .collect();
        Ok(DirectoryPermissionWitness {
            user: self.user.clone(),
            actor: self.actor.clone(),
            sandbox: self.sandbox.clone(),
            standing: Arc::clone(&self.standing),
            live,
            action_authority,
        })
    }

    /// Refresh an execution check, preserving the actual host's retained clock.
    /// # Errors
    /// Refuses an unavailable clock or a failed retained host policy sampler.
    pub fn at_current_time(&self) -> Result<Self, String> {
        let now = match &self.policy_time {
            Some(sampler) => (sampler.0)()?,
            None => crate::time::try_unix_now().ok_or("execution time unavailable")?,
        };
        Ok(self.clone().at_unix_time(now))
    }

    /// The same context reaching one more sandboxed resource.
    #[must_use]
    pub fn granting(mut self, cap: CapOverride) -> Self {
        self.sandbox.push(cap);
        self
    }

    /// The acting person's id.
    pub fn user(&self) -> &UserId {
        &self.user
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// The role the person holds in the room, if any.
    pub fn role(&self) -> Option<Role> {
        self.standing.policy.active_role(&self.standing.person)
    }

    /// The standing actions are decided against.
    pub fn standing(&self) -> &Standing {
        &self.standing
    }

    /// The sandbox capabilities in force.
    pub fn capabilities(&self) -> &[CapOverride] {
        &self.sandbox
    }

    fn policy_now(&self) -> Result<Option<u64>, String> {
        match &self.policy_time {
            Some(sampler) => (sampler.0)().map(Some),
            None => Ok(self.now),
        }
    }

    /// The answer for an action of `kind` at `node` below the room.
    pub fn answer(&self, node: &TreePath, kind: ActionKind) -> Answer {
        self.verdict_for(node, kind, false, false, |verdict| verdict.answer)
            .unwrap_or(Answer::Deny)
    }

    /// What `read` takes from the verdict for an action of `kind` at `node`:
    /// of a privileged action when `privileged` ([`policy::Verdict::privileged`]),
    /// relying on `Once` grants only when `once` (an admission that spends
    /// them) and this context can spend them.
    fn verdict_for<R>(
        &self,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        once: bool,
        read: impl FnOnce(&policy::Verdict<'_>) -> R,
    ) -> Result<R, String> {
        let now = self.policy_now()?;
        let cx = policy::Context::at_secs(now.unwrap_or(0));
        let sampled;
        let held = match &self.action_authority {
            Some(authority) if authority.matches(node) => {
                sampled = authority.sample_for(self)?.standing;
                &sampled
            }
            _ => &self.standing,
        };
        let standing = self.usable(held, once);
        let mut verdict = standing.verdict(&standing.node(node), kind, &cx);
        if privileged {
            verdict = verdict.privileged();
        }
        Ok(read(&verdict))
    }

    /// `held` without the `Once` grants this decision may not rely on: all of
    /// them unless `once` (an admission that spends them) and this context
    /// can spend them, and those it already spent. Most standings hold none
    /// to leave out and are decided against as they are; one is copied only
    /// to drop some.
    fn usable<'s>(&self, held: &'s Standing, once: bool) -> std::borrow::Cow<'s, Standing> {
        let usable = once && self.once_spender.is_some();
        let keep = |grant: &policy::Grant| {
            grant.lifetime != policy::Lifetime::Once || (usable && !self.spent.contains(&grant.id))
        };
        let all_kept = held
            .policy
            .grants
            .iter()
            .chain(held.bounds.iter().flat_map(|bound| &bound.grants))
            .all(keep);
        if all_kept {
            return std::borrow::Cow::Borrowed(held);
        }
        let mut standing = held.clone();
        standing.policy.grants.retain(keep);
        for bound in &mut standing.bounds {
            bound.grants.retain(keep);
        }
        std::borrow::Cow::Owned(standing)
    }

    /// Whether the scope is permitted: an action that is allowed or asks
    /// first, a sandboxed resource the app reaches. Fail-closed.
    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.check_with_denied_feedback(scope).is_none()
    }

    /// [`PermissionContext::check`] with the reason. `None` means permitted.
    pub fn check_with_denied_feedback(&self, scope: &PermissionScope) -> Option<Denial> {
        match scope {
            PermissionScope::Action { node, kind } => {
                // Explained only when refused: an allowed check needs no sentence.
                match self.verdict_for(node, *kind, false, false, |verdict| {
                    (verdict.answer == Answer::Deny).then(|| verdict.explain())
                }) {
                    Ok(Some(why)) => Some(Denial::NotGranted(why)),
                    Ok(None) => None,
                    Err(error) => Some(Denial::Unavailable(error)),
                }
            }
            PermissionScope::Sandbox { resource, access } => {
                let now = match self.policy_now() {
                    Ok(now) => now,
                    Err(error) => return Some(Denial::Unavailable(error)),
                };
                match decide_capability(&self.actor, resource, *access, &self.sandbox, now) {
                    CapDecision::Allow => None,
                    denied => Some(Denial::Sandbox(denied)),
                }
            }
        }
    }

    /// The answer for performing `action` on `resource` (of kind
    /// `resource_kind`), published by the app `app_id`.
    pub fn action_answer(
        &self,
        app_id: &str,
        resource_kind: &str,
        resource_id: &str,
        action: &tilo_actions::ActionSpec,
    ) -> Answer {
        self.need_answer(
            app_id,
            resource_kind,
            resource_id,
            &action.id,
            &action.contract.need(),
        )
    }

    fn need_answer(
        &self,
        app_id: &str,
        resource_kind: &str,
        resource_id: &str,
        action_id: &str,
        need: &tilo_actions::ActionNeed,
    ) -> Answer {
        if self.action_authority.is_none() {
            return self
                .published_answers(|answer| {
                    let mut node = answer.resource(app_id, resource_kind, resource_id);
                    answer.action(&mut node, need.field.as_deref(), action_id, need)
                })
                .unwrap_or(Answer::Deny);
        }
        let node = tilo_actions::action_node(
            &TreePath::root(),
            app_id,
            resource_kind,
            resource_id,
            need.field.as_deref(),
            action_id,
        );
        self.verdict_for(&node, need.kind, need.privileged, false, |verdict| {
            verdict.answer
        })
        .unwrap_or(Answer::Deny)
    }

    /// The answer for admitting one invocation of `action` on `resource`
    /// with these arguments, and the `Once` grants admitting it spends: the
    /// door that runs it hands them to [`Self::spend_once`] first.
    pub fn invocation_verdict(
        &self,
        app_id: &str,
        resource: &tilo_actions::Resource,
        action: &tilo_actions::ActionSpec,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> (Answer, Vec<policy::GrantId>) {
        let need = action.need_for(json, argument);
        let node = tilo_actions::action_node(
            &TreePath::root(),
            app_id,
            &resource.kind,
            resource.id.as_str(),
            need.field.as_deref(),
            &action.id,
        );
        self.verdict_for(&node, need.kind, need.privileged, true, |verdict| {
            (verdict.answer, verdict.spent())
        })
        .unwrap_or((Answer::Deny, Vec::new()))
    }

    /// The answer for invoking `action` on `resource` with these arguments.
    /// An Ask runs only once the person confirmed it in the host.
    pub fn invocation_answer(
        &self,
        app_id: &str,
        resource: &tilo_actions::Resource,
        action: &tilo_actions::ActionSpec,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Answer {
        self.need_answer(
            app_id,
            &resource.kind,
            resource.id.as_str(),
            &action.id,
            &action.need_for(json, argument),
        )
    }

    /// May the actor perform `action` on `resource` (of kind
    /// `resource_kind`), published by `app_id`? Allowed, or asking first.
    pub fn permits_action(
        &self,
        app_id: &str,
        resource_kind: &str,
        resource_id: &str,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        self.action_answer(app_id, resource_kind, resource_id, action) != Answer::Deny
    }

    /// `resource`, published by the app `app_id`, as the actor may reach
    /// it: the actions it may perform (allowed or asking first), and the
    /// nested resources that keep any. Denied is absent, never merely
    /// marked. `None` when nothing is left.
    pub fn permitted_resource(
        &self,
        app_id: &str,
        resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        // A host action authority decides some nodes from a standing it
        // samples per action: ask action by action.
        if self.action_authority.is_some() {
            return keep_permitted(resource, &|kind, id, action| {
                self.permits_action(app_id, kind, id, action)
            });
        }
        // Otherwise one authority decides every action of the tree. An
        // unavailable clock refuses them all, as each verdict would.
        self.published_answers(|answers| answers.keep_permitted(app_id, resource))
            .ok()
            .flatten()
    }

    /// Answers whether each of many actions of `app_id` is permitted, as
    /// [`Self::permits_action`] does.
    pub fn action_decider<'a>(&'a self, app_id: &'a str) -> ActionDecider<'a> {
        let answers = match (&self.action_authority, self.usable(&self.standing, false)) {
            (None, std::borrow::Cow::Borrowed(standing)) => {
                self.policy_now().ok().map(|now| PublishedAnswers {
                    authority: standing.authority(),
                    room: standing.policy.node(),
                    cx: policy::Context::at_secs(now.unwrap_or(0)),
                })
            }
            // A host action authority, a standing to filter, or no clock:
            // asked action by action.
            _ => None,
        };
        ActionDecider {
            permissions: self,
            app_id,
            answers,
            resource: std::cell::RefCell::new(None),
        }
    }

    /// `decide` with one authority for the standing as it is, for answers
    /// about published actions (no host action authority, no `Once` grant
    /// spent). An unavailable clock is an error.
    fn published_answers<R>(
        &self,
        decide: impl FnOnce(&PublishedAnswers<'_>) -> R,
    ) -> Result<R, String> {
        let now = self.policy_now()?;
        let standing = self.usable(&self.standing, false);
        let answers = PublishedAnswers {
            authority: standing.authority(),
            room: standing.policy.node(),
            cx: policy::Context::at_secs(now.unwrap_or(0)),
        };
        Ok(decide(&answers))
    }
}

/// Answers [`PermissionContext::permits_action`] for many actions of one app:
/// one authority for all of them, and the node of the resource last asked
/// about kept for the next action on it.
pub struct ActionDecider<'a> {
    permissions: &'a PermissionContext,
    app_id: &'a str,
    answers: Option<PublishedAnswers<'a>>,
    resource: std::cell::RefCell<Option<(String, String, TreePath)>>,
}

impl ActionDecider<'_> {
    /// [`PermissionContext::permits_action`] for `action` on the resource
    /// `resource_id` of kind `resource_kind`.
    pub fn permits(
        &self,
        resource_kind: &str,
        resource_id: &str,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        let Some(answers) = &self.answers else {
            return self.permissions.permits_action(
                self.app_id,
                resource_kind,
                resource_id,
                action,
            );
        };
        let mut held = self.resource.borrow_mut();
        let node = match &mut *held {
            Some((kind, id, node)) if kind == resource_kind && id == resource_id => node,
            slot => {
                let node = answers.resource(self.app_id, resource_kind, resource_id);
                &mut slot
                    .insert((resource_kind.to_owned(), resource_id.to_owned(), node))
                    .2
            }
        };
        let need = action.contract.need();
        answers.action(node, need.field.as_deref(), &action.id, &need) != Answer::Deny
    }
}

/// One authority's answers for the actions a room's apps publish.
struct PublishedAnswers<'s> {
    authority: policy::Authority<'s>,
    room: TreePath,
    cx: policy::Context<'static>,
}

impl PublishedAnswers<'_> {
    /// The node of a published resource in this room.
    fn resource(&self, app_id: &str, kind: &str, id: &str) -> TreePath {
        self.room.clone().app(app_id).kind(kind).resource(id)
    }

    /// The answer for `action` (of `need`) on the resource `node` names, as
    /// [`tilo_actions::action_node`] places it; `node` is left as it was.
    fn action(
        &self,
        node: &mut TreePath,
        field: Option<&str>,
        action: &str,
        need: &tilo_actions::ActionNeed,
    ) -> Answer {
        let resource = node.len();
        for part in field
            .unwrap_or_default()
            .split('.')
            .filter(|part| !part.is_empty())
        {
            node.push_name(policy::Level::Field, part);
        }
        node.push_name(policy::Level::Action, action);
        let answer = self
            .authority
            .answer(node, need.kind, need.privileged, &self.cx);
        node.truncate(resource);
        answer
    }

    /// [`PermissionContext::permitted_resource`] with these answers.
    fn keep_permitted(
        &self,
        app_id: &str,
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        let mut node = self.resource(app_id, &resource.kind, resource.id.as_str());
        resource.retain_actions(|action| {
            let need = action.contract.need();
            self.action(&mut node, need.field.as_deref(), &action.id, &need) != Answer::Deny
        });
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.keep_permitted(app_id, child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }
}

/// `resource` with only the actions `permits` keeps (by resource kind, id
/// and action) and the nested resources that keep any; `None` when nothing
/// is left.
fn keep_permitted(
    mut resource: tilo_actions::Resource,
    permits: &dyn Fn(&str, &str, &tilo_actions::ActionSpec) -> bool,
) -> Option<tilo_actions::Resource> {
    let id = resource.id.as_str().to_owned();
    let kind = resource.kind.clone();
    resource.retain_actions(|action| permits(&kind, &id, action));
    resource.children = resource
        .children
        .into_iter()
        .filter_map(|child| keep_permitted(child, permits))
        .collect();
    (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
}

/// A participant's permissions as they stand *now*, following every change the
/// host makes while the app runs: a role changed by the record's owner, a grant
/// revoked, a capability granted. Read-only: only the host changes them.
///
/// A [`PermissionContext`] is a snapshot; code that decides once at launch
/// what a participant may do goes stale the moment their role changes. Hold
/// one of these instead and ask again when it matters (every frame is cheap).
#[derive(Clone)]
pub struct LivePermissions(std::rc::Rc<std::cell::RefCell<PermissionContext>>);

impl LivePermissions {
    /// Follow the context the host shares with its components.
    pub(crate) fn shared(context: std::rc::Rc<std::cell::RefCell<PermissionContext>>) -> Self {
        Self(context)
    }

    /// Permissions that never change: a preview of a past version, a capture,
    /// a test fixture.
    pub fn fixed(context: PermissionContext) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(context)))
    }

    /// May the participant do this, as of now?
    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.0.borrow().check(scope)
    }

    /// The answer for an action of `kind` at `node` below the room, as of now.
    pub fn answer(&self, node: &TreePath, kind: ActionKind) -> Answer {
        self.0.borrow().answer(node, kind)
    }

    /// The permissions in force now.
    pub fn current(&self) -> PermissionContext {
        self.0.borrow().clone()
    }
}

impl std::fmt::Debug for LivePermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LivePermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}

mod action_authority;
pub mod capability;
use action_authority::ActionAuthoritySampler;

/// The shared admission ([`tilo_action_policy::admit`]) as a window takes
/// it for its own person, the decision a broker takes for a caller: an ask
/// is theirs to confirm in the window when they act themselves (`direct`),
/// and nobody else's here. `confirmed` is their confirmation, or the broker
/// having admitted it.
///
/// # Errors
/// Refused, or [`tilo_actions::DoorError::ConfirmationRequired`] until
/// the person confirmed an ask.
pub fn admit_in_window(
    answer: tilo_action_policy::Answer,
    action: &tilo_actions::ActionSpec,
    direct: bool,
    confirmed: bool,
) -> Result<(), tilo_actions::DoorError> {
    let admitted = tilo_action_policy::admit(
        tilo_action_policy::Invoked {
            answer,
            attended: action.contract.is_attended(),
            record: None,
        },
        tilo_action_policy::Asker {
            reachable: direct,
            // A window has no session delegate: an agent acting in it holds a
            // bounded standing (`direct` false), so its asks already refuse.
            delegated: false,
        },
    );
    match admitted {
        Err(_) => Err(crate::bound_dispatch::denied(action)),
        Ok(tilo_action_policy::Admission::AsksFirst) if !confirmed => {
            Err(tilo_actions::DoorError::ConfirmationRequired)
        }
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod policy_time_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(subject: policy::Subject, node: TreePath, kinds: policy::KindSet) -> policy::Grant {
        policy::Grant::new(
            "grant",
            subject,
            node,
            kinds,
            Answer::Allow,
            policy::Subject::person("owner"),
        )
    }

    #[test]
    fn grants_and_capabilities_reach_exactly_their_scope_while_live() {
        // phase: permissive_is_a_capability_not_a_bypass
        // The permissive context reaches the filesystem *because of a
        // capability*, not a special case in the checker: drop it and the same
        // owner is denied.
        {
            let ctx = PermissionContext::permissive();
            assert!(ctx.check(&PermissionScope::path(
                "/var/workspace/secret.png",
                Access::Read
            )));
            assert_eq!(ctx.capabilities().len(), 4);
            assert_eq!(ctx.capabilities()[0].access, Access::Write);
            assert_eq!(ctx.capabilities()[0].scope, Scope::FsPath("/".into()));
            assert!(ctx.check(&PermissionScope::app("notes", ActionKind::Outside)));

            // The same owner, no capability: a room role confers no reach over
            // paths.
            let stripped = PermissionContext::member(LOCAL_USER, Role::Owner);
            assert!(!stripped.check(&PermissionScope::path(
                "/var/workspace/secret.png",
                Access::Read
            )));
            assert!(stripped.check(&PermissionScope::app("notes", ActionKind::Outside)));
        }
        // phase: action_grant_is_narrow_while_app_grant_covers_every_action
        {
            let agent = |grants: Vec<policy::Grant>| {
                let owner = PermissionContext::member("owner", Role::Owner);
                owner.clone().with_standing(
                    owner
                        .standing()
                        .clone()
                        .bounded_by(policy::Subject::agent("scribe"), grants),
                )
            };
            let room = TreePath::room(LOCAL_ROOM);
            let open = |page: &str, action: &str| {
                PermissionScope::action("notes", "notes.page", page, action, ActionKind::Read)
            };
            let narrow = agent(vec![grant(
                policy::Subject::agent("scribe"),
                room.join(&action_node(
                    "notes",
                    "notes.page",
                    "notes.page:1",
                    "note.open",
                )),
                policy::KindSet::READ,
            )]);
            assert!(narrow.check(&open("notes.page:1", "note.open")));
            assert!(!narrow.check(&open("notes.page:1", "note.delete")));
            assert!(
                !narrow.check(&open("notes.page:2", "note.open")),
                "the same action on another page is not granted"
            );

            let broad = agent(vec![grant(
                policy::Subject::agent("scribe"),
                room.clone().app("notes"),
                policy::KindSet::READ,
            )]);
            assert!(broad.check(&open("notes.page:2", "note.delete")));
            assert!(
                !broad.check(&PermissionScope::app("notes", ActionKind::Undoable)),
                "a read grant is not a change"
            );
        }
        // phase: lapsed_capability_no_longer_grants
        // NEGATIVE: a capability that has lapsed is not a capability.
        {
            let cap = CapOverride::grant(
                Subject::person("u_guest"),
                Scope::FsPath("/var/workspace/shared".into()),
                Access::Write,
            )
            .with_expiry(1_000);
            let scope = PermissionScope::path("/var/workspace/shared/notes.md", Access::Write);
            let context =
                || PermissionContext::member("u_guest", Role::Viewer).granting(cap.clone());
            assert!(
                context().at_unix_time(999).check(&scope),
                "before the deadline"
            );
            let lapsed = context().at_unix_time(1_001);
            assert!(
                !lapsed.check(&scope),
                "after the deadline the grant is gone"
            );
            assert_eq!(
                lapsed.check_with_denied_feedback(&scope),
                Some(Denial::Sandbox(CapDecision::NotGranted)),
                "an expired capability is invisible"
            );
        }
    }

    #[test]
    fn action_checks_keep_resource_identity_and_the_contract_kind() {
        use tilo_actions::{ActionSpec, OperationKind};

        let action =
            ActionSpec::operation("edit", "Edit", OperationKind::Effect, "test.edit", &()).unwrap();
        assert_eq!(action.contract.kind, ActionKind::Outside);
        let owner = PermissionContext::member("owner", Role::Owner);
        let node = TreePath::room(LOCAL_ROOM).join(&action_node(
            "notes",
            "notes.page",
            "notes.page:1",
            "edit",
        ));
        let context = |kinds| {
            owner
                .clone()
                .with_standing(owner.standing().clone().bounded_by(
                    policy::Subject::agent("scribe"),
                    vec![grant(policy::Subject::agent("scribe"), node.clone(), kinds)],
                ))
        };
        let outside = context(policy::KindSet::OUTSIDE);
        assert!(outside.permits_action("notes", "notes.page", "notes.page:1", &action));
        assert!(!outside.permits_action("notes", "notes.page", "notes.page:2", &action));
        assert!(!outside.permits_action("other", "notes.page", "notes.page:1", &action));
        assert!(
            !context(policy::KindSet::READ_UNDOABLE).permits_action(
                "notes",
                "notes.page",
                "notes.page:1",
                &action
            ),
            "an outside action is not covered by an undoable grant"
        );
    }

    #[test]
    fn denials_are_default_and_explained() {
        // phase: viewer_is_denied_changes
        {
            let ctx = PermissionContext::member("u_guest", Role::Viewer);
            let write = PermissionScope::app("notes", ActionKind::Undoable);
            assert!(!ctx.check(&write), "a Viewer must not change the room");
            assert!(matches!(
                ctx.check_with_denied_feedback(&write),
                Some(Denial::NotGranted(_))
            ));
            assert!(ctx.check(&PermissionScope::app("notes", ActionKind::Read)));
        }
        // phase: a_restriction_denies_inside_its_node
        {
            let mut ctx = PermissionContext::member("u_editor", Role::Editor);
            let mut standing = ctx.standing().clone();
            standing.policy.grants.push(policy::Grant::new(
                "terminal-read-only",
                policy::Subject::Role(Role::Editor),
                TreePath::room(LOCAL_ROOM).app(AppKind::Terminal.app_id()),
                policy::KindSet::UNDOABLE,
                Answer::Deny,
                policy::Subject::person("owner"),
            ));
            ctx = ctx.with_standing(standing);
            assert!(!ctx.check(&PermissionScope::app_kind(
                AppKind::Terminal,
                ActionKind::Undoable
            )));
            assert!(ctx.check(&PermissionScope::app_kind(
                AppKind::FileExplorer,
                ActionKind::Undoable
            )));
        }
        // phase: a_delegation_without_grants_is_denied_everything
        {
            let owner = PermissionContext::member("owner", Role::Owner);
            let app = owner.clone().with_standing(
                owner
                    .standing()
                    .clone()
                    .bounded_by(policy::Subject::app("dev.example.thing"), Vec::new()),
            );
            assert!(!app.check(&PermissionScope::workspace(ActionKind::Read)));
            assert!(!app.check(&PermissionScope::app_kind(
                AppKind::FileExplorer,
                ActionKind::Read
            )));
            assert!(!app.check(&PermissionScope::path("/etc/shadow", Access::Read)));
        }
    }
}
