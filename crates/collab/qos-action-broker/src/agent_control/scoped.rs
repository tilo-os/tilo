//! Broker side of scoped agent grants (security review MUST-1..9). The
//! host-set invoker (MUST-11) is lane C's `enrolled_invocation::host_set_invoker`.
//!
//! One `authorize` for people and agents over the live, registered route:
//! [`authorize_published`] builds the protocol's [`OperationDescriptor`] from
//! a live [`PublishedSurface`] and evaluates [`authorize_operation`] against
//! the current workspace policy ([`WorkspaceOperationPolicy`]), for the
//! agent principal (`role_of_agent_at`) *and* the delegating owner.
//!
//! Scoped (v2) grants stay off until [`scoped_grants_enabled`]; v1 exact
//! tuples go through the same functions as degenerate scopes.
use collab_types::caps::{Actor, CapDecision, Principal, ResourceRef, Verb};
use collab_types::role::Role as CapRole;
use qos_agent_control_protocol::{
    AgentGrant, AgentStanding, Authorized, DeclaredKind, DelegationRegistry, GrantAction,
    GrantScope, InvokingActor, OperationDescriptor, OperationPolicy, PermissionLevel, PolicyObject,
    Refusal, authorize_operation, scopes_from_grants,
};
use std::sync::OnceLock;

use crate::collab_broker_capnp::{PublishedAction, PublishedSurface};
use crate::policy::WorkspacePolicy;

/// Environment switch for v2 scoped grants. On unless exactly `0` (an
/// explicit rollback to v1 exact tuples).
pub const SCOPED_GRANTS_ENV: &str = "QOS_AGENT_SCOPED_GRANTS";
/// Host setting for the default agent scope: `narrow` publishes the security
/// review's single-record edit scopes; anything else (default) publishes the
/// spawner's member rights on every live delegable record.
pub const GRANT_SCOPE_ENV: &str = "QOS_AGENT_GRANT_SCOPE";

/// Scoped (v2) grants are the default.
pub fn scoped_grants_enabled() -> bool {
    std::env::var(SCOPED_GRANTS_ENV).map_or(true, |value| value != "0")
}
/// Whether the host publishes the narrow per-record edit scopes instead of
/// the spawner's member rights.
pub fn narrow_grant_scope() -> bool {
    std::env::var(GRANT_SCOPE_ENV).is_ok_and(|value| value == "narrow")
}

/// The host-pinned delegation registry (first-party Writer and Chat).
pub fn delegation_registry() -> &'static DelegationRegistry {
    static REGISTRY: OnceLock<DelegationRegistry> = OnceLock::new();
    REGISTRY.get_or_init(DelegationRegistry::first_party)
}

/// Live schema digest of one app's delegation schema (MUST-6).
pub fn schema_digest(app_id: &str) -> Option<String> {
    delegation_registry()
        .schema_bytes(app_id)
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
}

pub fn declared_kind(kind: tilo_actions::OperationKind) -> DeclaredKind {
    match kind {
        tilo_actions::OperationKind::Document => DeclaredKind::Document,
        tilo_actions::OperationKind::View => DeclaredKind::View,
        tilo_actions::OperationKind::Effect => DeclaredKind::Effect,
    }
}
pub fn permission_level(permission: tilo_actions::PermissionRequirement) -> PermissionLevel {
    use tilo_actions::PermissionRequirement as P;
    match permission {
        P::None => PermissionLevel::None,
        P::Read => PermissionLevel::Read,
        P::Comment => PermissionLevel::Comment,
        P::Write => PermissionLevel::Write,
        P::Invite => PermissionLevel::Invite,
        P::Admin => PermissionLevel::Admin,
    }
}
fn verb(level: PermissionLevel) -> Option<Verb> {
    match level {
        PermissionLevel::None => None,
        PermissionLevel::Read => Some(Verb::Read),
        PermissionLevel::Comment => Some(Verb::Comment),
        PermissionLevel::Write => Some(Verb::Write),
        PermissionLevel::Invite => Some(Verb::Invite),
        PermissionLevel::Admin => Some(Verb::Admin),
    }
}
fn role_for_ceiling(level: PermissionLevel) -> Option<CapRole> {
    match level {
        PermissionLevel::None => None,
        PermissionLevel::Read | PermissionLevel::Invite => Some(CapRole::Viewer),
        PermissionLevel::Comment => Some(CapRole::Commenter),
        PermissionLevel::Write => Some(CapRole::Editor),
        PermissionLevel::Admin => Some(CapRole::Owner),
    }
}
fn ceiling_of(role: CapRole) -> Option<PermissionLevel> {
    match role {
        CapRole::Owner => Some(PermissionLevel::Admin),
        CapRole::Editor => Some(PermissionLevel::Write),
        CapRole::Commenter => Some(PermissionLevel::Comment),
        CapRole::Viewer => Some(PermissionLevel::Read),
        CapRole::Denied => None,
    }
}

