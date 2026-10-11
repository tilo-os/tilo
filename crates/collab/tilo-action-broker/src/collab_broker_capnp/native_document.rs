//! Scoped native publication retains originals privately through the held owner.
use super::{CapnpChannelHandle, published_invocation::OriginalInvocation};
use std::sync::Arc;
use tilo_peer_state::owner_commit::{CommitOperation, DocumentCommitCustody, OwnerCommitRequest};
use ui::invocation_scope::{
    DocumentAuthorization, NativePublicationClaims, PublishedInvocationLease,
};

pub(super) struct NativeDocumentCustody {
    authorization: DocumentAuthorization,
    route: super::CrdtRoute,
    parser: tilo_automerge_isolate::Isolate,
}

impl NativeDocumentCustody {
    pub(super) fn retain(channel: &CapnpChannelHandle, bytes: &[u8]) -> Result<Arc<Self>, String> {
        let claims = NativePublicationClaims::decode(bytes)?;
        let route = channel.route();
        let sdk_route = ui::collaboration::RouteSpec {
            app_id: route.app_id.clone(),
            cluster_id: route
                .cluster_id
                .parse()
                .map_err(|_| "invalid scoped native room")?,
            record: (!route.record_id.is_empty()).then(|| route.record_id.clone()),
        };
        let authorization =
            DocumentAuthorization::from_native_publication(sdk_route.clone(), claims, |claim| {
                let original = OriginalInvocation::from_retained_delivery(
                    channel,
                    claim.wire_id,
                    tilo_actions::InvocationId(uuid::Uuid::from_bytes(claim.invocation)),
                )?;
                let current = original.clone();
                PublishedInvocationLease::new(
                    original.request().clone(),
                    sdk_route.clone(),
                    Arc::new(move || current.check_lifecycle()),
                )
                .with_dispatch_spec(original.dispatch_spec().clone())?
                .with_owner_token(original)
            })?;
        Ok(Arc::new(Self {
            authorization,
            route,
            parser: tilo_automerge_isolate::Isolate::new(tilo_automerge_isolate::Limits::default()),
        }))
    }
}

impl DocumentCommitCustody for NativeDocumentCustody {
    fn validate_opened(
        &self,
        peer: &tilo_peer_state::StatePeer,
        request: &OwnerCommitRequest,
        opened: &[u8],
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        let CommitOperation::Document { app, record, .. } = &request.operation else {
            return Err("scoped native custody requires an actual document".into());
        };
        if request.room != peer.room()
            || request.room != self.route.cluster_id
            || app != &self.route.app_id
            || record != &self.route.record_id
        {
            return Err("scoped native owner route differs from retained originals".into());
        }
        let collab_types::DocumentFrame::Whole(mutation) =
            collab_types::DocumentFrame::decode(opened).map_err(|error| error.to_string())?
        else {
            return Err("scoped native mutation requires complete bounded opening".into());
        };
        let route = ui::collaboration::RouteSpec {
            app_id: app.clone(),
            cluster_id: request.room.parse().map_err(|_| "invalid native room")?,
            record: (!record.is_empty()).then(|| record.clone()),
        };
        self.authorization.check_binding(&route, &mutation)?;
        let actual = self
            .parser
            .change_hashes(mutation.data())
            .map_err(|error| error.to_string())?;
        let committed = peer
            .document(app, record)
            .map(|bytes| self.parser.change_hashes(&bytes))
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        self.authorization
            .validate_owner_base(&actual, |hash| committed.contains(hash))?;
        self.authorization
            .validate_owner_contributors(|request, retained_route, token| {
                let original = token
                    .downcast_ref::<OriginalInvocation>()
                    .ok_or("scoped native contributor has no broker-private original custody")?;
                if original.request() != request
                    || retained_route != &route
                    || original.route() != &self.route
                {
                    return Err("scoped native contributor original inputs or route changed".into());
                }
                validate_original(peer, original, now)
            })
    }
}

fn validate_original(
    peer: &tilo_peer_state::StatePeer,
    original: &OriginalInvocation,
    now: std::time::SystemTime,
) -> Result<(), String> {
    use collab_types::caps::{Actor, CapDecision, Principal, ResourceRef, Verb, decide_capability};
    // This is pure custody and publication checking: no membership callback or
    // owner/Store reopening while the native mutation turn is held.
    let seconds = original.check_custody()? / 1_000_000_000;
    original.check_current_publication()?;
    let certificate = original.certificate()?;
    if !peer
        .certificates()
        .iter()
        .any(|current| current == certificate)
    {
        return Err("original native contributor certificate changed or retired".into());
    }
    let (role, caps, person) = peer.permissions(certificate.device_public_key, now)?;
    let spec = original.dispatch_spec();
    if !matches!(
        spec.kind,
        tilo_actions::OperationKind::Document | tilo_actions::OperationKind::Effect
    ) || !spec
        .contract
        .permission
        .covers(tilo_actions::PermissionRequirement::Write)
        || original.request().address.action == runtime_collab::approved_checks::ACTION_ID
    {
        return Err("original action does not confer document mutation authority".into());
    }
    let mut actor = if let Some(agent) = original.agent_id() {
        if !peer.governed_agents(now)?.iter().any(|current| {
            current.acting
                && current.agent_id == agent
                && current.device_public_key == certificate.device_public_key
                && current.user_public_key == certificate.user_public_key
        }) {
            return Err("original native agent delegation ended or changed".into());
        }
        Actor::agent(agent)
    } else {
        Actor::person(Some(role), person)
    };
    actor = actor.in_room(peer.room().to_owned());
    if collab_types::foreign_device::cap_applies(
        certificate.capabilities.foreign || original.retained_foreign_restriction(),
        certificate.device_public_key == peer.authority().device_public_key,
    ) {
        actor = collab_types::foreign_device::on_foreign_device(actor, &caps, seconds);
    }
    let action = ResourceRef::Action {
        app: original.route().app_id.clone(),
        resource: original.request().address.resource.clone(),
        action: original.request().address.action.clone(),
    };
    let required = super::permission_verb(spec.contract.permission)
        .ok_or("original document contract has no mutation permission")?;
    let document = ResourceRef::Artifact {
        app: Some(original.route().app_id.clone()),
        kind: collab_types::ArtifactKind::Note,
        instance: collab_types::document_artifact_id(
            &original.route().app_id,
            peer.room(),
            &original.route().record_id,
        ),
    };
    if decide_capability(&actor, &action, required, &caps, Some(seconds)) != CapDecision::Allow
        || decide_capability(
            &actor.with_principal(Principal::App(original.route().app_id.clone())),
            &document,
            Verb::Write,
            &caps,
            Some(seconds),
        ) != CapDecision::Allow
    {
        return Err(
            "original native contributor is no longer authorized for this document action".into(),
        );
    }
    original.check_custody()?;
    original.check_current_publication()
}
