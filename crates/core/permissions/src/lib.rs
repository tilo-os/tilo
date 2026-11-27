//! The toolkit's permission check.
//!
//! Two questions, each answered by the one model that owns it:
//!
//! - **May this person (or their agent) perform this action?** Decided by
//!   `tilo-action-policy` over the workspace's grants: a [`MemberAccess`] names the
//!   person, the workspace's policy and any delegation bounding them, and
//!   [`PermissionScope::Action`] names a node below the workspace and the action's
//!   kind.
//! - **May this app's code reach this path, device or host?** Decided by the
//!   sandbox capabilities of [`caps`]
//!   ([`PermissionScope::Sandbox`]).
//!
//! The host installs both via [`PermissionContext::new`]; nothing here decides
//! anything itself.

use std::sync::Arc;

pub use tilo_action_policy::Grantee;
pub use tilo_member_types::{Acting, MemberId};

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

/// The host's revocation of the grants an admission spent.
type SpendGrants = Arc<dyn Fn(&[policy::GrantId]) -> Result<(), String> + Send + Sync>;

/// The host's revocation of the `Once` grants an in-app admission spent.
#[derive(Clone)]
struct OnceSpender(SpendGrants);

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

pub mod caps;

pub use caps::{Access, CapDecision, CapOverride, Effect, ResourceRef, Scope, decide_capability};
pub use tilo_action_policy as policy;
pub use tilo_action_policy::Role;
pub use tilo_action_policy::{Answer, MemberAccess, TreePath};
pub use tilo_action_types::{ActionKind, KindSet};

/// The id the local, unrestricted user acts as under
/// [`PermissionContext::permissive`].
pub const LOCAL_USER: &str = "local";

/// The workspace [`PermissionContext::permissive`] and [`PermissionContext::member`]
/// decide in, when no host names one.
pub const LOCAL_WORKSPACE: &str = "local";

/// Which system app a presence key or a permission scope refers to.
pub use tilo_presence::AppKind;

/// What a widget or a launch asks permission for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionScope {
    /// An action of `kind` at `node`, a path below the workspace (beginning at the
    /// app, or empty for the workspace itself).
    Action { node: TreePath, kind: ActionKind },
    /// Reaching a sandboxed resource (a path, a device, a host) with `access`.
    Sandbox {
        resource: ResourceRef,
        access: Access,
    },
}

impl PermissionScope {
    /// An action of `kind` on the workspace itself.
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

    /// One of the workspace's governance actions (inviting, granting, settings).
    pub fn governance(action: &str) -> Self {
        Self::Action {
            node: TreePath::root()
                .kind(policy::roles::WORKSPACE_KIND)
                .action(action),
            kind: policy::roles::GOVERNANCE_KIND,
        }
    }

    pub fn app_kind(app: AppKind, kind: ActionKind) -> Self {
        Self::app(app.app_id(), kind)
    }

    /// Reaching a filesystem path. Workspace roles carry no authority over the
    /// filesystem, so this always needs an explicit sandbox capability.
    pub fn path(path: impl Into<String>, access: Access) -> Self {
        Self::Sandbox {
            resource: ResourceRef::FsPath(path.into()),
            access,
        }
    }
}

/// The node below the workspace of action `action_id` on resource `resource_id`
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

/// A grant to `subject` of `kinds` at `relative` in [`LOCAL_WORKSPACE`].
fn local_grant(
    subject: policy::Grantee,
    relative: &TreePath,
    kinds: tilo_action_types::KindSet,
    until: Option<u64>,
) -> policy::Grant {
    let grant = policy::Grant::new(
        "local-grant",
        subject,
        TreePath::workspace(LOCAL_WORKSPACE).join(relative),
        kinds,
        Answer::Allow,
        policy::Grantee::person(LOCAL_USER),
    );
    match until {
        Some(at) => {
            grant.with_lifetime(tilo_action_types::Lifetime::Until(at.saturating_mul(1000)))
        }
        None => grant,
    }
}

/// A refused permission check, with the reason people and agents read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Denial {
    /// The workspace's grants do not allow the action; carries the explanation.
    NotGranted(String),
    /// The sandbox does not reach the resource.
    Sandbox(CapDecision),
    /// The decision could not be made (no policy time).
    Unavailable(String),
}

/// The authority and sandbox a frame's permission checks are decided against.
/// Held by `App`, consulted by `app::dispatch_guarded_event` before a guarded
/// widget ever sees an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionContext {
    /// The acting person's id. Empty when no person is named.
    user: MemberId,
    /// Who reaches sandboxed resources.
    acting: Acting,
    sandbox: Vec<CapOverride>,
    /// Who acts in the workspace and what bounds them. Shared, so a frame's
    /// copies of the context and its directory observation copy no policy.
    access: Arc<MemberAccess>,
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
    /// Whether the host running this context's actions reaches the person
    /// to ask them, when someone acts for them here: the host running a
    /// hosted workspace's records puts an ask to the person on their own
    /// devices ([`Self::reaching_person`]).
    reaches_person: bool,
    /// What the host said of the actions this window shows ([`shown`]): decided there, and
    /// only shown here.
    shown: Arc<ShownVerdicts>,
}

/// Inert directory cache identity. It cannot make a permission decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryPermissionObservation {
    user: MemberId,
    acting: Acting,
    sandbox: Vec<CapOverride>,
    access: Arc<MemberAccess>,
    /// Which grants (workspace, then each bound's own) and sandbox capabilities
    /// were live: time matters only where it changes an answer.
    live: Vec<bool>,
    action_authority: Option<(String, Box<DirectoryPermissionObservation>)>,
    shown: Arc<ShownVerdicts>,
}

impl PermissionContext {
    /// Decide with `access` in the workspace and `sandbox` for the app's code,
    /// reached as `actor`.
    pub fn new(acting: Acting, sandbox: Vec<CapOverride>, access: MemberAccess) -> Self {
        Self {
            user: MemberId::person(access.person_id().unwrap_or_default()),
            acting,
            sandbox,
            access: Arc::new(access),
            now: tilo_clock::try_unix_now(),
            policy_time: None,
            action_authority: None,
            once_spender: None,
            spent: std::collections::BTreeSet::new(),
            reaches_person: false,
            shown: Arc::default(),
        }
    }

    /// The same context, run by a host that puts an ask to the person on
    /// their own devices when someone acts for them: such an invocation asks
    /// first ([`tilo_actions::ApiError::ConfirmationRequired`]) where it
    /// would otherwise be refused, as the shell's broker asks.
    #[must_use]
    pub fn reaching_person(mut self) -> Self {
        self.reaches_person = true;
        self
    }

    /// Who an invocation admitted here asks ([`tilo_action_policy::MemberAccess::asker`]):
    /// the person, who answers themselves, or whom the host reaches; and
    /// whether someone acts for them.
    pub fn asker(&self) -> policy::Asker {
        self.access.asker(self.reaches_person)
    }

    /// The **unrestricted local user**: the owner of [`LOCAL_WORKSPACE`] holding
    /// one explicit sandbox capability over the whole filesystem.
    ///
    /// Not a bypass flag — ownership is the workspace's structural rule and the
    /// sandbox reach is a real [`CapOverride`], both evaluated like any other.
    /// Right for a solo install, `--shots`, and any session the compositor
    /// has not handed a narrower one. **Wrong** for anything acting on a
    /// remote peer's behalf.
    pub fn permissive() -> Self {
        Self::new(
            Acting::person(LOCAL_USER),
            [
                Scope::FsPath("/".into()),
                Scope::NetHost(crate::caps::EVERY.into()),
                Scope::FsResource(crate::caps::EVERY.into()),
            ]
            .into_iter()
            .chain(crate::caps::DeviceCap::ALL.map(Scope::Device))
            .enumerate()
            .map(|(index, scope)| {
                CapOverride::grant(Grantee::person(LOCAL_USER), scope, Access::Write)
                    .with_id(format!("local-user-unrestricted-{index}"))
            })
            .collect(),
            MemberAccess::person(
                policy::WorkspacePolicy::new(LOCAL_WORKSPACE, LOCAL_USER),
                LOCAL_USER,
            ),
        )
    }