/// The current workspace policy at one policy time, as the protocol's
/// [`OperationPolicy`]. Artifacts are the app's document artifact of the
/// route record in this workspace.
pub struct WorkspaceOperationPolicy<'a, W: WorkspacePolicy> {
    pub workspace: &'a W,
    pub room: &'a str,
    pub now: u64,
}
impl<W: WorkspacePolicy> WorkspaceOperationPolicy<'_, W> {
    fn resource(&self, object: PolicyObject<'_>) -> ResourceRef {
        match object {
            PolicyObject::Action { app, action } => ResourceRef::Action {
                app: app.into(),
                action: action.into(),
            },
            PolicyObject::Record { app, route_record } => ResourceRef::Artifact {
                app: Some(app.into()),
                kind: collab_types::ArtifactKind::Note,
                instance: collab_types::document_artifact_id(app, self.room, route_record),
            },
        }
    }
}
impl<W: WorkspacePolicy> OperationPolicy for WorkspaceOperationPolicy<'_, W> {
    fn person_allows(
        &self,
        person: &str,
        object: PolicyObject<'_>,
        level: PermissionLevel,
    ) -> bool {
        let Some(verb) = verb(level) else {
            return true;
        };
        let actor = self
            .workspace
            .actor_for_person_at(&person.to_string(), self.now);
        self.workspace
            .decide_at(&actor, &self.resource(object), verb, self.now)
            == CapDecision::Allow
    }
    fn person_ceiling(&self, person: &str) -> Option<PermissionLevel> {
        self.workspace
            .role_of_at(&person.to_string(), self.now)
            .and_then(ceiling_of)
    }
    fn agent_standing(&self, agent: &str) -> AgentStanding {
        match self.workspace.role_of_agent_at(agent, self.now) {
            None => AgentStanding::Absent,
            Some(role) => ceiling_of(role).map_or(AgentStanding::Denied, AgentStanding::Member),
        }
    }
    fn agent_allows(
        &self,
        agent: &str,
        ceiling: PermissionLevel,
        object: PolicyObject<'_>,
        level: PermissionLevel,
    ) -> bool {
        let Some(verb) = verb(level) else {
            return true;
        };
        // The agent principal with its explicit role, or the fixed
        // least-privilege delegated role; agent-specific restrictions apply.
        let mut actor = Actor::agent(agent).in_room(self.room.to_string());
        actor.role = role_for_ceiling(ceiling);
        self.workspace
            .decide_at(&actor, &self.resource(object), verb, self.now)
            == CapDecision::Allow
    }
}

/// The descriptor of one live published operation. The record comes only
/// from the registered route. For an agent (`registered_route`), a surface
/// that is not live or whose app differs from its route's app is refused
/// (T8); the person path keeps today's surface app id.
///
/// # Errors
///
/// Returns the refusal or decoding error that fails the check.
pub fn descriptor<'a>(
    surface: &'a PublishedSurface,
    action: &'a PublishedAction,
    argument: Option<&'a str>,
    registered_route: bool,
) -> Result<OperationDescriptor<'a>, Refusal> {
    if registered_route && (!surface.live || surface.app_id != surface.route.app_id) {
        return Err(Refusal::InvalidRoute);
    }
    Ok(OperationDescriptor {
        app_id: &surface.app_id,
        route_record: &surface.route.record_id,
        resource: &action.resource,
        resource_kind: &action.resource_kind,
        action: &action.action,
        declared: declared_kind(action.operation_kind),
        permission: permission_level(action.contract.permission),
        confirmation_required: !matches!(
            action.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ),
        argument,
    })
}

/// The agent's retained grants as scopes (v1 tuples degenerate).
///
/// # Errors
///
/// Returns the refusal or decoding error that fails the check.
pub fn grant_scopes(grants: &[GrantAction]) -> Result<Vec<GrantScope>, String> {
    scopes_from_grants(grants).map_err(|e| format!("invalid retained agent grants: {e}"))
}

