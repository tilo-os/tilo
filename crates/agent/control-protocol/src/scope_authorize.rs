//! One authorization function for people and agents (security review MUST-1).
//!
//! [`authorize_operation`] is pure over an [`OperationPolicy`] (the host's
//! workspace policy) and the live descriptor of one published operation:
//!
//! - **Person** (scope = ⊤): `decide(Action{app, action}, verb)` **and**
//!   `decide(Artifact(app, canonical_record(route)), verb)`. The record check
//!   closes the gap where the human check ignored the record.
//! - **Agent**: the same two checks for the agent principal *and* for the
//!   delegating owner (T21), plus: explicit class from a trusted delegation
//!   manifest (MUST-4), never `effect.fs.*` on a host worker, confirmation
//!   refused unattended (T4), bulk ops refused unattended (T5), scope coverage
//!   on the canonical route record (MUST-2) with the frozen schema digest
//!   (MUST-6), and argument bounds (`delete {count}` ≤ budget).
//!
//! Task state (accepted, not finished, not exited), governance pinning and
//! step/wall-clock/rate budgets are the host Store's part of the same step;
//! [`check_step_budget`], [`RateLimiter`], [`check_removal`] and
//! [`check_execution`] are their pure halves.

use crate::{
    DeclaredKind, DelegationRegistry, GrantScope, GrantVerb, OperationClass, ScopeBudgets,
    ScopeTarget, canonical_record,
};

/// `tilo_actions::PermissionRequirement`, mapped at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermissionLevel {
    None,
    Read,
    Comment,
    Write,
    Invite,
    Admin,
}
impl PermissionLevel {
    /// The scope verb an operation needs; `Err` when it is never delegable.
    fn grant_verb(self) -> Result<GrantVerb, Refusal> {
        match self {
            Self::None | Self::Read => Ok(GrantVerb::Read),
            Self::Comment => Ok(GrantVerb::Comment),
            Self::Write => Ok(GrantVerb::Write),
            Self::Invite | Self::Admin => Err(Refusal::NotDelegable(
                "invite and admin operations are never delegable",
            )),
        }
    }
    fn from_verb(verb: GrantVerb) -> Self {
        match verb {
            GrantVerb::Read => Self::Read,
            GrantVerb::Comment => Self::Comment,
            GrantVerb::Write => Self::Write,
        }
    }
}

/// What a policy decision ranges over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyObject<'a> {
    /// `ResourceRef::Action{app, action}`.
    Action { app: &'a str, action: &'a str },
    /// `ResourceRef::Artifact(app, document_artifact_id(app, room, route_record))`.
    Record { app: &'a str, route_record: &'a str },
}

/// An agent principal's explicit workspace standing (`role_of_agent_at`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStanding {
    /// No agent membership: the agent acts with the fixed least-privilege
    /// delegated role, the covering scope's verb.
    Absent,
    /// An explicit role; its ceiling caps the agent (an admin may set an
    /// agent to Commenter).
    Member(PermissionLevel),
    Denied,
}

/// The host's workspace policy, evaluated at the step's policy time.
pub trait OperationPolicy {
    /// `decide(person actor, object, verb) == Allow`.
    fn person_allows(&self, person: &str, object: PolicyObject<'_>, verb: PermissionLevel) -> bool;
    fn agent_standing(&self, agent: &str) -> AgentStanding;
    /// The role ceiling a person holds now (`None` = not a member). An agent
    /// without its own roster entry acts with its spawner's member rights.
    fn person_ceiling(&self, person: &str) -> Option<PermissionLevel>;
    /// `decide(agent actor holding a role with ceiling, object, verb) == Allow`
    /// (agent-specific restrictions still apply).
    fn agent_allows(
        &self,
        agent: &str,
        ceiling: PermissionLevel,
        object: PolicyObject<'_>,
        verb: PermissionLevel,
    ) -> bool;
}

/// The authenticated invoker, set by the host (never from a task envelope).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokingActor<'a> {
    Person { id: &'a str },
    Agent { agent: &'a str, delegator: &'a str },
}

/// The live descriptor of one published operation on a live, registered
/// route. `app_id` and `route_record` come only from the broker's route.
#[derive(Clone, Copy, Debug)]
pub struct OperationDescriptor<'a> {
    pub app_id: &'a str,
    pub route_record: &'a str,
    pub resource: &'a str,
    pub resource_kind: &'a str,
    pub action: &'a str,
    pub declared: DeclaredKind,
    pub permission: PermissionLevel,
    pub confirmation_required: bool,
    /// The invocation argument (JSON or text) for argument bounds.
    pub argument: Option<&'a str>,
}