    /// The person `person` holding `role` in [`LOCAL_WORKSPACE`] (owning it
    /// for [`Role::Owner`]), with no sandbox reach.
    pub fn member(person: &MemberId, role: Role) -> Self {
        Self::member_of(LOCAL_WORKSPACE, person, role)
    }

    /// The person `person` holding `role` in `workspace` (owning it for
    /// [`Role::Owner`]), with no sandbox reach.
    pub fn member_of(workspace: &str, person: &MemberId, role: Role) -> Self {
        let person = person.id().to_owned();
        let policy = if role == Role::Owner {
            policy::WorkspacePolicy::new(workspace, person.clone())
        } else {
            let mut policy =
                policy::WorkspacePolicy::unowned(workspace, &policy::Grantee::role(Role::Owner));
            policy.members.insert(person.clone(), role);
            policy
        };
        Self::new(
            Acting::person(person.clone()),
            Vec::new(),
            MemberAccess::person(policy, person),
        )
    }

    /// The permissions of a whole member access a host built (an installer
    /// bounded by a bot's grants), decided exactly as that access says.
    #[must_use]
    pub fn from_access(access: MemberAccess) -> Self {
        Self::new(
            Acting::person(access.person_id().unwrap_or_default()),
            Vec::new(),
            access,
        )
    }

    /// The person `person`, who is no member of [`LOCAL_WORKSPACE`]: nothing
    /// is allowed.
    pub fn stranger(person: &MemberId) -> Self {
        let person = person.id().to_owned();
        Self::new(
            Acting::person(person.clone()),
            Vec::new(),
            MemberAccess::person(
                policy::WorkspacePolicy::unowned(
                    LOCAL_WORKSPACE,
                    &policy::Grantee::role(Role::Owner),
                ),
                person,
            ),
        )
    }

    /// The person `person`, no member of [`LOCAL_WORKSPACE`], granted exactly
    /// `kinds` at `relative` (a path below the workspace), until Unix second
    /// `until` if given.
    pub fn granted(
        person: &MemberId,
        relative: &TreePath,
        kinds: tilo_action_types::KindSet,
        until: Option<u64>,
    ) -> Self {
        let grant = local_grant(
            policy::Grantee::Member(person.clone()),
            relative,
            kinds,
            until,
        );
        Self::stranger(person).with_grant(grant)
    }

    /// The owner of [`LOCAL_WORKSPACE`] acting through agent `agent`, which is
    /// granted exactly `kinds` at `relative`.
    pub fn agent_granted(
        agent: &str,
        relative: &TreePath,
        kinds: tilo_action_types::KindSet,
    ) -> Self {
        Self::agent_granted_at(agent, std::slice::from_ref(relative), kinds)
    }