/// The one effective scope set of an enrolled agent task: its frozen grant
/// scopes plus the records this task itself opened with a covered open action
/// ([`qos_agent_control_protocol::scopes_with_opened_records`]). Both the
/// per-step authorization and the enrolled catalogue use exactly this, so the
/// listed operations cannot drift from the authorized ones. `opened` must be
/// the host's durable record of this task's opens (never the agent's claim);
/// empty when the connection has no accepted, live task.
///
/// # Errors
///
/// Returns the decoding error of invalid retained grants.
pub fn task_effective_scopes(
    grants: &[GrantAction],
    opened: &[qos_agent_control_protocol::OpenedRecord],
) -> Result<Vec<GrantScope>, String> {
    Ok(qos_agent_control_protocol::scopes_with_opened_records(
        &grant_scopes(grants)?,
        opened,
        delegation_registry(),
        &schema_digest,
    ))
}

/// The one per-step authorization for a live published operation. `grant`
/// is `Some(scopes)` exactly when `actor` is an agent; agents on this path
/// always run unattended against a host worker.
///
/// # Errors
///
/// Returns the refusal or decoding error that fails the check.
pub fn authorize_published<W: WorkspacePolicy>(
    workspace: &W,
    now: u64,
    actor: InvokingActor<'_>,
    scopes: Option<&[GrantScope]>,
    surface: &PublishedSurface,
    action: &PublishedAction,
    argument: Option<&str>,
) -> Result<Authorized, Refusal> {
    let op = descriptor(surface, action, argument, scopes.is_some())?;
    let policy = WorkspaceOperationPolicy {
        workspace,
        room: &surface.route.cluster_id,
        now,
    };
    let digest = |app: &str| schema_digest(app);
    let grant = scopes.map(|scopes| AgentGrant {
        scopes,
        registry: delegation_registry(),
        schema_digest: &digest,
        host_worker: true,
        unattended: true,
    });
    authorize_operation(&policy, actor, grant.as_ref(), &op)
}

/// The enrolled catalogue for an agent: the owner-policy-filtered catalogue
/// narrowed to operations its scopes cover, with the same class, schema and
/// confirmation rules the step applies. Replaces the exact-tuple filter.
/// Frozen grants only; an enrolled task's catalogue uses
/// [`filter_catalogue_by_scopes`] with its [`task_effective_scopes`].
///
/// # Errors
///
/// Returns the refusal or decoding error that fails the check.
pub fn filter_scoped_catalogue(json: &str, grants: &[GrantAction]) -> Result<String, String> {
    filter_catalogue_by_scopes(json, &grant_scopes(grants)?)
}
/// [`filter_scoped_catalogue`] over already effective `scopes` (an enrolled
/// task's [`task_effective_scopes`], exactly what its steps authorize with).
///
/// # Errors
///
/// Returns the decoding error of an invalid catalogue.
pub fn filter_catalogue_by_scopes(json: &str, scopes: &[GrantScope]) -> Result<String, String> {
    let registry = delegation_registry();
    let mut catalogue: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let surfaces = catalogue
        .get_mut("surfaces")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("invalid native catalogue")?;
    for surface in surfaces.iter_mut() {
        let text = |key: &str| {
            surface
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let app = text("app_id").ok_or("catalogue app missing")?;
        let record = text("record_id").ok_or("catalogue record missing")?;
        let live = surface.get("live").and_then(serde_json::Value::as_bool) == Some(true);
        let actions = surface
            .get_mut("actions")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("catalogue actions missing")?;
        let Ok(canonical) = qos_agent_control_protocol::canonical_record(&record) else {
            actions.clear();
            continue;
        };
        let digest = schema_digest(&app);
        let package = registry.package_for_app(&app);
        actions.retain(|entry| {
            live && catalogue_entry_covered(
                entry,
                &app,
                &canonical,
                scopes,
                registry,
                digest.as_deref(),
                package,
            )
        });
    }
    surfaces.retain(|surface| {
        surface
            .get("actions")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|actions| !actions.is_empty())
    });
    serde_json::to_string(&catalogue).map_err(|e| e.to_string())
}
fn catalogue_entry_covered(
    entry: &serde_json::Value,
    app: &str,
    record: &str,
    scopes: &[GrantScope],
    registry: &DelegationRegistry,
    digest: Option<&str>,
    package: Option<&str>,
) -> bool {
    let field = |key: &str| entry.get(key).and_then(serde_json::Value::as_str);
    let (Some(resource), Some(action)) = (field("resource"), field("action")) else {
        return false;
    };
    let kind = field("resource_kind").unwrap_or_default();
    let Some(declared) = entry
        .get("operation_kind")
        .and_then(|v| serde_json::from_value::<tilo_actions::OperationKind>(v.clone()).ok())
    else {
        return false;
    };
    let Some(contract) = entry
        .get("contract")
        .and_then(|v| serde_json::from_value::<tilo_actions::ActionContract>(v.clone()).ok())
    else {
        return false;
    };
    if !matches!(
        contract.confirmation,
        tilo_actions::ConfirmationPolicy::None
    ) {
        return false;
    }
    let op_entry = registry.entry(app, kind, action);
    let class = registry.classify(app, kind, action, declared_kind(declared));
    if (op_entry.is_some() && class.is_none())
        || op_entry.is_some_and(|e| e.bulk)
        || class.is_some_and(|c| !c.delegable_on_host_worker())
    {
        return false;
    }
    let required = match permission_level(contract.permission) {
        PermissionLevel::None | PermissionLevel::Read => {
            qos_agent_control_protocol::GrantVerb::Read
        }
        PermissionLevel::Comment => qos_agent_control_protocol::GrantVerb::Comment,
        PermissionLevel::Write => qos_agent_control_protocol::GrantVerb::Write,
        PermissionLevel::Invite | PermissionLevel::Admin => return false,
    };
    let target = qos_agent_control_protocol::ScopeTarget {
        app_id: app,
        record_id: record,
        resource,
        action,
        class,
        required,
    };
    scopes.iter().any(|scope| {
        scope.covers(&target)
            && match &scope.package {
                None => scope.is_degenerate(),
                Some(pin) => {
                    digest == Some(pin.schema_digest.as_str())
                        && package == Some(pin.package_id.as_str())
                }
            }
    })
}