/// An agent's frozen grant, and the host facts it is checked against.
pub struct AgentGrant<'a> {
    pub scopes: &'a [GrantScope],
    pub registry: &'a DelegationRegistry,
    /// The live schema digest of an app's delegation schema.
    pub schema_digest: &'a dyn Fn(&str) -> Option<String>,
    /// The target is a Cloud host worker (host filesystem is not the user's).
    pub host_worker: bool,
    /// No person can answer a confirmation prompt.
    pub unattended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authorized {
    pub record: String,
    pub class: Option<OperationClass>,
    /// The tightest budgets of the covering scopes (agents only).
    pub budgets: Option<ScopeBudgets>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Workspace policy denies the person, the agent, or the delegator.
    NotAuthorized(&'static str),
    /// Not a delegable, explicitly classed operation.
    NotDelegable(&'static str),
    OutsideScope,
    /// The app's delegation schema or package changed since consent.
    SchemaChanged,
    /// Needs an interactive confirmation no one can give unattended.
    NeedsConfirmation,
    /// Over a content-impact, argument or bulk bound: ask the owner.
    NeedsApproval(&'static str),
    /// Step, wall-clock or rate budget exhausted.
    Budget(&'static str),
    /// Execution did not match the operation's class.
    ClassViolation(&'static str),
    InvalidRoute,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAuthorized(why) => write!(f, "not authorized: {why}"),
            Self::NotDelegable(why) => write!(f, "operation is not delegable: {why}"),
            Self::OutsideScope => f.write_str("agent action is outside its immutable task grants"),
            Self::SchemaChanged => f.write_str(
                "application delegation schema changed since consent; re-consent required",
            ),
            Self::NeedsConfirmation => {
                f.write_str("enrolled task action requires interactive confirmation")
            }
            Self::NeedsApproval(why) => write!(f, "owner approval required: {why}"),
            Self::Budget(why) => write!(f, "agent task budget exhausted: {why}"),
            Self::ClassViolation(why) => write!(f, "operation violated its class: {why}"),
            Self::InvalidRoute => f.write_str("reserved or invalid route record"),
        }
    }
}
impl std::error::Error for Refusal {}

/// The shared per-step authorization (see module docs). `grant` must be
/// `Some` exactly for an agent actor.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn authorize_operation(
    policy: &dyn OperationPolicy,
    actor: InvokingActor<'_>,
    grant: Option<&AgentGrant<'_>>,
    op: &OperationDescriptor<'_>,
) -> Result<Authorized, Refusal> {
    let record = canonical_record(op.route_record).map_err(|_| Refusal::InvalidRoute)?;
    let action_object = PolicyObject::Action {
        app: op.app_id,
        action: op.action,
    };
    let record_object = PolicyObject::Record {
        app: op.app_id,
        route_record: op.route_record,
    };
    let (agent, delegator, grant) = match (actor, grant) {
        (InvokingActor::Person { id }, None) => {
            if op.permission != PermissionLevel::None
                && !(policy.person_allows(id, action_object, op.permission)
                    && policy.person_allows(id, record_object, op.permission))
            {
                return Err(Refusal::NotAuthorized(
                    "caller no longer authorized for invocation",
                ));
            }
            return Ok(Authorized {
                record,
                class: None,
                budgets: None,
            });
        }
        (InvokingActor::Agent { agent, delegator }, Some(grant)) => (agent, delegator, grant),
        _ => return Err(Refusal::NotAuthorized("actor and grant disagree")),
    };

    let entry = grant.registry.entry(op.app_id, op.resource_kind, op.action);
    let class = grant
        .registry
        .classify(op.app_id, op.resource_kind, op.action, op.declared);
    if entry.is_some() && class.is_none() {
        return Err(Refusal::NotDelegable(
            "declared kind disagrees with the delegation manifest",
        ));
    }
    if grant.host_worker && class.is_some_and(|c| !c.delegable_on_host_worker()) {
        return Err(Refusal::NotDelegable(
            "host filesystem effects are never delegable",
        ));
    }
    if grant.unattended && op.confirmation_required {
        return Err(Refusal::NeedsConfirmation);
    }
    if grant.unattended && entry.is_some_and(|e| e.bulk) {
        return Err(Refusal::NeedsApproval(
            "bulk operations are never run unattended",
        ));
    }
    let required = op.permission.grant_verb()?;
    let target = ScopeTarget {
        app_id: op.app_id,
        record_id: &record,
        resource: op.resource,
        action: op.action,
        class,
        required,
    };
    let mut covering: Vec<&GrantScope> =
        grant.scopes.iter().filter(|s| s.covers(&target)).collect();
    if covering.is_empty() {
        return Err(Refusal::OutsideScope);
    }
    // A v2 scope stays bound to the package and schema it was consented for.
    let live_digest = (grant.schema_digest)(op.app_id);
    let live_package = grant.registry.package_for_app(op.app_id);
    covering.retain(|scope| match &scope.package {
        None => scope.is_degenerate(),
        Some(pin) => {
            live_digest.as_deref() == Some(pin.schema_digest.as_str())
                && live_package == Some(pin.package_id.as_str())
        }
    });
    if covering.is_empty() {
        return Err(Refusal::SchemaChanged);
    }
    let budgets = covering
        .iter()
        .map(|s| s.budgets())
        .reduce(ScopeBudgets::tightest)
        .unwrap_or_default();
    check_arguments(entry, op, &budgets)?;
    let delegated = covering
        .iter()
        .map(|s| s.verb)
        .max()
        .unwrap_or(GrantVerb::Read);
    check_principals(policy, agent, delegator, op, delegated)?;
    Ok(Authorized {
        record,
        class,
        budgets: Some(budgets),
    })
}

/// Argument bounds of a delegated operation (T5): a manifest count field
/// (`delete {count}`) must parse and stay within the task's budget.
fn check_arguments(
    entry: Option<&crate::DelegableOp>,
    op: &OperationDescriptor<'_>,
    budgets: &ScopeBudgets,
) -> Result<(), Refusal> {
    let Some(field) = entry.and_then(|e| e.count_param.as_deref()) else {
        return Ok(());
    };
    let count = op
        .argument
        .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
        .and_then(|v| v.get(field).and_then(serde_json::Value::as_u64));
    match count {
        Some(count) if count <= u64::from(budgets.max_delete_count) => {}
        Some(_) => return Err(Refusal::NeedsApproval("delete count over budget")),
        None if op.argument.is_none() => {}
        None => return Err(Refusal::NeedsApproval("unbounded delete count")),
    }
    Ok(())
}

/// T21: the agent principal (its explicit role, or the delegated verb when it
/// has none) and the delegating owner must both allow the operation, on the
/// Action and on the route record's Artifact.
fn check_principals(
    policy: &dyn OperationPolicy,
    agent: &str,
    delegator: &str,
    op: &OperationDescriptor<'_>,
    delegated: GrantVerb,
) -> Result<(), Refusal> {
    let action_object = PolicyObject::Action {
        app: op.app_id,
        action: op.action,
    };
    let record_object = PolicyObject::Record {
        app: op.app_id,
        route_record: op.route_record,
    };
    let ceiling = match policy.agent_standing(agent) {
        AgentStanding::Denied => {
            return Err(Refusal::NotAuthorized("agent is blocked in this workspace"));
        }
        AgentStanding::Member(ceiling) => ceiling,
        // No roster entry of its own: the agent is a participant with its
        // spawner's current member rights, capped by its frozen scope.
        AgentStanding::Absent => PermissionLevel::from_verb(delegated).min(
            policy
                .person_ceiling(delegator)
                .ok_or(Refusal::NotAuthorized("delegating owner is not a member"))?,
        ),
    };
    let verb = op.permission.max(PermissionLevel::Read);
    let policy_objects: &[PolicyObject<'_>] = if op.permission == PermissionLevel::None {
        &[record_object]
    } else {
        &[action_object, record_object]
    };
    for object in policy_objects {
        let object_verb = if matches!(object, PolicyObject::Record { .. }) {
            verb
        } else {
            op.permission
        };
        if ceiling < object_verb || !policy.agent_allows(agent, ceiling, *object, object_verb) {
            return Err(Refusal::NotAuthorized("agent principal denied"));
        }
        if !policy.person_allows(delegator, *object, object_verb) {
            return Err(Refusal::NotAuthorized("delegating owner denied"));
        }
    }
    Ok(())
}

/// Step and wall-clock budget of one task (MUST-9), checked by the host from
/// its durable step ledger and the task's acceptance time.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn check_step_budget(
    budgets: &ScopeBudgets,
    steps_bound: u32,
    elapsed_ms: u64,
) -> Result<(), Refusal> {
    if steps_bound >= budgets.max_steps {
        return Err(Refusal::Budget("step budget"));
    }
    if elapsed_ms > u64::from(budgets.wall_clock_secs) * 1_000 {
        return Err(Refusal::Budget("wall-clock budget"));
    }
    Ok(())
}

/// Token bucket for invocations per task (burst and sustained, MUST-9).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimiter {
    tokens_milli: u64,
    last_ms: u64,
}
impl RateLimiter {
    pub fn new(budgets: &ScopeBudgets, now_ms: u64) -> Self {
        Self {
            tokens_milli: u64::from(budgets.invocation_burst) * 1_000,
            last_ms: now_ms,
        }
    }
    /// Take one token, or refuse without consuming.
    ///
    /// # Errors
    ///
    /// Returns the refusal or validation error that fails the check.
    pub fn admit(&mut self, budgets: &ScopeBudgets, now_ms: u64) -> Result<(), Refusal> {
        let capacity = u64::from(budgets.invocation_burst) * 1_000;
        let elapsed = now_ms.saturating_sub(self.last_ms);
        self.tokens_milli = self
            .tokens_milli
            .saturating_add(elapsed.saturating_mul(u64::from(budgets.invocations_per_sec)))
            .min(capacity);
        self.last_ms = self.last_ms.max(now_ms);
        self.tokens_milli = self
            .tokens_milli
            .checked_sub(1_000)
            .ok_or(Refusal::Budget("invocation rate"))?;
        Ok(())
    }
}

