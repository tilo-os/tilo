//! Explicit recovery of this profile's old, unscoped content. Published on
//! the existing workspace action door, never on the blob socket. An import
//! creates new room reachability; it does not certify historical ownership.

use std::sync::Arc;

use tilo_action_broker::services::{
    WorkspaceGovernanceOutcome, WorkspaceGovernanceRequest, WorkspaceGovernanceServices,
};
use tilo_actions::remote::Pending;
use tilo_actions::{ActionAddress, ActionContract, ActionDescriptor, ActionParam, ParamType};
use tilo_desktop_model::{RoomId, Workspace};
use tilo_fs::document_media::{
    DocumentMedia, LegacyImportApproval, LegacyImportAuthority, LegacyImportRequest,
    LegacyMediaImportHost, MAX_LEGACY_IMPORT_LIFETIME,
};

use super::workspace::ProfileWorkspace;

#[cfg(test)]
#[path = "media_import/authority_tests.rs"]
mod authority_tests;

pub(super) const RESOURCE: &str = "workspace.legacy-media";
pub(super) const ACTION: &str = "import-legacy-media";
const SOURCE: &str = "runtime-profile-content";

pub(super) fn service(
    workspace: &Arc<ProfileWorkspace>,
    media: &crate::profile::MediaHost,
) -> std::io::Result<Arc<dyn WorkspaceGovernanceServices>> {
    Ok(Arc::new(MediaImport::new(
        Arc::clone(workspace),
        media.store(),
        media.handle()?,
    )))
}

pub(super) struct MediaImport {
    workspace: Arc<ProfileWorkspace>,
    store: Arc<tilo_fs::ContentStore>,
    runtime: tokio::runtime::Handle,
}

impl MediaImport {
    pub(super) fn new(
        workspace: Arc<ProfileWorkspace>,
        store: Arc<tilo_fs::ContentStore>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            workspace,
            store,
            runtime,
        }
    }
}

/// Kept only for the duration of the admitted owner invocation. Neither this
/// object nor its approval can be supplied by JSON or handed to an app.
struct Authority {
    workspace: Arc<ProfileWorkspace>,
    request: LegacyImportRequest,
    approval: LegacyImportApproval,
    device: [u8; 32],
    lease: Option<tilo_actions::remote::ExecutionLease>,
}

fn owner_revision(
    workspace: &impl tilo_identity_host::broker::WorkspaceMembership,
    room: &str,
    account: &str,
) -> anyhow::Result<String> {
    let room = RoomId::parse(room).ok_or_else(|| anyhow::anyhow!("invalid destination room"))?;
    let mut revision = None;
    workspace.with_workspace(&room, &mut |current| {
        if current.owner().map(String::as_str) == Some(account) {
            // Only structural ownership authorizes this action. Task progress,
            // receipts, layout and delegated grants cannot revoke owner custody.
            // Retain completed transfers so an away-and-back transfer also
            // invalidates an approval minted before the transfer.
            revision = serde_json::to_vec(&(
                &current.room,
                current.owner(),
                &current.ownership_transfer_audit,
            ))
            .ok()
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
        }
    });
    revision.ok_or_else(|| anyhow::anyhow!("legacy import requires the current destination owner"))
}

impl LegacyImportAuthority for Authority {
    fn authorize(
        &self,
        request: &LegacyImportRequest,
        approval: &LegacyImportApproval,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.workspace.governs(),
            "legacy import requires the local room owner"
        );
        anyhow::ensure!(
            self.lease
                .as_ref()
                .is_none_or(tilo_actions::remote::ExecutionLease::is_live),
            "legacy import owner execution ended"
        );
        anyhow::ensure!(
            request == &self.request && approval == &self.approval,
            "legacy import custody changed"
        );
        anyhow::ensure!(
            owner_revision(
                self.workspace.as_ref(),
                &request.destination_room,
                &approval.account
            )? == approval.authority_revision,
            "legacy import destination authority changed"
        );
        // Reads current enrollment and fleet revocations, not startup's snapshot.
        let (_, cert) =
            tilo_identity_host::broker::trusted_current_device(&request.destination_room)?;
        let selected =
            tilo_identity_host::registry::selected_identity_profile(&request.destination_room)?
                .ok_or_else(|| anyhow::anyhow!("legacy import identity unavailable"))?;
        anyhow::ensure!(
            cert.device_public_key == self.device && selected.principal_id() == approval.account,
            "legacy import enrollment changed"
        );
        Ok(())
    }
}

