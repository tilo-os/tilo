//! Creation selection and results in the existing serialized owner.
use super::*;
impl SessionOwner {
    /// Explicit rollout only. Ordinary opens and commands do not select V6.
    pub fn activate_creation(&mut self) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        self.aggregate.require_writable()?;
        if self.store.is_poisoned() {
            bail!("reopen owner after uncertain durability");
        }
        if !matches!(self.aggregate.publication, PublicationStorage::V5 { .. })
            || !self.peer.admission_v5_selected()
            || self.peer.incomplete_reason().is_some()
        {
            bail!("creation requires explicit native V5 delivery selection");
        }
        if self.aggregate.creation.is_some() {
            return self.flush();
        }
        let mut next = self.aggregate.clone();
        next.creation = Some(crate::canonical_records::creation::CreationLedgerV1::default());
        self.commit(next, self.peer.clone())
    }
}
#[cfg(test)]
mod tests;

use crate::canonical_records::creation::*;
use qos_peer_state::{
    desktop_command::{DesktopCommand, DesktopCommandError},
    owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest},
};

/// Uncommitted local data prepared only from the already authenticated owner request.
/// The caller still holds the same owner turn through publication.
pub(crate) struct DesktopCreationRequest {
    intent: CreationIntentV1,
    certificate: DeviceCert,
    original: Vec<u8>,
}
fn key(request: &OwnerCommitRequest) -> Result<CreationKeyV1, String> {
    let CommitOperation::DesktopCommand {
        caller, invocation, ..
    } = &request.operation
    else {
        return Err("not a desktop creation request".into());
    };
    Ok(CreationKeyV1 {
        room: *uuid::Uuid::parse_str(&request.room)
            .map_err(|e| e.to_string())?
            .as_bytes(),
        caller_root: caller.user_public_key,
        caller_device: caller.device_public_key,
        invocation: *invocation.as_bytes(),
    })
}
impl SessionOwner {
    pub(crate) fn prepare_desktop_creation(
        &self,
        request: &OwnerCommitRequest,
        app: &str,
    ) -> Result<Option<DesktopCreationRequest>, String> {
        let Some(ledger) = &self.aggregate.creation else {
            return Ok(None);
        };
        let CommitOperation::DesktopCommand {
            caller, command, ..
        } = &request.operation
        else {
            return Ok(None);
        };
        match &command.command {
            DesktopCommand::OpenTab { app: requested, .. }
            | DesktopCommand::OpenApp { app: requested }
                if requested == app => {}
            DesktopCommand::DuplicateTab { .. } => (),
            _ => return Err("not a checked desktop factory command".into()),
        }
        let application = crate::canonical_records::resolve(app)
            .ok_or("application has no installed checked record factory")?;
        let checkout = qos_desktop::records::component_record(
            app,
            1,
            application.definition().record_kind.clone(),
        );
        if checkout.id.is_package_primary() && self.peer.document(app, "").is_some() {
            return Ok(None);
        }
        let key = key(request)?;
        let intent = CreationIntentV1 {
            version: 1,
            key: key.clone(),
            original_caller_certificate_digest: certificate_digest(caller)?,
            app: app.to_owned(),
            registry_revision: application.registry_revision(),
            source: CreationSourceV1::DesktopFactory {
                original_desktop_digest: request.binding()?.request_digest,
                owner_scope: RetainedInvocationScope::Certified {
                    root: key.caller_root,
                    device: key.caller_device,
                },
            },
        };
        let original = postcard::to_stdvec(request).map_err(|e| e.to_string())?;
        ledger.reserve_row(&intent, caller, &original)?;
        Ok(Some(DesktopCreationRequest {
            intent,
            certificate: caller.clone(),
            original,
        }))
    }