/// Removal floor per task, so a tiny document can still be corrected.
pub const MIN_TASK_REMOVAL_BYTES: u64 = 256;

/// Content-impact budget (MUST-8), measured by the host from the applied
/// envelopes: bytes removed this step, removed earlier in the task, and the
/// document size at the task's first step.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn check_removal(
    budgets: &ScopeBudgets,
    removed_this_step: u64,
    removed_before: u64,
    document_bytes_at_start: u64,
) -> Result<(), Refusal> {
    if removed_this_step > u64::from(budgets.removed_bytes_per_step) {
        return Err(Refusal::NeedsApproval(
            "removes too much content in one step",
        ));
    }
    let percent =
        document_bytes_at_start.saturating_mul(u64::from(budgets.removed_percent_per_task)) / 100;
    // A near-empty document may still have its few bytes corrected.
    let task_limit =
        u64::from(budgets.removed_bytes_per_task).min(percent.max(MIN_TASK_REMOVAL_BYTES));
    if removed_before.saturating_add(removed_this_step) > task_limit {
        return Err(Refusal::NeedsApproval(
            "removes too much content in this task",
        ));
    }
    Ok(())
}

/// What the host observed when one operation executed (MUST-5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionFacts {
    /// Registered document envelopes of the target app it produced.
    pub app_envelopes: usize,
    /// Any envelope of another type or app.
    pub foreign_envelopes: bool,
    /// Any job, filesystem, network or launch request.
    pub side_effects: bool,
    /// It changed some view other than the invoker's own.
    pub foreign_view_change: bool,
}
/// Refuse (and log at the caller) an execution that does not match its class.
///
/// # Errors
///
/// Returns the refusal or validation error that fails the check.
pub fn check_execution(class: OperationClass, facts: &ExecutionFacts) -> Result<(), Refusal> {
    if facts.foreign_envelopes {
        return Err(Refusal::ClassViolation("foreign envelope"));
    }
    if facts.foreign_view_change {
        return Err(Refusal::ClassViolation(
            "changed another participant's view",
        ));
    }
    match class {
        OperationClass::Document | OperationClass::Disclose
            if facts.app_envelopes != 1 || facts.side_effects =>
        {
            return Err(Refusal::ClassViolation(
                "document op must yield exactly one envelope and no effect",
            ));
        }
        OperationClass::View | OperationClass::Query
            if facts.app_envelopes != 0 || facts.side_effects =>
        {
            return Err(Refusal::ClassViolation(
                "view or query op must not commit or cause effects",
            ));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DelegationRegistry, PackagePin, RecordScope, ResourceScope, WRITER_APP_ID,
        WRITER_PACKAGE_ID, scopes_from_v1,
    };
    use std::collections::{HashMap, HashSet};

    /// A tiny workspace: person roles by id, agent standing, per-record denies.
    #[derive(Default)]
    struct Policy {
        people: HashMap<&'static str, PermissionLevel>,
        agent: Option<AgentStanding>,
        denied_records: HashSet<&'static str>,
        denied_actions: HashSet<&'static str>,
    }
    impl Policy {
        fn object_ok(&self, object: PolicyObject<'_>) -> bool {
            match object {
                PolicyObject::Record { route_record, .. } => {
                    !self.denied_records.contains(route_record)
                }
                PolicyObject::Action { action, .. } => !self.denied_actions.contains(action),
            }
        }
    }
    impl OperationPolicy for Policy {
        fn person_allows(
            &self,
            person: &str,
            object: PolicyObject<'_>,
            verb: PermissionLevel,
        ) -> bool {
            self.people.get(person).is_some_and(|role| *role >= verb) && self.object_ok(object)
        }
        fn person_ceiling(&self, person: &str) -> Option<PermissionLevel> {
            self.people.get(person).copied()
        }
        fn agent_standing(&self, _: &str) -> AgentStanding {
            self.agent.unwrap_or(AgentStanding::Absent)
        }
        fn agent_allows(
            &self,
            _: &str,
            ceiling: PermissionLevel,
            object: PolicyObject<'_>,
            verb: PermissionLevel,
        ) -> bool {
            ceiling >= verb && self.object_ok(object)
        }
    }
    fn policy() -> Policy {
        Policy {
            people: HashMap::from([
                ("owner", PermissionLevel::Admin),
                ("editor", PermissionLevel::Write),
                ("viewer", PermissionLevel::Read),
            ]),
            ..Policy::default()
        }
    }
    fn registry() -> DelegationRegistry {
        DelegationRegistry::first_party()
    }
    fn digest_of(app: &str) -> Option<String> {
        let bytes = registry().schema_bytes(app)?;
        // Any deterministic digest works for the pure tests.
        let mut acc = [0u8; 32];
        for (i, b) in bytes.iter().enumerate() {
            acc[i % 32] = acc[i % 32].wrapping_mul(31).wrapping_add(*b);
        }
        Some(acc.iter().fold(String::new(), |mut hex, b| {
            use std::fmt::Write;
            let _ = write!(hex, "{b:02x}");
            hex
        }))
    }
    fn edit_scope(record: &str) -> GrantScope {
        GrantScope {
            app_id: WRITER_APP_ID.into(),
            record: RecordScope::Exact(record.into()),
            verb: GrantVerb::Write,
            classes: vec![
                OperationClass::Document,
                OperationClass::View,
                OperationClass::Query,
            ],
            resource: ResourceScope::Any,
            actions: None,
            deny_actions: vec![],
            package: Some(PackagePin {
                package_id: WRITER_PACKAGE_ID.into(),
                version: crate::FIRST_PARTY_DELEGATION_VERSION.into(),
                schema_digest: digest_of(WRITER_APP_ID).unwrap(),
            }),
            budgets: None,
            explicit_workspace_consent: false,
        }
    }
    fn op(
        action: &str,
        declared: DeclaredKind,
        permission: PermissionLevel,
    ) -> OperationDescriptor<'_> {
        OperationDescriptor {
            app_id: WRITER_APP_ID,
            route_record: "rec-1",
            resource: "word-processor.document",
            resource_kind: "word-processor.document",
            action,
            declared,
            permission,
            confirmation_required: false,
            argument: None,
        }
    }
    const AGENT: InvokingActor<'static> = InvokingActor::Agent {
        agent: "agent-1",
        delegator: "owner",
    };
    fn check(
        policy: &Policy,
        actor: InvokingActor<'_>,
        scopes: Option<&[GrantScope]>,
        op: &OperationDescriptor<'_>,
    ) -> Result<Authorized, Refusal> {
        let registry = registry();
        let digest = |app: &str| digest_of(app);
        let grant = scopes.map(|scopes| AgentGrant {
            scopes,
            registry: &registry,
            schema_digest: &digest,
            host_worker: true,
            unattended: true,
        });
        authorize_operation(policy, actor, grant.as_ref(), op)
    }

    #[test]
    fn write_scope_on_record_allows_typing_and_selection() {
        let scopes = [edit_scope("rec-1")];
        let p = policy();
        for (action, declared, permission) in [
            ("type-text", DeclaredKind::Document, PermissionLevel::Write),
            ("select-range", DeclaredKind::View, PermissionLevel::Read),
            ("move-caret", DeclaredKind::View, PermissionLevel::None),
            ("read-text", DeclaredKind::Effect, PermissionLevel::Read),
        ] {
            let ok = check(&p, AGENT, Some(&scopes), &op(action, declared, permission)).unwrap();
            assert_eq!(ok.record, "rec-1");
            assert!(ok.class.is_some(), "{action}");
            assert_eq!(ok.budgets, Some(ScopeBudgets::default()));
        }
    }

    #[test]
    fn scope_denies_effects_other_records_admin_and_unlisted_ops() {
        let scopes = [edit_scope("rec-1")];
        let p = policy();
        let refused = |o: OperationDescriptor<'_>| check(&p, AGENT, Some(&scopes), &o).unwrap_err();
        // Effect (save/open), even when Writer mislabels them Document.
        assert!(matches!(
            refused(op("save", DeclaredKind::Effect, PermissionLevel::Write)),
            Refusal::NotDelegable(_)
        ));
        assert!(matches!(
            refused(op("open", DeclaredKind::Effect, PermissionLevel::Read)),
            Refusal::NotDelegable(_)
        ));
        assert!(matches!(
            refused(op("save", DeclaredKind::Document, PermissionLevel::Write)),
            Refusal::NotDelegable(_)
        ));
        // Another record: the route decides the record, not the resource name.
        let mut other = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        other.route_record = "rec-2";
        other.resource = "word-processor.document:rec-1";
        assert_eq!(refused(other), Refusal::OutsideScope);
        // Admin op.
        assert!(matches!(
            refused(op(
                "type-text",
                DeclaredKind::Document,
                PermissionLevel::Admin
            )),
            Refusal::NotDelegable(_)
        ));
        // Unlisted op, and a Chat op under a Writer scope.
        assert_eq!(
            refused(op(
                "delete-style",
                DeclaredKind::Document,
                PermissionLevel::Write
            )),
            Refusal::OutsideScope
        );
        let mut chat = op("post", DeclaredKind::Document, PermissionLevel::Write);
        chat.app_id = crate::CHAT_APP_ID;
        chat.resource_kind = "chat.channel";
        assert_eq!(refused(chat), Refusal::OutsideScope);
        // Reserved route spelling.
        let mut reserved = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        reserved.route_record = "@qos:package-primary";
        assert_eq!(refused(reserved), Refusal::InvalidRoute);
    }

    #[test]
    fn confirmation_bulk_and_delete_count_are_refused_unattended() {
        let scopes = [edit_scope("rec-1")];
        let p = policy();
        let mut confirm = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        confirm.confirmation_required = true;
        assert_eq!(
            check(&p, AGENT, Some(&scopes), &confirm),
            Err(Refusal::NeedsConfirmation)
        );
        assert!(matches!(
            check(
                &p,
                AGENT,
                Some(&scopes),
                &op("accept-all", DeclaredKind::Document, PermissionLevel::Write)
            ),
            Err(Refusal::NeedsApproval(_))
        ));
        let mut erase = op("erase-text", DeclaredKind::Document, PermissionLevel::Write);
        erase.argument = Some(r#"{"direction":"backward","count":256}"#);
        assert!(check(&p, AGENT, Some(&scopes), &erase).is_ok());
        erase.argument = Some(r#"{"direction":"backward","count":1000000}"#);
        assert!(matches!(
            check(&p, AGENT, Some(&scopes), &erase),
            Err(Refusal::NeedsApproval(_))
        ));
        erase.argument = Some("not json");
        assert!(matches!(
            check(&p, AGENT, Some(&scopes), &erase),
            Err(Refusal::NeedsApproval(_))
        ));
    }

    #[test]
    fn agent_role_and_owner_are_both_checked() {
        let scopes = [edit_scope("rec-1")];
        let typing = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        // Agent set to Commenter, scope Write → typing refused, reading allowed.
        let mut p = policy();
        p.agent = Some(AgentStanding::Member(PermissionLevel::Comment));
        assert_eq!(
            check(&p, AGENT, Some(&scopes), &typing),
            Err(Refusal::NotAuthorized("agent principal denied"))
        );
        assert!(
            check(
                &p,
                AGENT,
                Some(&scopes),
                &op("read-text", DeclaredKind::Effect, PermissionLevel::Read)
            )
            .is_ok()
        );
        p.agent = Some(AgentStanding::Denied);
        assert!(
            check(
                &p,
                AGENT,
                Some(&scopes),
                &op("read-text", DeclaredKind::Effect, PermissionLevel::Read)
            )
            .is_err()
        );
        // Owner downgraded (e.g. revoked workspace Write) → next step refused.
        let mut p = policy();
        p.people.insert("owner", PermissionLevel::Read);
        assert!(matches!(
            check(&p, AGENT, Some(&scopes), &typing),
            Err(Refusal::NotAuthorized(_))
        ));
        // A per-record restriction on the owner binds the agent too.
        let mut p = policy();
        p.denied_records.insert("rec-1");
        assert!(check(&p, AGENT, Some(&scopes), &typing).is_err());
    }

    #[test]
    fn agent_without_roster_entry_acts_with_spawner_member_rights() {
        let scopes = [edit_scope("rec-1")];
        let typing = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        let mut p = policy();
        p.people.insert("commenter", PermissionLevel::Comment);
        let agent = InvokingActor::Agent {
            agent: "a",
            delegator: "commenter",
        };
        assert!(check(&p, agent, Some(&scopes), &typing).is_err());
        assert!(
            check(
                &p,
                agent,
                Some(&scopes),
                &op("read-text", DeclaredKind::Effect, PermissionLevel::Read)
            )
            .is_ok()
        );
        let stranger = InvokingActor::Agent {
            agent: "a",
            delegator: "stranger",
        };
        assert!(check(&p, stranger, Some(&scopes), &typing).is_err());
    }

    #[test]
    fn schema_or_package_change_needs_reconsent() {
        let mut stale = edit_scope("rec-1");
        stale.package.as_mut().unwrap().schema_digest = "0".repeat(64);
        let p = policy();
        let typing = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        assert_eq!(
            check(&p, AGENT, Some(&[stale]), &typing),
            Err(Refusal::SchemaChanged)
        );
        let mut moved = edit_scope("rec-1");
        moved.package.as_mut().unwrap().package_id = "dev.tilo.other".into();
        assert_eq!(
            check(&p, AGENT, Some(&[moved]), &typing),
            Err(Refusal::SchemaChanged)
        );
    }

    #[test]
    fn v1_tuples_keep_exactly_todays_allow_set() {
        let v1 = scopes_from_v1(&[crate::GrantAction {
            app_id: WRITER_APP_ID.into(),
            record_id: "rec-1".into(),
            resource: "word-processor.document".into(),
            action: "append-paragraph".into(),
        }]);
        let p = policy();
        assert!(
            check(
                &p,
                AGENT,
                Some(&v1),
                &op(
                    "append-paragraph",
                    DeclaredKind::Document,
                    PermissionLevel::Write
                )
            )
            .is_ok()
        );
        // Not widened to other actions of the same class, record or resource.
        assert_eq!(
            check(
                &p,
                AGENT,
                Some(&v1),
                &op("type-text", DeclaredKind::Document, PermissionLevel::Write)
            ),
            Err(Refusal::OutsideScope)
        );
        let mut section = op(
            "append-paragraph",
            DeclaredKind::Document,
            PermissionLevel::Write,
        );
        section.resource = "word-processor.section:h1";
        assert_eq!(
            check(&p, AGENT, Some(&v1), &section),
            Err(Refusal::OutsideScope)
        );
    }

    #[test]
    fn same_function_for_people_binds_the_record() {
        let p = policy();
        let typing = op("type-text", DeclaredKind::Document, PermissionLevel::Write);
        let editor = InvokingActor::Person { id: "editor" };
        assert!(check(&p, editor, None, &typing).is_ok());
        assert!(check(&p, InvokingActor::Person { id: "viewer" }, None, &typing).is_err());
        assert!(check(&p, InvokingActor::Person { id: "stranger" }, None, &typing).is_err());
        // The person path now checks the record too.
        let mut p = policy();
        p.denied_records.insert("rec-1");
        assert!(check(&p, editor, None, &typing).is_err());
        // The same descriptor through the agent path under an editor delegator.
        let p = policy();
        let agent = InvokingActor::Agent {
            agent: "a",
            delegator: "editor",
        };
        assert!(check(&p, agent, Some(&[edit_scope("rec-1")]), &typing).is_ok());
        let agent = InvokingActor::Agent {
            agent: "a",
            delegator: "viewer",
        };
        assert!(check(&p, agent, Some(&[edit_scope("rec-1")]), &typing).is_err());
        // Mismatched actor and grant.
        assert!(check(&p, editor, Some(&[edit_scope("rec-1")]), &typing).is_err());
        assert!(check(&p, AGENT, None, &typing).is_err());
    }

    #[test]
    fn large_documents_stay_delegable_with_one_scope() {
        // 200 headings publish hundreds of section resources; one scope
        // covers all of them on the same record (the 64-tuple cap is gone).
        let scopes = [edit_scope("rec-1")];
        let p = policy();
        for i in 0..200 {
            let resource = format!("word-processor.section:h{i}");
            let mut align = op("align-left", DeclaredKind::Document, PermissionLevel::Write);
            align.resource = &resource;
            align.resource_kind = "word-processor.section";
            assert!(check(&p, AGENT, Some(&scopes), &align).is_ok());
        }
    }

    #[test]
    fn step_rate_removal_and_execution_budgets() {
        let b = ScopeBudgets::default();
        assert!(check_step_budget(&b, 199, 1_000).is_ok());
        assert!(check_step_budget(&b, 200, 1_000).is_err());
        assert!(check_step_budget(&b, 1, 15 * 60 * 1_000 + 1).is_err());

        let mut rate = RateLimiter::new(&b, 0);
        for _ in 0..20 {
            rate.admit(&b, 0).unwrap();
        }
        assert!(rate.admit(&b, 0).is_err());
        assert!(rate.admit(&b, 100).is_err());
        rate.admit(&b, 200).unwrap(); // 5/s sustained: one token per 200 ms
        assert!(rate.admit(&b, 200).is_err());

        // 10 KB document: select-all + type-text "" removes it all → refused.
        assert!(check_removal(&b, 10_240, 0, 10_240).is_err());
        assert!(check_removal(&b, 512, 0, 10_240).is_ok());
        // Per task: min(8 KiB, 10 % of 10 KB = 1024).
        assert!(check_removal(&b, 600, 500, 10_240).is_err());
        assert!(check_removal(&b, 20, 0, 20).is_ok());

        let one = ExecutionFacts {
            app_envelopes: 1,
            ..ExecutionFacts::default()
        };
        assert!(check_execution(OperationClass::Document, &one).is_ok());
        assert!(check_execution(OperationClass::Query, &one).is_err());
        assert!(check_execution(OperationClass::View, &ExecutionFacts::default()).is_ok());
        let fs = ExecutionFacts {
            side_effects: true,
            ..one
        };
        assert!(check_execution(OperationClass::Document, &fs).is_err());
        let two = ExecutionFacts {
            app_envelopes: 2,
            ..one
        };
        assert!(check_execution(OperationClass::Document, &two).is_err());
        let foreign = ExecutionFacts {
            foreign_view_change: true,
            ..ExecutionFacts::default()
        };
        assert!(check_execution(OperationClass::View, &foreign).is_err());
    }
}