impl WorkspaceGovernanceServices for MediaImport {
    fn app_id(&self) -> &'static str {
        "so.tilo.workspace"
    }

    fn handles(&self, action: &str) -> bool {
        action == ACTION
    }

    fn requires_descriptor_admission(&self, action: &str) -> bool {
        action == ACTION
    }

    fn descriptors(&self, workspace: &Workspace) -> Vec<ActionDescriptor> {
        if !self.workspace.governs() || workspace.room.to_string() != self.workspace.room() {
            return Vec::new();
        }
        vec![ActionDescriptor {
            address: ActionAddress::new(RESOURCE, ACTION),
            semantic_identity: None,
            resource_kind: "workspace.legacy-media".into(),
            resource_label: "Legacy profile media".into(),
            action_label: "Import selected legacy media".into(),
            description: Some("Owner only. Creates new reachability in this room from exact resident hashes in this profile's content store; no scan or ownership inference. Up to 128 hashes and 256 MiB. Source store: runtime-profile-content; source room: unscoped.".into()),
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            contract: ActionContract {
                kind: tilo_action_policy::ActionKind::Outside,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                execution: tilo_actions::ExecutionMode::Background { cancellable: false },
                result: tilo_actions::ResultSpec::Json {
                    schema: serde_json::json!({"type": "object", "properties": {"imported_bytes": {"type": "integer", "minimum": 0}}, "required": ["imported_bytes"], "additionalProperties": false}),
                },
                tags: vec![tilo_actions::contract::LOCAL_DEVICE_ONLY.into()],
                ..Default::default()
            },
            input: tilo_actions::door::InputSpec::None,
            params: vec![
                ActionParam::required("source_store", ParamType::Text, "Host-configured source: runtime-profile-content").with_choices([SOURCE]),
                ActionParam::required("source_room", ParamType::Text, "Legacy scope: unscoped").with_choices(["unscoped"]),
                ActionParam::required("destination_room", ParamType::Text, "This runtime's owned room"),
                ActionParam::required("hashes", ParamType::Array, "Exact unique lowercase blake3 hashes; 1 to 128")
                    .with_schema(serde_json::json!({"minItems": 1, "maxItems": 128, "uniqueItems": true, "items": {"type": "string", "pattern": "^[0-9a-f]{64}$"}})),
                ActionParam::required("max_bytes", ParamType::Integer, "Total byte ceiling; 1 to 268435456")
                    .with_schema(serde_json::json!({"minimum": 1, "maximum": 268_435_456})),
            ],
            view: None,
        }]
    }

    fn start(
        &self,
        invocation: &WorkspaceGovernanceRequest<'_>,
    ) -> Result<Pending<WorkspaceGovernanceOutcome>, String> {
        let (host, permit) = self
            .prepare(invocation)
            .map_err(|error| error.to_string())?;
        let (answer, pending) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let outcome =
                match tokio::time::timeout(MAX_LEGACY_IMPORT_LIFETIME, host.import(permit)).await {
                    Ok(Ok(bytes)) => Ok(tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({ "imported_bytes": bytes })),
                        resources: Vec::new(),
                    }),
                    result => Err(Box::new(tilo_actions::ActionFailure {
                        code: "legacy_import_incomplete".into(),
                        message: match result {
                            Ok(Err(error)) => error.to_string(),
                            _ => "legacy import expired; a partial import may remain".into(),
                        },
                        details: None,
                        retryable: false,
                    })),
                };
            answer.complete(outcome);
        });
        Ok(pending)
    }
}

impl MediaImport {
    fn prepare(
        &self,
        invocation: &WorkspaceGovernanceRequest<'_>,
    ) -> anyhow::Result<(
        LegacyMediaImportHost,
        tilo_fs::document_media::LegacyImportPermit,
    )> {
        anyhow::ensure!(
            self.workspace.governs(),
            "legacy import requires the local room owner"
        );
        anyhow::ensure!(
            invocation.local_device
                && invocation.resource == RESOURCE
                && invocation.action == ACTION,
            "legacy import requires the local owner action"
        );
        let account = invocation.actor.direct_person().ok_or_else(|| {
            anyhow::anyhow!("legacy import requires the owner, not a delegated caller")
        })?;
        anyhow::ensure!(
            invocation
                .confirmation
                .get("confirm")
                .and_then(serde_json::Value::as_bool)
                == Some(true),
            "explicit confirmation is required"
        );
        let argument = invocation
            .argument
            .and_then(|value| value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
            .ok_or_else(|| anyhow::anyhow!("legacy import requires typed arguments"))?;
        anyhow::ensure!(argument.len() <= 32 * 1024, "legacy import argument limit");
        let request: LegacyImportRequest = serde_json::from_str(argument)?;
        anyhow::ensure!(
            request.destination_room == invocation.room && invocation.room == self.workspace.room(),
            "legacy import destination is not this runtime's owned room"
        );
        let revision = owner_revision(self.workspace.as_ref(), invocation.room, account)?;
        let (_, cert) = tilo_identity_host::broker::trusted_current_device(invocation.room)?;
        let approval = LegacyImportApproval {
            account: account.into(),
            device: hex::encode(cert.device_public_key),
            authority_revision: revision,
            invocation: invocation.invocation.0.to_string(),
            nonce: uuid::Uuid::new_v4().to_string(),
            argument_digest: *blake3::hash(&serde_json::to_vec(&request)?).as_bytes(),
        };
        let authority = Arc::new(Authority {
            workspace: self.workspace.clone(),
            request: request.clone(),
            approval: approval.clone(),
            device: cert.device_public_key,
            lease: invocation.execution_lease.clone(),
        });
        let media = DocumentMedia::new(self.store.clone());
        let host =
            LegacyMediaImportHost::new_unscoped(SOURCE.into(), media.clone(), media, authority);
        let permit = host.permit(request, approval, MAX_LEGACY_IMPORT_LIFETIME)?;
        Ok((host, permit))
    }
}
