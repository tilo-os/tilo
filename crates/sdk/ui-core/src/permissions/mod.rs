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
///
/// This is an *identity*, not a policy: it names an app so a presence
/// cursor can be scoped to it. The stable string [`AppKind::app_id`]
/// returns is the same app id the compositor's portal and a
/// `Principal::App` subject use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}

impl AppKind {
    /// The stable app id.
    pub fn app_id(self) -> &'static str {
        match self {
            AppKind::Terminal => "terminal",
            AppKind::FileExplorer => "file-explorer",
            AppKind::Browser => "browser",
            AppKind::Settings => "settings",
            AppKind::Unknown => "unknown",
        }
    }
}

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

    /// An action of `kind` anywhere in the app `app_id`.
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
    /// Who acts in the room and what bounds them.
    standing: Standing,
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
    standing: Standing,
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
            standing,
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

    /// The same context with one more grant in the room's policy.
    #[must_use]
    pub fn with_grant(mut self, grant: policy::Grant) -> Self {
        self.standing.policy.grants.push(grant);
        self
    }

    /// The same person acting through a delegation of `subject` carrying
    /// `grants`.
    #[must_use]
    pub fn acting_as(mut self, subject: policy::Subject, grants: Vec<policy::Grant>) -> Self {
        self.standing = self.standing.bounded_by(subject, grants);
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

    /// The same context with the room's policy replaced, for a host that
    /// learned of a change.
    #[must_use]
    pub fn with_standing(mut self, standing: Standing) -> Self {
        self.user = UserId(standing.person.clone());
        self.standing = standing;
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
            standing: self.standing.clone(),
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
        self.verdict(node, kind)
            .map_or(Answer::Deny, |(answer, _)| answer)
    }

    fn verdict(&self, node: &TreePath, kind: ActionKind) -> Result<(Answer, String), String> {
        self.verdict_for(node, kind, false, false)
            .map(|(answer, why, _)| (answer, why))
    }

    /// [`Self::verdict`], of a privileged action when `privileged`
    /// ([`policy::Verdict::privileged`]), relying on `Once` grants only when
    /// `once` (an admission that spends them) and this context can spend
    /// them; with the ones it relied on.
    fn verdict_for(
        &self,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        once: bool,
    ) -> Result<(Answer, String, Vec<policy::GrantId>), String> {
        let now = self.policy_now()?;
        let cx = policy::Context::at_secs(now.unwrap_or(0));
        let mut standing = match &self.action_authority {
            Some(authority) if authority.matches(node) => authority.sample_for(self)?.standing,
            _ => self.standing.clone(),
        };
        let usable = once && self.once_spender.is_some();
        let keep = |grant: &policy::Grant| {
            grant.lifetime != policy::Lifetime::Once || (usable && !self.spent.contains(&grant.id))
        };
        standing.policy.grants.retain(keep);
        for bound in &mut standing.bounds {
            bound.grants.retain(keep);
        }
        let mut verdict = standing.verdict(&standing.node(node), kind, &cx);
        if privileged {
            verdict = verdict.privileged();
        }
        Ok((verdict.answer, verdict.explain(), verdict.spent()))
    }

    /// Whether the scope is permitted: an action that is allowed or asks
    /// first, a sandboxed resource the app reaches. Fail-closed.
    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.check_with_denied_feedback(scope).is_none()
    }

    /// [`PermissionContext::check`] with the reason. `None` means permitted.
    pub fn check_with_denied_feedback(&self, scope: &PermissionScope) -> Option<Denial> {
        match scope {
            PermissionScope::Action { node, kind } => match self.verdict(node, *kind) {
                Ok((Answer::Deny, why)) => Some(Denial::NotGranted(why)),
                Ok(_) => None,
                Err(error) => Some(Denial::Unavailable(error)),
            },
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
        let node = tilo_actions::action_node(
            &TreePath::root(),
            app_id,
            resource_kind,
            resource_id,
            need.field.as_deref(),
            action_id,
        );
        self.verdict_for(&node, need.kind, need.privileged, false)
            .map_or(Answer::Deny, |(answer, ..)| answer)
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
        self.verdict_for(&node, need.kind, need.privileged, true)
            .map_or((Answer::Deny, Vec::new()), |(answer, _, spent)| {
                (answer, spent)
            })
    }

    /// May the actor take this one invocation of `action` on `resource`,
    /// carrying `json` (or the prompted `argument`)? Allowed; or asking
    /// first when the person acts themselves, their own gesture being the
    /// answer. Someone acting for them does not get past an ask here.
    pub fn permits_invocation(
        &self,
        app_id: &str,
        resource: &tilo_actions::Resource,
        action: &tilo_actions::ActionSpec,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> bool {
        self.invocation_answer(app_id, resource, action, json, argument)
            .admits(self.standing.bounds.is_empty())
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
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        let resource_id = resource.id.as_str().to_owned();
        let kind = resource.kind.clone();
        resource
            .actions
            .retain(|action| self.permits_action(app_id, &kind, &resource_id, action));
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.permitted_resource(app_id, child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }
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