    /// Exact retained request lookup precedes source-revision/factory gates, but
    /// current membership, original ownership and record disclosure still apply.
    #[cfg(test)]
    fn replay_desktop_creation(
        &mut self,
        request: &OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<Option<OwnerCommitReceipt>, DesktopCommandError> {
        self.replay_desktop_creation_current(request, now, None)
    }
    pub(crate) fn replay_desktop_creation_current(
        &mut self,
        request: &OwnerCommitRequest,
        now: SystemTime,
        time: Option<&qos_action_broker::peer_actions::PolicyTime>,
    ) -> Result<Option<OwnerCommitReceipt>, DesktopCommandError> {
        let rejected = DesktopCommandError::Rejected;
        let Some(ledger) = &self.aggregate.creation else {
            return Ok(None);
        };
        let key = key(request).map_err(rejected)?;
        let Some(row) = ledger.lookup(&key) else {
            return Ok(None);
        };
        let original = postcard::to_stdvec(request).map_err(|e| rejected(e.to_string()))?;
        let CommitOperation::DesktopCommand { caller, .. } = &request.operation else {
            unreachable!()
        };
        if row.original_request != original || row.original_certificate != *caller {
            return Err(rejected("creation identity substitution".into()));
        }
        row.validate().map_err(rejected)?;
        if request.room != self.peer.room()
            || !self.permits_publication()
            || !self.peer.certificates().contains(caller)
        {
            return Err(rejected(
                "creation caller or owner is no longer current".into(),
            ));
        }
        self.peer
            .permissions(caller.device_public_key, now)
            .map_err(rejected)?;
        if self.store.is_poisoned() {
            return Err(DesktopCommandError::Uncertain(
                "reopen owner after uncertain durability".into(),
            ));
        }
        self.aggregate
            .require_writable()
            .map_err(|e| rejected(e.to_string()))?;
        self.peer.validated_media_policy().map_err(rejected)?;
        let route = row.result.body.route.clone();
        self.peer
            .check_document_read(caller.device_public_key, &route.app, &route.record, now)
            .map_err(rejected)?;
        // Status must not hand out an expired genesis envelope. Refresh through
        // the normal governor publisher/barrier, retaining the immutable result.
        let mut checkpoint_committed = false;
        if !self.creation_checkpoint_current(&route, now) {
            let refreshed = match time {
                Some(time) => {
                    let fence = OwnerPolicyFence::Desktop(
                        crate::desktop_commands::DesktopPolicyFence::observation(
                            request,
                            time,
                            &route.app,
                            &route.record,
                        ),
                    );
                    let mut candidate = self.peer.clone();
                    let generation = self
                        .aggregate
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| rejected("session generation exhausted".into()))?;
                    let at = fence.sample().map_err(|e| rejected(e.to_string()))?;
                    let messages = self
                        .prepare_certified_bootstrap_at(&mut candidate, generation, at)
                        .map_err(|e| rejected(e.to_string()))?;
                    let mut next = self.aggregate.clone();
                    self.replace_publication(&mut next, &messages)
                        .map_err(|e| rejected(e.to_string()))?;
                    self.commit_delivery(next, candidate, None, CommitMode::PolicyFenced(fence))
                }
                None => self.refresh_checkpoint(),
            };
            refreshed.map_err(|e| match e.downcast_ref::<OwnerPolicyRefused>() {
                Some(refused) => rejected(refused.0.clone()),
                None => DesktopCommandError::Uncertain(e.to_string()),
            })?;
            checkpoint_committed = true;
        }
        let cap = self
            .serialized_commit_cap()
            .map_err(|e| rejected(e.to_string()))?;
        let bytes = encode_with_cap(&self.aggregate, &self.identity, None, cap)
            .map_err(|e| rejected(e.to_string()))?;
        // A pure retained read has no new barrier: its disclosure is checked
        // after encoding. Never reclassify a successfully written checkpoint as
        // a no-effect rejection because its policy lifetime expires afterward.
        if let Some(time) = time.filter(|_| !checkpoint_committed) {
            #[cfg(test)]
            policy_fence::before_check();
            crate::desktop_commands::DesktopPolicyFence::observation(
                request,
                time,
                &route.app,
                &route.record,
            )
            .revalidate(self)
            .map_err(rejected)?;
        }
        Ok(Some(OwnerCommitReceipt {
            binding: request.binding().map_err(rejected)?,
            generation: self.aggregate.generation,
            aggregate_hash: *blake3::hash(&bytes).as_bytes(),
        }))
    }
    fn creation_checkpoint_current(
        &self,
        route: &ResolvedCreationRouteV1,
        now: SystemTime,
    ) -> bool {
        let Ok(Some(receipt)) = self.peer.durable_coverage(&route.app, &route.record) else {
            return false;
        };
        if !self
            .peer
            .durable_covers_current(&route.app, &route.record)
            .is_ok_and(|covered| covered)
        {
            return false;
        }
        self.outgoing().iter().any(|publication| {
            let Ok(message) = postcard::from_bytes::<NeighbourMsg>(publication.bytes) else {
                return false;
            };
            let NeighbourMsg::AppDocument {
                app_id,
                cluster_id,
                record_id,
                envelope,
                ..
            } = &message
            else {
                return false;
            };
            app_id == &route.app
                && cluster_id == self.peer.room()
                && record_id == &route.record
                && envelope.author.as_bytes() == &receipt.coverage.author
                && envelope.seq == receipt.coverage.sequence
                && *blake3::hash(&envelope.payload).as_bytes() == receipt.coverage.payload_hash
                && qos_peer_state::outbox::fresh(&message, now)
        })
    }
    pub(crate) fn commit_desktop_creation_candidate(
        &mut self,
        candidate: StatePeer,
        request: Option<DesktopCreationRequest>,
        publications: &[NeighbourMsg],
        original_request: &OwnerCommitRequest,
        time: Option<&qos_action_broker::peer_actions::PolicyTime>,
    ) -> Result<CommitReceipt> {
        let row = match request {
            Some(request) if !publications.is_empty() => {
                if publications.len() != 1 {
                    bail!("desktop creation must resolve one exact record");
                }
                let NeighbourMsg::AppDocument {
                    app_id,
                    cluster_id,
                    record_id,
                    envelope,
                    ..
                } = &publications[0]
                else {
                    bail!("creation publication missing");
                };
                if app_id != &request.intent.app
                    || cluster_id != self.peer.room()
                    || self.peer.document(app_id, record_id).is_some()
                {
                    bail!("creation route no longer absent");
                }
                let admission = candidate
                    .lookup_exact_local_document(
                        app_id,
                        record_id,
                        envelope,
                        policy_fence::sample(time).map_err(OwnerPolicyRefused)?,
                    )
                    .map_err(anyhow::Error::msg)?
                    .context("creation exact admission missing")?;
                let application = crate::canonical_records::resolve(app_id)
                    .context("checked creation factory missing")?;
                let result = CreationResultV1::from_body(CreationResultBodyV1 {
                    version: 1,
                    key: request.intent.key.clone(),
                    intent_digest: request.intent.digest().map_err(anyhow::Error::msg)?,
                    route: ResolvedCreationRouteV1 {
                        room: request.intent.key.room,
                        app: app_id.clone(),
                        record: record_id.clone(),
                        kind: application.definition().record_kind.as_str().to_owned(),
                    },
                    terminal: CreationTerminalV1::Created(CreatedV1 {
                        // Existing qps coverage payload identity of the exact whole
                        // genesis. This field is not a causal-equivalence witness.
                        genesis_payload_hash: *blake3::hash(&envelope.payload).as_bytes(),
                        winning_signed_envelope_digest: admission.operation_id(),
                        publication_author: admission.author(),
                        publication_sequence: admission.sequence(),
                        exact_admission_identity: admission.operation_id(),
                    }),
                    owner_generation: self
                        .aggregate
                        .generation
                        .checked_add(1)
                        .context("session generation exhausted")?,
                })
                .map_err(anyhow::Error::msg)?;
                Some(CreationRowV1 {
                    intent: request.intent,
                    original_certificate: request.certificate,
                    original_request: request.original,
                    result,
                })
            }
            _ => None,
        };
        let fence = time.map(|time| {
            OwnerPolicyFence::Desktop(crate::desktop_commands::DesktopPolicyFence::mutation(
                original_request,
                time,
                publications
                    .iter()
                    .filter_map(|message| match message {
                        NeighbourMsg::AppDocument {
                            app_id, record_id, ..
                        } => Some((app_id.clone(), record_id.clone())),
                        _ => None,
                    })
                    .collect(),
            ))
        });
        self.commit_mutation_candidate_with_creation(
            candidate,
            self.directory().to_vec(),
            row,
            fence,
        )
    }
}
