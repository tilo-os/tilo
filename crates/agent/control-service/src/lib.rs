//! Agent grant admission policy over the current workspace governance.
use tilo_action_broker::policy::WorkspacePolicy;

/// Admission of one frozen scope for the current owner:
/// manifest-delegable app (no hard-coded app list), canonical non-reserved
/// records only, no workspace wildcard without a separate consent path, and
/// the owner holds the scope's verb on the app, on each exact action and on
/// each record's document artifact.
pub fn admissible_scope(
    workspace: &tilo_desktop_model::Workspace,
    actor: &collab_types::caps::Actor,
    room: &str,
    registry: &tilo_agent_control_protocol::DelegationRegistry,
    scope: &tilo_agent_control_protocol::GrantScope,
) -> bool {
    use collab_types::caps::{ResourceRef, Verb};
    use tilo_agent_control_protocol::{GrantVerb, RecordScope, route_record};
    if !registry.is_delegable_app(&scope.app_id) || matches!(scope.record, RecordScope::InWorkspace)
    {
        return false;
    }
    let verb = match scope.verb {
        GrantVerb::Read => Verb::Read,
        GrantVerb::Comment => Verb::Comment,
        GrantVerb::Write => Verb::Write,
    };
    let mut resources = vec![];
    match &scope.actions {
        Some(actions) => resources.extend(actions.iter().map(|action| ResourceRef::Action {
            app: scope.app_id.clone(),
            action: action.clone(),
        })),
        None => resources.push(ResourceRef::App(scope.app_id.clone())),
    }
    for record in scope.records() {
        let Ok(record) = route_record(record) else {
            return false;
        };
        resources.push(ResourceRef::Artifact {
            app: Some(scope.app_id.clone()),
            kind: collab_types::ArtifactKind::Note,
            instance: collab_types::document_artifact_id(&scope.app_id, room, record),
        });
    }
    resources
        .iter()
        .all(|resource| workspace.allows(actor, resource, verb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{CapOverride, Subject, Verb};
    use tilo_agent_control_protocol::{
        DelegationRegistry, GrantScope, GrantVerb, OperationClass, PackagePin, RecordScope,
        ResourceScope,
    };

    fn scope(app: &str, record: RecordScope) -> GrantScope {
        GrantScope {
            app_id: app.into(),
            record,
            verb: GrantVerb::Write,
            classes: vec![OperationClass::Document, OperationClass::View],
            resource: ResourceScope::Any,
            actions: None,
            deny_actions: vec![],
            package: Some(PackagePin {
                package_id: "so.tilo.word-processor".into(),
                version: "1".into(),
                schema_digest: "a".repeat(64),
            }),
            budgets: None,
            explicit_workspace_consent: false,
        }
    }

    #[test]
    fn admission_is_manifest_driven_single_record_and_owner_checked() {
        let mut workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(3), "Room");
        workspace.set_owner("owner".to_string());
        let room = workspace.room.to_string();
        let owner = workspace.actor_for_person(&"owner".to_string());
        let registry = DelegationRegistry::first_party();
        let writer = tilo_agent_control_protocol::WRITER_APP_ID;
        assert!(admissible_scope(
            &workspace,
            &owner,
            &room,
            &registry,
            &scope(writer, RecordScope::Exact("rec-1".into()))
        ));
        // Not in a trusted delegation manifest (no hard-coded app list).
        assert!(!admissible_scope(
            &workspace,
            &owner,
            &room,
            &registry,
            &scope("so.tilo.calculator", RecordScope::Exact("rec-1".into()))
        ));
        assert!(!admissible_scope(
            &workspace,
            &owner,
            &room,
            &DelegationRegistry::empty(),
            &scope(writer, RecordScope::Exact("rec-1".into()))
        ));
        // A workspace wildcard never enters through this admission path.
        let mut wildcard = scope(writer, RecordScope::InWorkspace);
        wildcard.explicit_workspace_consent = true;
        assert!(!admissible_scope(
            &workspace, &owner, &room, &registry, &wildcard
        ));
        // A viewer cannot delegate Write.
        workspace.grant_capability(CapOverride::membership(
            Subject::person("viewer"),
            room.clone(),
            Verb::Read,
        ));
        let viewer = workspace.actor_for_person(&"viewer".to_string());
        assert!(!admissible_scope(
            &workspace,
            &viewer,
            &room,
            &registry,
            &scope(writer, RecordScope::Exact("rec-1".into()))
        ));
    }
}
