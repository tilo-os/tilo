//! Exact selected-workspace material for a single runtime MCP execution.
//! Uses existing custody and enrollment; never enrolls, provisions or grants.
use super::{Arc, Connect, Joined, WorkspaceId, workspace_topic};
use ed25519_dalek::Signer as _;
use tilo_actions::remote::Caller;
use tilo_user_identity::DeviceCert;

/// Why an admitted run may not go on.
///
/// The client sees the refusal; only what it can act on is said, so a
/// changed custody (the workspace or device this run was admitted for is gone)
/// and custody that could not be read (a disk or identity store that failed
/// this once) are never the same answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::standalone) enum Refusal {
    /// The run ended: a custody change this run was admitted for, or the
    /// caller's own request to end it.
    Ended,
    /// This call could not be checked because the identity material could
    /// not be read. Nothing changed; the run stays live and the call may be
    /// retried.
    Unavailable { reason: String },
    /// The call came from a caller this run was not admitted for.
    Stranger,
}

impl Refusal {
    /// The message the client sees. It carries no custody material, only
    /// what happened to the check.
    pub(in crate::standalone) fn message(&self) -> String {
        match self {
            Self::Ended => "execution session ended".into(),
            Self::Unavailable { reason } => format!(
                "the caller's custody could not be verified, so this call was not run: {reason}; nothing changed, retry the call"
            ),
            Self::Stranger => "the caller may not invoke this action".into(),
        }
    }
}

impl From<Refusal> for tilo_actions::remote::ActionHostError {
    /// The client sees what it can act on: an ended run stays ended, and a
    /// check that could not be made names itself as retryable.
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unavailable { reason } => Self::Unavailable { reason },
            Refusal::Ended | Refusal::Stranger => Self::NotPermitted,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for Refusal {}

pub(in crate::standalone) struct SessionAdmission {
    caller: Caller,
    workspace: WorkspaceId,
    certificate: DeviceCert,
    enrollment: (tilo_peer_state::join::TransportHint, u64),
    presentation: Vec<u8>,
    _registration: super::super::execution::ExecutionRegistration,
}

impl SessionAdmission {
    pub(in crate::standalone) fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }
    pub(in crate::standalone) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(in crate::standalone) fn presentation(&self) -> &[u8] {
        &self.presentation
    }

    pub(in crate::standalone) fn check(&self, joined: &Joined) -> Result<(), Refusal> {
        if !self.caller.execution_is_live() {
            return Err(Refusal::Ended);
        }
        self.check_original_custody(joined)
    }

    /// Compare the custody this run was admitted with to the workspace's now.
    ///
    /// Only a comparison that was actually made can end the run: material
    /// that could not be read (a full disk, a busy identity store) says
    /// nothing about custody, so that call is refused as retryable and the
    /// run keeps its lease. Ending it there would kill the person's whole
    /// agent session over one transient failure.
    fn check_original_custody(&self, joined: &Joined) -> Result<(), Refusal> {
        let (certificate, enrollment) = match joined.mcp_material(self.workspace) {
            Ok(material) => material,
            Err(reason) => {
                tracing::warn!(
                    workspace = %self.workspace,
                    %reason,
                    "an admitted run's custody could not be read; refusing this call, keeping the run"
                );
                return Err(Refusal::Unavailable { reason });
            }
        };
        if certificate != self.certificate || enrollment != self.enrollment {
            tracing::warn!(
                workspace = %self.workspace,
                "an admitted run's workspace or device custody changed; ending the run"
            );
            self.caller.retire_execution();
            return Err(Refusal::Ended);
        }
        Ok(())
    }
}

impl Joined {
    fn mcp_material(
        &self,
        workspace: WorkspaceId,
    ) -> Result<(DeviceCert, (tilo_peer_state::join::TransportHint, u64)), String> {
        // Read apart, so a custody that no longer matches the enrollment is
        // a confirmed change rather than a read that failed.
        let (certificate, _) = self
            .custody
            .current_material(Some(workspace_topic(workspace)?))?;
        let enrollment = self.owner.with_current(workspace, |view| {
            Ok((view.transport_hint()?.clone(), view.continuity()))
        })?;
        Ok((certificate, enrollment))
    }

    pub(in crate::standalone) fn admit_mcp_execution(
        &self,
        workspace: &str,
        actor: &str,
    ) -> Result<Arc<SessionAdmission>, String> {
        let selected = WorkspaceId::parse(workspace).ok_or("invalid exact MCP workspace")?;
        if selected.is_nil() || selected.to_string() != workspace {
            return Err("noncanonical MCP workspace".into());
        }
        // Only a workspace this device currently takes part in under its
        // enrolled custody is admitted.
        Connect::new(self.owner.clone(), selected)?.material(&self.custody)?;
        let (certificate, enrollment) = self.mcp_material(selected)?;
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                .in_workspace(workspace)
                // Preserve the standalone host's existing foreign-device restriction,
                // including when a signed certificate permits a stronger device.
                .from_foreign_device(true)
                .in_new_execution();
        if caller.principal() != &tilo_actions::remote::CallerPrincipal::Person(actor.into()) {
            caller.retire_execution();
            return Err(
                "selected workspace identity differs from the runtime profile actor".into(),
            );
        }
        // The one owner observer is already installed. Register this exact
        // lease, then re-read the pinned material: a concurrent native fence
        // either closes the lease or is detected by the changed continuity.
        let registration = self.executions.register_checked(selected, &caller, || {
            let current = self.mcp_material(selected)?;
            if current != (certificate.clone(), enrollment.clone()) {
                return Err("workspace changed while admitting execution".into());
            }
            Ok(())
        })?;
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.into(),
            device_certificate: serde_json::to_vec(&certificate)
                .map_err(|error| error.to_string())?,
            proof: self
                .custody
                .device_signing_key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let admission = Arc::new(SessionAdmission {
            caller,
            workspace: selected,
            certificate,
            enrollment,
            presentation,
            _registration: registration,
        });
        self.executions.attach(&admission)?;
        admission.check(self).map_err(|refusal| refusal.message())?;
        Ok(admission)
    }

    pub(in crate::standalone) fn recheck_mcp_executions(
        &self,
        workspace: WorkspaceId,
    ) -> Result<(), String> {
        // Baselines come only from host-admitted sessions, never a presented
        // certificate. Release the registry lock before material reads.
        let admissions = self.executions.admissions(workspace)?;
        self.recheck_admissions(&admissions)
    }

    pub(super) fn recheck_admissions(
        &self,
        admissions: &[Arc<SessionAdmission>],
    ) -> Result<(), String> {
        let mut result = Ok(());
        for admission in admissions {
            // An execution may end normally after the snapshot. Its lifetime
            // says nothing about the workspace's custody, so only the pinned
            // material is compared; a retired run stays retired. Custody
            // that could not be read is retried by the next pass, as the
            // announced change stays pending.
            if admission.check_original_custody(self) == Err(Refusal::Ended) {
                result = Err(Refusal::Ended.message());
            }
        }
        result
    }
}