/// The admitted owner principal of an enrolled caller (the delegator).
pub fn delegator_principal(caller: &tilo_user_identity::DeviceCert) -> String {
    match Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(
        caller.user_public_key,
    )) {
        Principal::Person(id) => id,
        _ => hex::encode(caller.user_public_key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceId};
    use collab_types::caps::{CapOverride, Effect, Scope, Subject};
    use qos_agent_control_protocol::{
        GrantVerb, OperationClass, PackagePin, RecordScope, ResourceScope, WRITER_APP_ID,
        WRITER_PACKAGE_ID, scope_carrier,
    };

    const OWNER: &str = "owner-person";
    const AGENT: &str = "agent-1";

    fn workspace() -> Workspace {
        let mut w = Workspace::test_default(WorkspaceId(7), "Room");
        w.set_owner(OWNER.to_string());
        w
    }
    const ROOM: &str = "room-7";
    fn surface(record: &str, actions: Vec<PublishedAction>) -> PublishedSurface {
        PublishedSurface {
            peer_id: 9,
            app_id: WRITER_APP_ID.into(),
            route: CrdtRoute::record(WRITER_APP_ID, ROOM, record),
            actions,
            live: true,
        }
    }
    fn action(action: &str, kind: tilo_actions::OperationKind) -> PublishedAction {
        PublishedAction {
            resource: "word-processor.document".into(),
            action: action.into(),
            resource_kind: "word-processor.document".into(),
            resource_label: "Document".into(),
            action_label: action.into(),
            description: None,
            operation_kind: kind,
            params: vec![],
            contract: tilo_actions::ActionContract::default(),
            input: crate::collab_broker_capnp::PublishedInput::None,
        }
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
                version: qos_agent_control_protocol::FIRST_PARTY_DELEGATION_VERSION.into(),
                schema_digest: schema_digest(WRITER_APP_ID).unwrap(),
            }),
            budgets: None,
            explicit_workspace_consent: false,
        }
    }
    const AGENT_ACTOR: InvokingActor<'static> = InvokingActor::Agent {
        agent: AGENT,
        delegator: OWNER,
    };

    #[test]
    fn broker_authorize_checks_agent_role_owner_record_and_scope() {
        let w = workspace();
        let typing = action("type-text", tilo_actions::OperationKind::Document);
        let s = surface("rec-1", vec![typing.clone()]);
        let scopes = [edit_scope("rec-1")];
        // No agent membership: least-privilege delegated role, owner allows.
        assert!(authorize_published(&w, 10, AGENT_ACTOR, Some(&scopes), &s, &typing, None).is_ok());
        // Another record's route.
        let other = surface("rec-2", vec![typing.clone()]);
        assert_eq!(
            authorize_published(&w, 10, AGENT_ACTOR, Some(&scopes), &other, &typing, None),
            Err(Refusal::OutsideScope)
        );
        // Agent set to Commenter by an admin → typing refused (T21).
        let mut commenter = workspace();
        commenter.grant_capability(CapOverride::membership(
            Subject::agent(AGENT),
            commenter.room.to_string(),
            Verb::Comment,
        ));
        assert!(matches!(
            authorize_published(
                &commenter,
                10,
                AGENT_ACTOR,
                Some(&scopes),
                &s,
                &typing,
                None
            ),
            Err(Refusal::NotAuthorized(_))
        ));
        // The owner is not the delegator any more (a stranger delegates).
        let stranger = InvokingActor::Agent {
            agent: AGENT,
            delegator: "stranger",
        };
        assert!(authorize_published(&w, 10, stranger, Some(&scopes), &s, &typing, None).is_err());
        // Surface republished under another app id (T8).
        let mut forged = s.clone();
        forged.app_id = "so.tilo.chat".into();
        assert_eq!(
            authorize_published(&w, 10, AGENT_ACTOR, Some(&scopes), &forged, &typing, None),
            Err(Refusal::InvalidRoute)
        );
    }

    #[test]
    fn broker_human_path_gains_the_record_check() {
        let mut w = workspace();
        let member = "member-person";
        w.grant_capability(CapOverride::membership(
            Subject::person(member),
            w.room.to_string(),
            Verb::Write,
        ));
        let typing = action("type-text", tilo_actions::OperationKind::Document);
        let s = surface("rec-1", vec![typing.clone()]);
        let person = InvokingActor::Person { id: member };
        assert!(authorize_published(&w, 10, person, None, &s, &typing, None).is_ok());
        // Restrict the member on this record's artifact only.
        w.grant_capability(CapOverride::new(
            Subject::person(member),
            Scope::ResourceInstance(collab_types::document_artifact_id(
                WRITER_APP_ID,
                ROOM,
                "rec-1",
            )),
            Effect::Restrict,
            Verb::Read,
        ));
        assert!(authorize_published(&w, 10, person, None, &s, &typing, None).is_err());
        let s2 = surface("rec-2", vec![typing.clone()]);
        assert!(authorize_published(&w, 10, person, None, &s2, &typing, None).is_ok());
    }

    #[test]
    fn scoped_catalogue_lists_only_covered_classified_ops() {
        let ops = [
            ("type-text", "Document"),
            ("move-caret", "View"),
            ("save", "Effect"),
            ("save", "Document"),
            ("accept-all", "Document"),
            ("delete-style", "Document"),
        ];
        let actions: Vec<_> = ops
            .iter()
            .map(|(a, k)| {
                serde_json::json!({"resource":"word-processor.document","action":a,
                "resource_kind":"word-processor.document","operation_kind":k,
                "contract":tilo_actions::ActionContract::default()})
            })
            .collect();
        let json = serde_json::json!({"surfaces":[
            {"app_id":WRITER_APP_ID,"record_id":"rec-1","live":true,"actions":actions},
            {"app_id":WRITER_APP_ID,"record_id":"rec-2","live":true,"actions":actions},
        ]});
        let carrier = scope_carrier(&edit_scope("rec-1")).unwrap();
        let out: serde_json::Value =
            serde_json::from_str(&filter_scoped_catalogue(&json.to_string(), &[carrier]).unwrap())
                .unwrap();
        let surfaces = out["surfaces"].as_array().unwrap();
        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0]["record_id"], "rec-1");
        let names: Vec<_> = surfaces[0]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["type-text", "move-caret"]);
        // v1 tuples keep exact filtering, including the primary alias.
        let v1 = GrantAction {
            app_id: WRITER_APP_ID.into(),
            record_id: "rec-2".into(),
            resource: "word-processor.document".into(),
            action: "delete-style".into(),
        };
        let out: serde_json::Value =
            serde_json::from_str(&filter_scoped_catalogue(&json.to_string(), &[v1]).unwrap())
                .unwrap();
        assert_eq!(out["surfaces"][0]["record_id"], "rec-2");
        assert_eq!(out["surfaces"][0]["actions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn scoped_grants_are_on_by_default() {
        if std::env::var(SCOPED_GRANTS_ENV).is_err() {
            assert!(scoped_grants_enabled());
        }
        if std::env::var(GRANT_SCOPE_ENV).is_err() {
            assert!(!narrow_grant_scope());
        }
    }
}