    /// The owner of [`LOCAL_WORKSPACE`] acting through agent `agent`, which is
    /// granted exactly `kinds` at each of `nodes` (paths below the workspace):
    /// the scope an invitation gave it.
    pub fn agent_granted_at(
        agent: &str,
        nodes: &[TreePath],
        kinds: tilo_action_types::KindSet,
    ) -> Self {
        let subject = policy::Grantee::agent(agent, LOCAL_USER);
        let grants = nodes
            .iter()
            .map(|node| local_grant(subject.clone(), node, kinds, None))
            .collect();
        Self::member(&MemberId::person(LOCAL_USER), Role::Owner).acting_as(
            tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent(agent, LOCAL_USER)),
            grants,
        )
    }

    #[must_use]
    pub fn with_grant(mut self, grant: policy::Grant) -> Self {
        Arc::make_mut(&mut Arc::make_mut(&mut self.access).policy)
            .grants
            .push(grant);
        self
    }

    /// The same person acting through a delegation of `subject` carrying
    /// `grants`.
    #[must_use]
    pub fn acting_as(
        mut self,
        subject: tilo_member_types::Actor,
        grants: Vec<policy::Grant>,
    ) -> Self {
        self.access = Arc::new(Arc::unwrap_or_clone(self.access).acting_as(subject, grants));
        self
    }

    /// The same person, reading only: what a retired document or a past
    /// version offers whoever holds it.
    #[must_use]
    pub fn read_only(&self) -> Self {
        let mut context = Self::member_of(self.access.workspace(), &self.user, Role::Viewer);
        context.acting = self.acting.clone();
        context.now = self.now;
        context.policy_time.clone_from(&self.policy_time);
        context
    }

    /// Retain the actual host's fallible policy-time sampler (Unix seconds).
    /// This supplies time only, not authority. Every decision resamples;
    /// failure denies without consulting the wall clock or a numeric pin.
    /// Clones retain this same sampler. Nothing samples a clock that failed
    /// on a schedule: once it can be read again, its host announces that
    /// ([`outside_changed`]).
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
    pub fn with_once_spender(mut self, spend: SpendGrants) -> Self {
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
    /// author's (`writable`, the workspace's Editor) or a viewer's, as the
    /// workspace's grants now say
    /// (the workspace sync's `writable`). Only for
    /// the permissions a workspace launch made from its participation, the
    /// person a Viewer or an Editor and nothing bounding them; anything else
    /// (an owner's own app, a delegation) is `None` and stays as it is. The
    /// samplers, sandbox and spent grants carry over.
    #[must_use]
    pub fn with_participation(&self, writable: bool) -> Option<Self> {
        let person = self.access.person_id().unwrap_or_default().to_owned();
        let current = self.access.policy.members.get(&person).copied()?;
        if self.access.acting.delegated()
            || !matches!(current, Role::Viewer | Role::Editor)
            || self.access.policy.members.len() != 1
        {
            return None;
        }
        let role = if writable { Role::Editor } else { Role::Viewer };
        let mut access = (*self.access).clone();
        Arc::make_mut(&mut access.policy)
            .members
            .insert(person, role);
        let mut next = self.clone();
        next.access = Arc::new(access);
        Some(next)
    }

    /// The same context with the workspace's policy replaced, for a host that
    /// learned of a change.
    #[must_use]
    pub fn with_access(mut self, access: MemberAccess) -> Self {
        self.user = MemberId::person(access.person_id().unwrap_or_default());
        self.access = Arc::new(access);
        self
    }

    /// How long until this context's answers next change with time alone,
    /// in whole seconds of its policy clock: until the soonest instant a
    /// grant or sandbox capability it decides by stops being live. Never
    /// zero, so a wake for it never comes before the change. Only a context following a
    /// policy clock changes so; one judged at a fixed instant never does.
    /// The host's view of an app's action authority announces its own
    /// changes ([`outside_changed`]), as does a clock that cannot be read
    /// when it can be again.
    pub fn next_change_in(&self) -> Option<std::time::Duration> {
        let sampler = self.policy_time.as_ref()?;
        let now = (sampler.0)().ok()?;
        let ends = |lifetime: tilo_action_types::Lifetime| match lifetime {
            // Live while the clock's milliseconds are before it.
            tilo_action_types::Lifetime::Until(at) => {
                Some(at.div_ceil(1000)).filter(|at| *at > now)
            }
            _ => None,
        };
        let after = |grants: &mut dyn Iterator<Item = &policy::Grant>| {
            grants.filter_map(|grant| ends(grant.lifetime)).min()
        };
        let access_ends = |access: &MemberAccess| {
            after(&mut access.policy.grants.iter().chain(access.granted.iter()))
        };
        let authority = self
            .action_authority
            .as_ref()
            .and_then(|authority| authority.sample_for(self).ok())
            .and_then(|current| access_ends(&current.access));
        let sandbox = self
            .sandbox
            .iter()
            .filter_map(|cap| ends(cap.lifetime))
            .min();
        [access_ends(&self.access), authority, sandbox]
            .into_iter()
            .flatten()
            .min()
            .map(|at| std::time::Duration::from_secs(at.saturating_sub(now)))
    }

    /// Compare the current policy meaning, not callback allocation or elapsed
    /// seconds. Sampling is fallible; no cached/empty observation replaces failure.
    pub fn directory_observation(&self) -> Result<DirectoryPermissionObservation, String> {
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
                    Box::new(current.directory_observation()?),
                ))
            })
            .transpose()?;
        let cx = policy::Context::at_secs(now.unwrap_or(0));
        let live = self
            .access
            .policy
            .grants
            .iter()
            .chain(self.access.granted.iter())
            .map(|grant| grant.is_live(&cx))
            .chain(
                self.sandbox
                    .iter()
                    .map(|cap| cap.is_live_at(now.map(|now| now.saturating_mul(1000)))),
            )
            .collect();
        Ok(DirectoryPermissionObservation {
            user: self.user.clone(),
            acting: self.acting.clone(),
            sandbox: self.sandbox.clone(),
            access: Arc::clone(&self.access),
            live,
            action_authority,
            shown: Arc::clone(&self.shown),
        })
    }

    /// The same context reaching one more sandboxed resource.
    #[must_use]
    pub fn granting(mut self, cap: CapOverride) -> Self {
        self.sandbox.push(cap);
        self
    }

    /// The acting person's id.
    pub fn user(&self) -> &MemberId {
        &self.user
    }

    /// Who reaches sandboxed resources.
    pub fn acting(&self) -> &Acting {
        &self.acting
    }

    /// The role the person holds in the workspace, if any.
    pub fn role(&self) -> Option<Role> {
        self.access
            .policy
            .active_role(self.access.person_id().unwrap_or_default())
    }

    /// The access actions are decided against.
    pub fn access(&self) -> &MemberAccess {
        &self.access
    }

    /// The sandbox capabilities in force.
    pub fn capabilities(&self) -> &[CapOverride] {
        &self.sandbox
    }

    /// What the host said of the actions this window shows.
    #[must_use]
    pub fn shown(&self) -> &ShownVerdicts {
        &self.shown
    }

    /// The window asks its host about the `actions` it shows: they are refused here until their
    /// verdicts arrive.
    pub fn ask_shown(&mut self, actions: &[ShownAction]) {
        let asked = self.shown.asking(actions);
        if *self.shown != asked {
            self.shown = Arc::new(asked);
        }
    }

    /// Take the verdicts the host sent of the actions asked about ([`Self::ask_shown`]).
    pub fn decide_shown(&mut self, decisions: &[ActionDecision]) {
        Arc::make_mut(&mut self.shown).decide(decisions);
    }

    /// Why an invocation of `action` on `resource` is refused here, as a person reads it, when
    /// the host was asked about it or another workspace holds the record.
    #[must_use]
    pub fn shown_refusal(
        &self,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
    ) -> Option<tilo_actions::ApiError> {
        let reason = match self.shown.refusal(resource.id.as_str(), &action.id) {
            Some(reason) => reason,
            None if resource.workspace.is_some() => shown::CHECKING,
            None => return None,
        };
        Some(tilo_actions::ApiError::Unavailable {
            code: "permission_denied".into(),
            reason: reason.to_owned(),
        })
    }

    /// The answer for `action` on `resource` when the host was asked about it, or another
    /// workspace holds the record: what the host said, and refused until it did.
    fn shown_answer(&self, resource: &tilo_actions::ResourceSpec, action: &str) -> Option<Answer> {
        self.shown
            .answer(resource.id.as_str(), action)
            .or_else(|| resource.workspace.as_ref().map(|_| Answer::Deny))
    }

    fn policy_now(&self) -> Result<Option<u64>, String> {
        match &self.policy_time {
            Some(sampler) => (sampler.0)().map(Some),
            None => Ok(self.now),
        }
    }

    /// The answer for an action of `kind` at `node` below the workspace.
    pub fn answer(&self, node: &TreePath, kind: ActionKind) -> Answer {
        self.verdict_for(node, kind, false, false, |verdict, _| verdict.answer)
            .unwrap_or(Answer::Deny)
    }

    /// What `read` takes from the verdict for an action of `kind` at `node`:
    /// of a privileged action when `privileged` ([`policy::Verdict::privileged`]),
    /// relying on `Once` grants only when `once` (an admission that spends
    /// them) and this context can spend them. `read` is also told whether the
    /// verdict's Allow is the answer the person gave in advance
    /// ([`policy::Authority::answered_in_advance`]).
    fn verdict_for<R>(
        &self,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        once: bool,
        read: impl FnOnce(&policy::Verdict<'_>, bool) -> R,
    ) -> Result<R, String> {
        let now = self.policy_now()?;
        let cx = policy::Context::at_secs(now.unwrap_or(0));
        let sampled;
        let held = match &self.action_authority {
            Some(authority) if authority.matches(node) => {
                sampled = authority.sample_for(self)?.access;
                &sampled
            }
            _ => &self.access,
        };
        let access = self.usable(held, once);
        let authority = access.authority();
        let mut verdict = authority.decide(&access.node(node), kind, &cx);
        if privileged {
            verdict = verdict.privileged();
        }
        let answered_in_advance = authority.answered_in_advance(&verdict);
        Ok(read(&verdict, answered_in_advance))
    }

    /// `held` without the `Once` grants this decision may not rely on: all of
    /// them unless `once` (an admission that spends them) and this context
    /// can spend them, and those it already spent. Most accesses hold none
    /// to leave out and are decided against as they are; one is copied only
    /// to drop some.
    fn usable<'s>(&self, held: &'s MemberAccess, once: bool) -> std::borrow::Cow<'s, MemberAccess> {
        let usable = once && self.once_spender.is_some();
        let keep = |grant: &policy::Grant| {
            grant.lifetime != tilo_action_types::Lifetime::Once
                || (usable && !self.spent.contains(&grant.id))
        };
        let all_kept = held
            .policy
            .grants
            .iter()
            .chain(held.granted.iter())
            .all(keep);
        if all_kept {
            return std::borrow::Cow::Borrowed(held);
        }
        let mut access = held.clone();
        Arc::make_mut(&mut access.policy).grants.retain(keep);
        access.granted.retain(keep);
        std::borrow::Cow::Owned(access)
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
                match self.verdict_for(node, *kind, false, false, |verdict, _| {
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
                // The context's clock is in seconds; a lifetime ends in milliseconds.
                let now = now.map(|now| now.saturating_mul(1000));
                match decide_capability(&self.acting, resource, *access, &self.sandbox, now) {
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
        // Any member may ask the workspace's host to run a member-own action for themselves, as
        // them: what the app itself may do with the record decides nothing about it. Admission of
        // one the host runs still decides each call.
        if action.contract.is_member_own() {
            return Answer::Allow;
        }
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
        if let Some(answer) = self.shown.answer(resource_id, action_id) {
            return answer;
        }
        if self.action_authority.is_none() {
            return self
                .published_answers(|answer| {
                    let mut node = answer.resource(app_id, resource_kind, resource_id);
                    answer.action(&mut node, need.field.as_deref(), action_id, need)
                })
                .unwrap_or(Answer::Deny);
        }
        self.need_admission(app_id, resource_kind, resource_id, action_id, need)
            .answer
    }

    /// [`Self::need_answer`], and whether that answer is one the person gave in advance.
    fn need_admission(
        &self,
        app_id: &str,
        resource_kind: &str,
        resource_id: &str,
        action_id: &str,
        need: &tilo_actions::ActionNeed,
    ) -> WindowAnswer {
        if let Some(answer) = self.shown.answer(resource_id, action_id) {
            return WindowAnswer::of(answer);
        }
        if self.action_authority.is_none() {
            return self
                .published_answers(|answer| {
                    let mut node = answer.resource(app_id, resource_kind, resource_id);
                    answer.admission(&mut node, need.field.as_deref(), action_id, need)
                })
                .unwrap_or(WindowAnswer::of(Answer::Deny));
        }
        let node = tilo_actions::action_node(
            &TreePath::root(),
            app_id,
            resource_kind,
            resource_id,
            need.field.as_deref(),
            action_id,
        );
        self.verdict_for(
            &node,
            need.kind,
            need.privileged,
            false,
            |verdict, answered_in_advance| WindowAnswer {
                answer: verdict.answer,
                answered_in_advance,
            },
        )
        .unwrap_or(WindowAnswer::of(Answer::Deny))
    }

    /// The answer for admitting one invocation of `action` on `resource`,
    /// published by the app `app_id` in a window on the record `record`,
    /// with these arguments, and the `Once` grants admitting it spends: the
    /// API that runs it hands them to [`Self::spend_once`] first.
    pub fn invocation_verdict(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> (WindowAnswer, Vec<policy::GrantId>) {
        if let Some(answer) = self.shown_answer(resource, &action.id) {
            return (WindowAnswer::of(answer), Vec::new());
        }
        let need = input_need(&resource.kind, action, action.need_for(json, argument));
        let node = tilo_actions::action_node(
            &TreePath::root(),
            app_id,
            &resource.kind,
            resource.id.as_str(),
            need.field.as_deref(),
            &action.id,
        );
        self.verdict_for(
            &node,
            need.kind,
            need.privileged,
            true,
            |verdict, answered_in_advance| {
                let answer = WindowAnswer {
                    answer: verdict.answer,
                    answered_in_advance,
                };
                (answer, verdict.spent())
            },
        )
        .unwrap_or((WindowAnswer::of(Answer::Deny), Vec::new()))
    }

    /// The answer for invoking `action` on `resource` with these arguments.
    /// An Ask runs only once the person confirmed it in the host.
    pub fn invocation_answer(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> Answer {
        self.invocation_admission(app_id, resource, action, json, argument)
            .answer
    }

    /// [`Self::invocation_answer`], and whether its Allow is the answer the person gave in
    /// advance: what [`admit_in_window`] weighs.
    pub fn invocation_admission(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> WindowAnswer {
        // A member-own action runs for whoever is a member of the workspace: the app's own
        // context does not keep it from them, and the host admits each call.
        if action.contract.is_member_own() {
            return WindowAnswer::of(Answer::Allow);
        }
        if let Some(answer) = self.shown_answer(resource, &action.id) {
            return WindowAnswer::of(answer);
        }
        self.need_admission(
            app_id,
            &resource.kind,
            resource.id.as_str(),
            &action.id,
            &input_need(&resource.kind, action, action.need_for(json, argument)),
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

    /// [`Self::permits_action`] for one invocation of `action_id` that is what `need` is: an
    /// edit a carrier carries, rather than the least the carrier declares.
    fn permits_need(
        &self,
        app_id: &str,
        resource_kind: &str,
        resource_id: &str,
        action_id: &str,
        need: &tilo_actions::ActionNeed,
    ) -> bool {
        self.need_answer(app_id, resource_kind, resource_id, action_id, need) != Answer::Deny
    }

    /// Whether what the actor is denied is listed to it as not allowed rather than left out: an
    /// agent's, on a resource it may otherwise act on, so it hears that it is refused, and where
    /// that is changed, rather than that there is no such action. A person's controls show only
    /// what they may do.
    fn lists_refused(&self) -> bool {
        self.access.acting.is_agent()
    }

    /// `resource`, published by the app `app_id`, as the actor may reach
    /// it: the actions it may perform (allowed or asking first), and the
    /// nested resources that keep any. Denied is absent, or for an agent
    /// marked not allowed beside what it may do there ([`Self::lists_refused`]).
    /// `None` when nothing is left.
    pub fn permitted_resource(
        &self,
        app_id: &str,
        resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        // A host action authority decides some nodes from a member access it
        // samples per action, and the workspaces holding records held elsewhere decide theirs:
        // ask action by action.
        let mut held = std::collections::HashSet::new();
        held_elsewhere(&resource, &mut held);
        if self.action_authority.is_some() || !held.is_empty() || !self.shown.is_empty() {
            return keep_permitted(
                resource,
                &|kind, id, action, need| {
                    if held.contains(id) {
                        return self.shown.answer(id, &action.id).unwrap_or(Answer::Deny)
                            != Answer::Deny;
                    }
                    self.permits_need(app_id, kind, id, &action.id, need)
                },
                self.lists_refused(),
            );
        }
        // Otherwise one authority decides every action of the tree. An
        // unavailable clock refuses them all, as each verdict would.
        let refused = self.lists_refused();
        self.published_answers(|answers| answers.keep_permitted(app_id, resource, refused))
            .ok()
            .flatten()
    }

    /// Answers whether each of many actions of `app_id` is permitted, as
    /// [`Self::permits_action`] does.
    pub fn action_decider<'a>(&'a self, app_id: &'a str) -> ActionDecider<'a> {
        let answers = match (&self.action_authority, self.usable(&self.access, false)) {
            (None, std::borrow::Cow::Borrowed(access)) => {
                self.policy_now().ok().map(|now| PublishedAnswers {
                    authority: access.authority(),
                    workspace: access.policy.node(),
                    cx: policy::Context::at_secs(now.unwrap_or(0)),
                })
            }
            // A host action authority, a member access to filter, or no clock:
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

    /// `decide` with one authority for the member access as it is, for answers
    /// about published actions (no host action authority, no `Once` grant
    /// spent). An unavailable clock is an error.
    fn published_answers<R>(
        &self,
        decide: impl FnOnce(&PublishedAnswers<'_>) -> R,
    ) -> Result<R, String> {
        let now = self.policy_now()?;
        let access = self.usable(&self.access, false);
        let answers = PublishedAnswers {
            authority: access.authority(),
            workspace: access.policy.node(),
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
        // A member-own action is published whatever the app itself may do with the record: it
        // runs on the workspace's host, for whoever asked, as them.
        action.contract.is_member_own()
            || self.permits_need(
                resource_kind,
                resource_id,
                &action.id,
                &action.contract.need(),
            )
    }

    /// [`Self::permits`] for one invocation of `action_id` that is what `need` is: an edit a
    /// carrier carries, rather than the least the carrier declares.
    pub fn permits_need(
        &self,
        resource_kind: &str,
        resource_id: &str,
        action_id: &str,
        need: &tilo_actions::ActionNeed,
    ) -> bool {
        if let Some(answer) = self.permissions.shown.answer(resource_id, action_id) {
            return answer != Answer::Deny;
        }
        let Some(answers) = &self.answers else {
            return self.permissions.permits_need(
                self.app_id,
                resource_kind,
                resource_id,
                action_id,
                need,
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
        answers.action(node, need.field.as_deref(), action_id, need) != Answer::Deny
    }
}

/// One authority's answers for the actions a workspace's apps publish.
struct PublishedAnswers<'s> {
    authority: policy::Authority<'s>,
    workspace: TreePath,
    cx: policy::Context<'s>,
}

impl PublishedAnswers<'_> {
    /// The node of a published resource in this workspace.
    fn resource(&self, app_id: &str, kind: &str, id: &str) -> TreePath {
        tilo_actions::resource_node(&self.workspace, app_id, kind, id)
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
        Self::at_action(node, field, action, |node| {
            self.authority
                .answer(node, need.kind, need.privileged, &self.cx)
        })
    }

    /// [`Self::action`], and whether its Allow is the answer the person gave in advance
    /// ([`policy::Authority::answered_in_advance`]), read from the whole verdict only where it
    /// allows.
    fn admission(
        &self,
        node: &mut TreePath,
        field: Option<&str>,
        action: &str,
        need: &tilo_actions::ActionNeed,
    ) -> WindowAnswer {
        Self::at_action(node, field, action, |node| {
            let answer = self
                .authority
                .answer(node, need.kind, need.privileged, &self.cx);
            let answered_in_advance = answer == Answer::Allow && {
                let mut verdict = self.authority.decide(node, need.kind, &self.cx);
                if need.privileged {
                    verdict = verdict.privileged();
                }
                self.authority.answered_in_advance(&verdict)
            };
            WindowAnswer {
                answer,
                answered_in_advance,
            }
        })
    }

    /// What `read` says at the node of `action` (and `field`) on the resource `node` names, as
    /// [`tilo_actions::action_node`] places it; `node` is left as it was.
    fn at_action<R>(
        node: &mut TreePath,
        field: Option<&str>,
        action: &str,
        read: impl FnOnce(&TreePath) -> R,
    ) -> R {
        let resource = node.len();
        for part in field
            .unwrap_or_default()
            .split('.')
            .filter(|part| !part.is_empty())
        {
            node.push_name(policy::Level::Field, part);
        }
        node.push_name(policy::Level::Action, action);
        let read = read(node);
        node.truncate(resource);
        read
    }

    /// [`Self::action`] as [`Permitted`]: a refusal says what refused it.
    fn permitted(
        &self,
        node: &mut TreePath,
        field: Option<&str>,
        action: &str,
        need: &tilo_actions::ActionNeed,
    ) -> Permitted {
        if self.action(node, field, action, need) != Answer::Deny {
            return Ok(());
        }
        let resource = node.len();
        for part in field
            .unwrap_or_default()
            .split('.')
            .filter(|part| !part.is_empty())
        {
            node.push_name(policy::Level::Field, part);
        }
        node.push_name(policy::Level::Action, action);
        let mut verdict = self.authority.decide(node, need.kind, &self.cx);
        if need.privileged {
            verdict = verdict.privileged();
        }
        node.truncate(resource);
        Err(verdict.refused_by())
    }

    /// [`PermissionContext::permitted_resource`] with these answers.
    fn keep_permitted(
        &self,
        app_id: &str,
        mut resource: tilo_actions::ResourceSpec,
        refused: bool,
    ) -> Option<tilo_actions::ResourceSpec> {
        let mut node = self.resource(app_id, &resource.kind, resource.id.as_str());
        sift_actions(&mut resource, refused, |action, need| {
            self.permitted(&mut node, need.field.as_deref(), &action.id, need)
        });
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.keep_permitted(app_id, child, refused))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }
}

/// `resource` with only the actions `permits` keeps (by resource kind, id
/// and action) and the nested resources that keep any; `None` when nothing
/// is left.
fn keep_permitted(
    mut resource: tilo_actions::ResourceSpec,
    permits: &dyn Fn(&str, &str, &tilo_actions::ActionSpec, &tilo_actions::ActionNeed) -> bool,
    refused: bool,
) -> Option<tilo_actions::ResourceSpec> {
    let id = resource.id.as_str().to_owned();
    let kind = resource.kind.clone();
    sift_actions(&mut resource, refused, |action, need| {
        permitted(permits(&kind, &id, action, need))
    });
    resource.children = resource
        .children
        .into_iter()
        .filter_map(|child| keep_permitted(child, permits, refused))
        .collect();
    (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
}

/// `resource`'s actions as `permits` decides them (`Err` with what refused one, where the check
/// knows): the denied ones taken out, or, where `refused` lists them
/// ([`PermissionContext::lists_refused`]) and the resource keeps any action the actor may take,
/// kept and marked not allowed ([`tilo_actions::Availability::not_allowed`]). A resource the
/// actor may take nothing on discloses none of its actions.
fn sift_actions(
    resource: &mut tilo_actions::ResourceSpec,
    refused: bool,
    mut permits: impl FnMut(&tilo_actions::ActionSpec, &tilo_actions::ActionNeed) -> Permitted,
) {
    let mut allowed: Vec<Permitted> = resource
        .actions
        .iter()
        .map(|action| {
            if action.contract.is_member_own() {
                // Offered to every member, and kept whatever the actor may do here: the host
                // runs it for them, as them, and admits each call.
                Ok(())
            } else {
                permits(action, &action.contract.need())
            }
        })
        .collect();
    offer_carriers(resource, &mut allowed, &mut permits);
    // What a carrier beside them names is cut by these same answers: an action dropped here,
    // or listed as not allowed, is one the actor's call of it through a typed edit or a group
    // is refused for too, so neither names it to them.
    let denied: Vec<String> = resource
        .actions
        .iter()
        .zip(&allowed)
        .filter(|(_, allowed)| allowed.is_err())
        .map(|(action, _)| action.id.clone())
        .collect();
    if refused && allowed.iter().any(Result::is_ok) {
        for (action, allowed) in resource.actions.iter_mut().zip(allowed) {
            if let Err(by) = allowed {
                mark_refused(action, by);
            }
        }
    } else {
        let mut allowed = allowed.into_iter();
        resource.retain_actions(|_| allowed.next().is_some_and(|allowed| allowed.is_ok()));
    }
    if !denied.is_empty() {
        let denied: Vec<&str> = denied.iter().map(String::as_str).collect();
        for action in &mut resource.actions {
            action.without_carried(&denied);
        }
    }
}

/// Mark `action` as one the actor may not take, saying what refused it.
fn mark_refused(action: &mut tilo_actions::ActionSpec, by: Option<policy::Refuser>) {
    action.type_mut().contract.availability = tilo_actions::Availability::not_allowed(by);
}

/// Offer a carrier the actor may take any of `resource`'s own actions with. A generic edit or a
/// group is offered under the least of the edits it carries and decided per invocation, so its
/// least need refusing it says nothing of the edits it can make. It is also offered to whoever
/// may take any edit here (a grant of one action): each value it carries is still decided when
/// it is invoked. Only edits count: a read of the resource is no edit of it.
fn offer_carriers(
    resource: &tilo_actions::ResourceSpec,
    allowed: &mut [Permitted],
    permits: &mut impl FnMut(&tilo_actions::ActionSpec, &tilo_actions::ActionNeed) -> Permitted,
) {
    let any_change = resource
        .actions
        .iter()
        .zip(allowed.iter())
        .any(|(action, allowed)| {
            allowed.is_ok()
                && !action.carries_actions()
                && action.contract.need().kind != tilo_actions::ActionKind::Read
        });
    if any_change {
        for (action, allowed) in resource.actions.iter().zip(allowed.iter_mut()) {
            if action.carries_actions() {
                *allowed = Ok(());
            }
        }
        return;
    }
    let carried: Vec<tilo_actions::ActionNeed> = resource
        .actions
        .iter()
        .filter(|action| !action.carries_actions())
        .map(|action| action.contract.need())
        .collect();
    for (action, allowed) in resource.actions.iter().zip(allowed.iter_mut()) {
        if allowed.is_ok() || !action.carries_actions() {
            continue;
        }
        let least = action.contract.need().kind;
        if carried
            .iter()
            .any(|need| need.kind >= least && permits(action, need).is_ok())
        {
            *allowed = Ok(());
        }
    }
}

/// Whether an action is permitted, or what refuses it where the check knows.
type Permitted = Result<(), Option<policy::Refuser>>;

/// `allowed` as [`Permitted`], from a check that does not say what refused it.
const fn permitted(allowed: bool) -> Permitted {
    if allowed { Ok(()) } else { Err(None) }
}

/// The permissions a host shares with what it hosts (an app, its components,
/// a worker) and changes while they run: a role changed by the record's
/// owner, a grant revoked, a workspace's participation. Every change goes through
/// [`Self::set`], which moves [`shared_revision`] on, so whatever derives
/// something from shared permissions (which controls may run) derives it
/// again only after a change, not on every frame.
#[derive(Clone)]
pub struct SharedPermissions(std::rc::Rc<std::cell::RefCell<PermissionContext>>);

thread_local! {
    static SHARED_REVISION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Moves on with every change of an outside authority shared permissions
/// follow ([`outside_changed`]).
static OUTSIDE_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What to call when an outside authority changes: each answers whether it still wants to hear.
type OutsideWaker = Box<dyn Fn() -> bool + Send>;

/// The loops to wake when an outside authority changes.
static OUTSIDE_WAKES: std::sync::Mutex<Vec<OutsideWaker>> = std::sync::Mutex::new(Vec::new());

/// Moves on with every change of any [`SharedPermissions`] on this thread,
/// and with every change of an outside authority they follow.
#[must_use]
pub fn shared_revision() -> u64 {
    SHARED_REVISION
        .get()
        .wrapping_add(OUTSIDE_REVISION.load(std::sync::atomic::Ordering::Acquire))
}

/// An outside authority permissions follow changed, on any thread: the
/// host's view of an app's action authority, or a policy clock
/// ([`PermissionContext::with_policy_time_sampler`]) that could not be read
/// and can be again. Moves [`shared_revision`] on and wakes every loop that
/// asked ([`wake_on_outside_change`]), so what it derived from permissions
/// is derived again.
pub fn outside_changed() {
    OUTSIDE_REVISION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    OUTSIDE_WAKES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|wake| wake());
}

/// Call `wake` whenever an outside authority permissions follow changes, until it answers that it
/// no longer wants to hear (a loop whose scope closed).
pub fn wake_on_outside_change(wake: impl Fn() -> bool + Send + 'static) {
    OUTSIDE_WAKES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Box::new(wake));
}

impl SharedPermissions {
    /// Share `context`.
    #[must_use]
    pub fn new(context: PermissionContext) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(context)))
    }

    /// The permissions in force now, to read.
    ///
    /// # Panics
    /// While a [`Self::set`] of the same permissions runs.
    pub fn borrow(&self) -> std::cell::Ref<'_, PermissionContext> {
        self.0.borrow()
    }

    /// [`Self::borrow`], or an error while a change of them runs.
    ///
    /// # Errors
    /// While a [`Self::set`] or [`Self::update`] of the same permissions runs.
    pub fn try_borrow(
        &self,
    ) -> Result<std::cell::Ref<'_, PermissionContext>, std::cell::BorrowError> {
        self.0.try_borrow()
    }

    /// Change the permissions in force in place (a one-use grant spent).
    pub fn update<R>(&self, change: impl FnOnce(&mut PermissionContext) -> R) -> R {
        let (result, moved) = {
            let mut live = self.0.borrow_mut();
            let before = live.clone();
            let result = change(&mut live);
            (result, *live != before)
        };
        // A change that changed nothing is no change: nothing drawn from
        // these permissions is stale.
        if moved {
            SHARED_REVISION.set(SHARED_REVISION.get().wrapping_add(1));
        }
        result
    }

    /// The permissions in force now.
    #[must_use]
    pub fn get(&self) -> PermissionContext {
        self.0.borrow().clone()
    }

    /// Put `context` in force for everyone sharing these permissions.
    pub fn set(&self, context: PermissionContext) {
        {
            let mut live = self.0.borrow_mut();
            // What the host said of the actions shown is the host's to change
            // ([`PermissionContext::decide_shown`]), not this window's access.
            let mut context = context;
            context.shown = Arc::clone(&live.shown);
            // A sync that hands over the permissions already in force moved
            // nothing: what was drawn from them stays.
            if *live == context {
                return;
            }
            *live = context;
        }
        SHARED_REVISION.set(SHARED_REVISION.get().wrapping_add(1));
    }

    /// Whether `self` and `other` are the same shared permissions.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }

    /// A handle that does not keep the permissions shared.
    #[must_use]
    pub fn downgrade(&self) -> WeakPermissions {
        WeakPermissions(std::rc::Rc::downgrade(&self.0))
    }
}

impl From<PermissionContext> for SharedPermissions {
    fn from(context: PermissionContext) -> Self {
        Self::new(context)
    }
}

impl std::fmt::Debug for SharedPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedPermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}

/// [`SharedPermissions`] held without keeping them shared.
#[derive(Clone)]
pub struct WeakPermissions(std::rc::Weak<std::cell::RefCell<PermissionContext>>);

impl WeakPermissions {
    /// The permissions, while anyone still shares them.
    #[must_use]
    pub fn upgrade(&self) -> Option<SharedPermissions> {
        self.0.upgrade().map(SharedPermissions)
    }

    /// Whether anyone still shares the permissions.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        self.0.strong_count() > 0
    }
}

/// A member's permissions as they stand *now*, following every change the
/// host makes while the app runs: a role changed by the record's owner, a grant
/// revoked, a capability granted. Read-only: only the host changes them.
///
/// A [`PermissionContext`] is a snapshot; code that decides once at launch
/// what a member may do goes stale the moment their role changes. Hold
/// one of these instead and ask again when it matters (every frame is cheap).
#[derive(Clone)]
pub struct LivePermissions(SharedPermissions);

impl LivePermissions {
    /// Follow the context the host shares with its components: what a host that is no app window
    /// (a page's own sheet) opens a record with, decided as the host's windows are.
    #[must_use]
    pub fn shared(context: SharedPermissions) -> Self {
        Self(context)
    }

    /// Permissions that never change: a preview of a past version, a capture,
    /// a test fixture.
    pub fn fixed(context: PermissionContext) -> Self {
        Self(SharedPermissions::new(context))
    }

    /// May the member do this, as of now?
    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.0.borrow().check(scope)
    }

    /// The answer for an action of `kind` at `node` below the workspace, as of now.
    pub fn answer(&self, node: &TreePath, kind: ActionKind) -> Answer {
        self.0.borrow().answer(node, kind)
    }

    /// Whether anyone besides the member belongs to the workspace, its owner
    /// or a member, here now or not: what the workspace holds is shared with them.
    pub fn workspace_has_others(&self) -> bool {
        let context = self.0.borrow();
        let access = context.access();
        access
            .policy
            .has_others(access.person_id().unwrap_or_default())
    }

    /// The permissions in force now.
    pub fn current(&self) -> PermissionContext {
        self.0.get()
    }

    /// The window asks its host about the actions it shows
    /// ([`PermissionContext::ask_shown`]).
    pub fn ask_shown(&self, actions: &[ShownAction]) {
        self.0.update(|context| context.ask_shown(actions));
    }

    /// The host's verdicts of them ([`PermissionContext::decide_shown`]).
    pub fn decide_shown(&self, decisions: &[ActionDecision]) {
        self.0.update(|context| context.decide_shown(decisions));
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
use action_authority::ActionAuthoritySampler;
pub mod shown;
pub mod shown_actions;
use crate::shown_actions::{ActionDecision, ShownAction};
pub use shown::ShownVerdicts;

/// Add to `held` the id of every resource in `resource` held in another workspace.
fn held_elsewhere(
    resource: &tilo_actions::ResourceSpec,
    held: &mut std::collections::HashSet<String>,
) {
    if resource.workspace.is_some() {
        held.insert(resource.id.as_str().to_owned());
    }
    for child in &resource.children {
        held_elsewhere(child, held);
    }
}

/// What invoking an action of a resource of `kind` needs. The verbs an
/// accessibility surface publishes for a control — focusing it, pressing it,
/// toggling it, setting its value — stand in for the person's own pointer and
/// keyboard, which only they invoke
/// ([`tilo_actions::kind_is_a_drawn_control`]), and need what reading needs:
/// what the control then writes is decided as those writes are, so a person
/// typing in a field is not asked first as if the field reached out. The
/// control's own action says what it does and keeps that need; a button's
/// effect that reaches outside still asks.
fn input_need(
    kind: &str,
    action: &tilo_actions::ActionSpec,
    need: tilo_actions::ActionNeed,
) -> tilo_actions::ActionNeed {
    if tilo_actions::kind_is_a_drawn_control(kind)
        && action.operation_type.as_deref() == Some(tilo_actions::SEMANTIC_OPERATION_TYPE)
    {
        tilo_actions::ActionNeed {
            kind: ActionKind::Read,
            field: None,
            privileged: false,
        }
    } else {
        need
    }
}

/// What a window's grants answer for one invocation, as [`admit_in_window`] weighs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowAnswer {
    /// The grants' answer for the action at its node.
    pub answer: Answer,
    /// Whether that Allow is the answer the person gave their agent in advance for this very
    /// action or resource ([`policy::Authority::answered_in_advance`]): what someone acting for
    /// them may then do unasked, though it leaves the system or its person attends.
    pub answered_in_advance: bool,
}

impl WindowAnswer {
    /// `answer`, given by nobody in advance.
    #[must_use]
    pub const fn of(answer: Answer) -> Self {
        Self {
            answer,
            answered_in_advance: false,
        }
    }
}

/// The refusal of an invocation the presence may not take.
pub fn denied(action: &tilo_actions::ActionSpec) -> tilo_actions::ApiError {
    tilo_actions::ApiError::Unavailable {
        code: "permission_denied".into(),
        reason: format!("you may not {} here", action.label.to_lowercase()),
    }
}

/// The shared admission ([`tilo_action_policy::admit`]) as a window's API
/// takes it, the decision a broker takes for a caller: `asker` is who the
/// invocation asks ([`PermissionContext::asker`]).
///
/// # Errors
/// Refused, or [`tilo_actions::ApiError::ConfirmationRequired`] where the
/// workspace's grants leave it to the person: the call then goes to the
/// device's dispatch, which keeps it as its asked record until they answer.
pub fn admit_in_window(
    answer: WindowAnswer,
    action: &tilo_actions::ActionSpec,
    asker: tilo_action_policy::Asker,
) -> Result<(), tilo_actions::ApiError> {
    let admitted = tilo_action_policy::admit(
        tilo_action_policy::Invoked {
            answer: answer.answer,
            attended: action.contract.is_attended(),
            outside: action.contract.kind == tilo_actions::ActionKind::Outside,
            answered_in_advance: answer.answered_in_advance,
            resource: None,
        },
        asker,
    );
    match admitted {
        Err(_) => Err(denied(action)),
        Ok(tilo_action_policy::Admission::AsksFirst) => {
            Err(tilo_actions::ApiError::ConfirmationRequired)
        }
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod policy_time_tests;

#[cfg(test)]
mod control_input_tests;

#[cfg(test)]
mod answered_in_advance_tests;

#[cfg(test)]
mod refused_listing_tests {
    use super::*;
    use tilo_actions::{ActionSpec, OperationKind};

    /// An agent is shown what it is denied as not allowed, beside what it may do on the same
    /// resource, so it hears that it is refused rather than that there is no such action, here by
    /// the workspace's own rule (its person may only read), as by its person's settings for it; a
    /// person's controls show only what they may do, and a resource the agent may do nothing on
    /// discloses nothing.
    #[test]
    fn an_agent_is_listed_what_it_is_denied_as_not_allowed() {
        let person = crate::MemberId::person("ad".repeat(32));
        let mut read =
            ActionSpec::operation("read", "Read", OperationKind::View, "t.read", &()).unwrap();
        read.type_mut().contract.kind = ActionKind::Read;
        let mut edit =
            ActionSpec::operation("edit", "Edit", OperationKind::Effect, "t.edit", &()).unwrap();
        edit.type_mut().contract.kind = ActionKind::Undoable;
        let note = tilo_actions::ResourceSpec::new("notes.page", "page:1", "Plan")
            .with_action(read)
            .with_action(edit.clone());
        let viewer = PermissionContext::member(&person, Role::Viewer);
        let shown = viewer
            .permitted_resource("so.tilo.notes", note.clone())
            .expect("the viewer reads it");
        assert!(shown.actions.iter().all(|action| action.id != "edit"));
        // Its agent, which the person lets read and change: their own role still denies changes.
        let agent = viewer.acting_as(
            tilo_member_types::Actor::Member(crate::MemberId::agent("scribe", person.id())),
            vec![policy::Grant::new(
                "scribe-notes",
                policy::Grantee::agent("scribe", person.id()),
                TreePath::workspace(LOCAL_WORKSPACE),
                tilo_action_types::KindSet::READ_UNDOABLE,
                Answer::Allow,
                policy::Grantee::person(person.id()),
            )],
        );
        let listed = agent
            .permitted_resource("so.tilo.notes", note)
            .expect("the agent reads it");
        let edit_listed = listed
            .actions
            .iter()
            .find(|action| action.id == "edit")
            .expect("listed to the agent");
        assert_eq!(
            edit_listed.contract.availability,
            tilo_actions::Availability::not_allowed(Some(policy::Refuser::Workspace)),
            "the workspace's own rule refuses it"
        );
        let only_edit =
            tilo_actions::ResourceSpec::new("notes.page", "page:2", "Secret").with_action(edit);
        assert!(
            agent
                .permitted_resource("so.tilo.notes", only_edit.clone())
                .is_none()
        );
        // An editor's agent their person set to Deny for changes in Notes is told it is its
        // person's setting, and where they change it.
        let editor = PermissionContext::member(&person, Role::Editor);
        let denied = editor.acting_as(
            tilo_member_types::Actor::Member(crate::MemberId::agent("scribe", person.id())),
            [policy::Grant::new(
                "scribe-all",
                policy::Grantee::agent("scribe", person.id()),
                TreePath::workspace(LOCAL_WORKSPACE),
                tilo_action_types::KindSet::READ_UNDOABLE,
                Answer::Allow,
                policy::Grantee::person(person.id()),
            )]
            .into_iter()
            // Its person's answer, as the workspace's settings under Agents keep it: Deny for
            // changes in Notes.
            .chain([policy::agent_answers::grant(
                LOCAL_WORKSPACE,
                ("scribe", person.id()),
                (
                    &policy::agent_answers::AnswerScope::app("so.tilo.notes"),
                    ActionKind::Undoable,
                ),
                Answer::Deny,
                policy::Grantee::person(person.id()),
            )])
            .collect(),
        );
        let note = tilo_actions::ResourceSpec::new("notes.page", "page:3", "Plan")
            .with_action(shown.actions[0].clone())
            .with_action(only_edit.actions[0].clone());
        let listed = denied
            .permitted_resource("so.tilo.notes", note)
            .expect("the agent reads it");
        let edit_listed = listed.actions.iter().find(|action| action.id == "edit");
        assert_eq!(
            edit_listed.map(|action| &action.contract.availability),
            Some(&tilo_actions::Availability::not_allowed(Some(
                policy::Refuser::Settings
            ))),
            "its person's setting refuses it"
        );
    }

    /// A typed edit says which actions it carries, and each call of it is decided by the one
    /// it carries. What it names to an actor is cut by the answers that decide those actions
    /// by their own names: to pin is a moderator's, so an editor's typed edit does not
    /// name it, and their agent, listed it as not allowed, is not named it there either.
    #[test]
    fn a_carrier_names_to_an_actor_only_what_they_may_take_by_name() {
        let person = crate::MemberId::person("ad".repeat(32));
        let edit = |id: &str| {
            let mut spec =
                ActionSpec::operation(id, id, OperationKind::Effect, format!("t.{id}"), &())
                    .unwrap();
            spec.type_mut().contract.kind = ActionKind::Undoable;
            spec
        };
        let carrier = ActionSpec::typed(
            "apply",
            "Apply",
            "Action as JSON",
            tilo_actions::ExecutionLocus::ClientLocal,
            vec![
                tilo_actions::ActionParam::required(
                    "action",
                    tilo_actions::ParamType::Object,
                    "An edit",
                )
                .with_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": ["post", "pin"]},
                        "args": {"type": "object"},
                    },
                })),
            ],
            |_| tilo_actions::EncodedAction {
                type_id: "t.apply".to_owned(),
                payload: Vec::new(),
            },
        )
        .carrying()
        .of_kind(ActionKind::Undoable);
        let chat = tilo_actions::ResourceSpec::new("chat", "chat:1", "Chat")
            .with_action(edit("post"))
            .with_action(edit("pin").below_field(policy::roles::MODERATION_FIELD))
            .with_action(carrier);
        let named = |shown: &tilo_actions::ResourceSpec| {
            shown
                .actions
                .iter()
                .find(|action| action.id == "apply")
                .and_then(|apply| apply.params().first()?.schema.clone())
                .map(|schema| schema["properties"]["action"]["enum"].clone())
        };
        let both = Some(serde_json::json!(["post", "pin"]));
        let post_alone = Some(serde_json::json!(["post"]));
        let moderator = PermissionContext::member(&person, Role::Moderator)
            .permitted_resource("so.tilo.chat", chat.clone())
            .expect("a moderator edits it");
        assert_eq!(named(&moderator), both, "a moderator may both");
        let editor = PermissionContext::member(&person, Role::Editor);
        let shown = editor
            .permitted_resource("so.tilo.chat", chat.clone())
            .expect("an editor edits it");
        assert!(
            shown.actions.iter().all(|action| action.id != "pin"),
            "an editor is not listed the pin"
        );
        assert_eq!(named(&shown), post_alone, "nor named it by the typed edit");
        let agent = editor.acting_as(
            tilo_member_types::Actor::Member(crate::MemberId::agent("scribe", person.id())),
            vec![policy::Grant::new(
                "scribe-chat",
                policy::Grantee::agent("scribe", person.id()),
                TreePath::workspace(LOCAL_WORKSPACE),
                tilo_action_types::KindSet::READ_UNDOABLE,
                Answer::Allow,
                policy::Grantee::person(person.id()),
            )],
        );
        let listed = agent
            .permitted_resource("so.tilo.chat", chat)
            .expect("the agent edits it");
        assert!(
            listed
                .actions
                .iter()
                .find(|action| action.id == "pin")
                .is_some_and(|action| !action.contract.availability.is_available()),
            "the agent is listed the pin as not allowed"
        );
        assert_eq!(
            named(&listed),
            post_alone,
            "and its typed edit does not name it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(
        subject: policy::Grantee,
        node: TreePath,
        kinds: tilo_action_types::KindSet,
    ) -> policy::Grant {
        policy::Grant::new(
            "grant",
            subject,
            node,
            kinds,
            Answer::Allow,
            policy::Grantee::person("owner"),
        )
    }

    tilo_test_env::scenario! {
        fn permissions_reach_their_scope_keep_identity_and_deny_by_default;

        phase grants_and_capabilities_reach_exactly_their_scope_while_live;
        {
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
                assert_eq!(
                    ctx.capabilities().len(),
                    3 + crate::caps::DeviceCap::ALL.len(),
                    "every path, site and file, and every device by its type"
                );
                assert_eq!(ctx.capabilities()[0].access, Access::Write);
                assert_eq!(ctx.capabilities()[0].scope, Scope::FsPath("/".into()));
                assert!(ctx.check(&PermissionScope::app("notes", ActionKind::Outside)));

                // The same owner, no capability: a workspace role confers no reach over
                // paths.
                let stripped = PermissionContext::member(&crate::MemberId::person(LOCAL_USER), Role::Owner);
                assert!(!stripped.check(&PermissionScope::path(
                    "/var/workspace/secret.png",
                    Access::Read
                )));
                assert!(stripped.check(&PermissionScope::app("notes", ActionKind::Outside)));
            }
            // phase: action_grant_is_narrow_while_app_grant_covers_every_action
            {
                let agent = |grants: Vec<policy::Grant>| {
                    let owner = PermissionContext::member(&crate::MemberId::person("owner"), Role::Owner);
                    owner
                        .clone()
                        .with_access(owner.access().clone().acting_as(
                            tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent("scribe", "owner")),
                            grants,
                        ))
                };
                let workspace = TreePath::workspace(LOCAL_WORKSPACE);
                let open = |page: &str, action: &str| {
                    PermissionScope::action("notes", "notes.page", page, action, ActionKind::Read)
                };
                let narrow = agent(vec![grant(
                    policy::Grantee::agent("scribe", "owner"),
                    workspace.join(&action_node(
                        "notes",
                        "notes.page",
                        "notes.page:1",
                        "note.open",
                    )),
                    tilo_action_types::KindSet::READ,
                )]);
                assert!(narrow.check(&open("notes.page:1", "note.open")));
                assert!(!narrow.check(&open("notes.page:1", "note.delete")));
                assert!(
                    !narrow.check(&open("notes.page:2", "note.open")),
                    "the same action on another page is not granted"
                );

                let broad = agent(vec![grant(
                    policy::Grantee::agent("scribe", "owner"),
                    workspace.app("notes"),
                    tilo_action_types::KindSet::READ,
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
                    Grantee::person("u_guest"),
                    Scope::FsPath("/var/workspace/shared".into()),
                    Access::Write,
                )
                .lasting(tilo_action_types::Lifetime::Until(1_000_000));
                let scope = PermissionScope::path("/var/workspace/shared/notes.md", Access::Write);
                let context =
                    || PermissionContext::member(&crate::MemberId::person("u_guest"), Role::Viewer).granting(cap.clone());
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

        phase action_checks_keep_resource_identity_and_the_contract_kind;
        {
            use tilo_actions::{ActionSpec, OperationKind};

            let action =
                ActionSpec::operation("edit", "Edit", OperationKind::Effect, "test.edit", &()).unwrap();
            assert_eq!(action.contract.kind, ActionKind::Outside);
            let owner = PermissionContext::member(&crate::MemberId::person("owner"), Role::Owner);
            let node = TreePath::workspace(LOCAL_WORKSPACE).join(&action_node(
                "notes",
                "notes.page",
                "notes.page:1",
                "edit",
            ));
            let context = |kinds| {
                owner
                    .clone()
                    .with_access(owner.access().clone().acting_as(
                        tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent("scribe", "owner")),
                        vec![grant(policy::Grantee::agent("scribe", "owner"), node.clone(), kinds)],
                    ))
            };
            let outside = context(tilo_action_types::KindSet::OUTSIDE);
            assert!(outside.permits_action("notes", "notes.page", "notes.page:1", &action));
            assert!(!outside.permits_action("notes", "notes.page", "notes.page:2", &action));
            assert!(!outside.permits_action("other", "notes.page", "notes.page:1", &action));
            assert!(
                !context(tilo_action_types::KindSet::READ_UNDOABLE).permits_action(
                    "notes",
                    "notes.page",
                    "notes.page:1",
                    &action
                ),
                "an outside action is not covered by an undoable grant"
            );
        }

        phase denials_are_default_and_explained;
        {
            // phase: viewer_is_denied_changes
            {
                let ctx = PermissionContext::member(&crate::MemberId::person("u_guest"), Role::Viewer);
                let write = PermissionScope::app("notes", ActionKind::Undoable);
                assert!(!ctx.check(&write), "a Viewer must not change the workspace");
                assert!(matches!(
                    ctx.check_with_denied_feedback(&write),
                    Some(Denial::NotGranted(_))
                ));
                assert!(ctx.check(&PermissionScope::app("notes", ActionKind::Read)));
            }
            // phase: a_restriction_denies_inside_its_node
            {
                let mut ctx = PermissionContext::member(&crate::MemberId::person("u_editor"), Role::Editor);
                let mut access = ctx.access().clone();
                Arc::make_mut(&mut access.policy)
                    .grants
                    .push(policy::Grant::new(
                        "terminal-read-only",
                        policy::Grantee::role(Role::Editor),
                        TreePath::workspace(LOCAL_WORKSPACE).app(AppKind::Terminal.app_id()),
                        tilo_action_types::KindSet::UNDOABLE,
                        Answer::Deny,
                        policy::Grantee::person("owner"),
                    ));
                ctx = ctx.with_access(access);
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
                let owner = PermissionContext::member(&crate::MemberId::person("owner"), Role::Owner);
                let app = owner
                    .clone()
                    .with_access(owner.access().clone().acting_as(
                        tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent("thing", "owner")),
                        Vec::new(),
                    ));
                assert!(!app.check(&PermissionScope::workspace(ActionKind::Read)));
                assert!(!app.check(&PermissionScope::app_kind(
                    AppKind::FileExplorer,
                    ActionKind::Read
                )));
                assert!(!app.check(&PermissionScope::path("/etc/shadow", Access::Read)));
            }
        }
    }

    /// A member-own action stays on a resource whatever the context itself may do with it: the
    /// workspace's host runs it for a member, as them, so a reader's own context publishes it too,
    /// and each call is admitted on its own.
    #[test]
    fn a_member_own_action_is_published_whatever_the_actor_may_do_with_the_record() {
        let mut contract = tilo_actions::ActionContract {
            kind: ActionKind::Undoable,
            ..tilo_actions::ActionContract::default()
        };
        contract.tags.push(tilo_actions::MEMBER_OWN_TAG.to_owned());
        let connect = tilo_actions::ActionSpec::operation(
            "connect",
            "connect",
            tilo_actions::OperationKind::Effect,
            "discord.effects",
            &(),
        )
        .unwrap()
        .with_contract(contract);
        let mut resource =
            tilo_actions::ResourceSpec::new("discord.guild", "discord.guild", "Discord server");
        resource.actions.push(connect);

        let reader = PermissionContext::member(&crate::MemberId::person("reader"), Role::Viewer);
        let kept = reader
            .permitted_resource("discord", resource)
            .expect("the reader keeps the member-own action");
        assert!(
            kept.actions.iter().any(|action| action.id == "connect"),
            "a member-own action is published to a reader"
        );
    }
}
