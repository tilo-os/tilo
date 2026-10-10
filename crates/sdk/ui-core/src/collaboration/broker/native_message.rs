//! One complete native signed Message publication. No ordinary send fallback.
use super::{BridgeState, LocalSdkDocumentSigner, SdkDocumentSigner, SdkRoute};
use crate::invocation_scope::NativeExecutedContribution;
use collab_types::{DocumentMutation, SignedOp};
use runtime_collab::document_contribution::{
    DocumentContributionBody, DocumentContributionReference, DocumentContributionUpload,
    MAX_CHUNK_BYTES, MAX_REFERENCES, MAX_SIGNED_OP_BYTES, MAX_UPLOAD_BYTES,
};
use std::sync::{Arc, Mutex};

macro_rules! delivery_marker {
    ($stage:literal, $outcome:literal) => {{
        tracing::debug!(
            target: "tilo_agent_delivery",
            concat!("native-delivery:", $stage, ":", $outcome)
        )
    }};
}

/// One shovel-wide charge, shared by every route and retained through unknown
/// publication outcomes. This is memory accounting, not an authority guard.
#[derive(Clone, Default)]
pub(super) struct NativeDocumentBudget(Arc<Mutex<usize>>);

pub(super) struct NativeDocumentQueue {
    budget: NativeDocumentBudget,
    charged: usize,
    contributions: Vec<Arc<NativeExecutedContribution>>,
    prepared: Option<PreparedNativeMessagePublication>,
}

impl NativeDocumentQueue {
    pub(super) fn new(budget: NativeDocumentBudget) -> Self {
        Self {
            budget,
            charged: 0,
            contributions: Vec::new(),
            prepared: None,
        }
    }

    fn resize_charge(&mut self, bytes: usize) -> Result<(), String> {
        let mut total = self
            .budget
            .0
            .lock()
            .map_err(|_| "native Message budget unavailable")?;
        let next = total
            .checked_sub(self.charged)
            .and_then(|n| n.checked_add(bytes))
            .ok_or("native Message budget overflow")?;
        if next > MAX_UPLOAD_BYTES {
            return Err("native Message pending publications exceed document capacity".into());
        }
        *total = next;
        self.charged = bytes;
        Ok(())
    }

    pub(super) fn claimed(&self) -> bool {
        !self.contributions.is_empty()
    }

    pub(super) fn stage(
        &mut self,
        route: &SdkRoute,
        contribution: Arc<NativeExecutedContribution>,
    ) -> Result<(), String> {
        delivery_marker!("source_stage", "in_flight");
        let result = self.stage_exact(route, contribution);
        match &result {
            Ok(()) => delivery_marker!("source_stage", "complete"),
            Err(_) => delivery_marker!("source_stage", "definitive_refusal"),
        }
        result
    }

    fn stage_exact(
        &mut self,
        route: &SdkRoute,
        contribution: Arc<NativeExecutedContribution>,
    ) -> Result<(), String> {
        if self.prepared.is_some() || self.contributions.len() >= MAX_REFERENCES {
            return Err("native Message publication already pending".into());
        }
        contribution.source().check()?;
        if contribution.source().route() != &route.spec
            || self.contributions.iter().any(|prior| {
                prior.source().request().invocation == contribution.source().request().invocation
                    || prior
                        .changes()
                        .iter()
                        .any(|hash| contribution.changes().contains(hash))
            })
        {
            return Err("native Message contribution duplicates or crosses routes".into());
        }
        let bytes = self
            .charged
            .checked_add(
                contribution
                    .retained_bytes()
                    .ok_or("native Message capacity overflow")?,
            )
            .ok_or("native Message capacity overflow")?;
        self.resize_charge(bytes)?;
        self.contributions.push(contribution);
        Ok(())
    }

    #[expect(
        clippy::future_not_send,
        reason = "actual Capnp capabilities stay on the shovel LocalSet"
    )]
    pub(super) async fn publish(
        &mut self,
        route: &SdkRoute,
        signer: &mut LocalSdkDocumentSigner,
        mutation: &DocumentMutation,
        state: &BridgeState,
    ) -> Result<(), String> {
        if self.prepared.is_none() {
            // Reserve all transient DTO/signing copies BEFORE encoding.
            // Shared route accounting remains charged throughout await.
            self.resize_charge(preparation_charge(mutation, &self.contributions)?)?;
            delivery_marker!("prepare", "in_flight");
            let prepared = match PreparedNativeMessagePublication::prepare(
                route,
                signer,
                mutation,
                &self.contributions,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    delivery_marker!("prepare", "uncertain");
                    return Err(error);
                }
            };
            delivery_marker!("prepare", "complete");
            let bytes = prepared
                .retained_bytes()
                .and_then(|n| {
                    self.contributions
                        .iter()
                        .try_fold(n, |n, c| n.checked_add(c.retained_bytes()?))
                })
                .ok_or("native Message capacity overflow")?;
            self.prepared = Some(prepared);
            self.resize_charge(bytes)?;
        }
        let prepared = self
            .prepared
            .as_mut()
            .ok_or("native Message publication absent")?;
        prepared.matches_mutation(mutation)?;
        prepared
            .publish(route, state)
            .await
            .map_err(|_| "native Message publication outcome unresolved".to_string())
    }

    pub(super) fn observed_terminal(
        &mut self,
        wire_id: u64,
        invocation: Option<tilo_actions::InvocationId>,
    ) {
        if self
            .prepared
            .as_ref()
            .is_some_and(PreparedNativeMessagePublication::saved)
            && self.contributions.iter().any(|c| {
                c.source().request().wire_id == wire_id
                    && invocation.is_none_or(|uuid| uuid == c.source().request().invocation)
            })
        {
            // An aggregate cannot release another original's retained custody.
            self.contributions.retain(|c| {
                c.source().request().wire_id != wire_id
                    || invocation.is_some_and(|uuid| uuid != c.source().request().invocation)
            });
            if self.contributions.is_empty() {
                self.prepared = None;
                let _ = self.resize_charge(0);
            }
        }
    }
}

impl Drop for NativeDocumentQueue {
    fn drop(&mut self) {
        if let Ok(mut total) = self.budget.0.lock() {
            *total = total.saturating_sub(self.charged);
        }
    }
}

fn preparation_charge(
    mutation: &DocumentMutation,
    contributions: &[Arc<NativeExecutedContribution>],
) -> Result<usize, String> {
    if mutation.data().len() > MAX_SIGNED_OP_BYTES {
        return Err("native Message requires one bounded complete signed document".into());
    }
    // Mutation tag/length and fixed SignedOp fields fit comfortably within
    // this overhead; the actual encoded complete op has a separate hard cap.
    let op = mutation
        .data()
        .len()
        .checked_add(1024)
        .ok_or("native Message capacity overflow")?;
    contributions.iter().try_fold(
        op.checked_mul(3)
            .ok_or("native Message capacity overflow")?,
        |bytes, contribution| {
            let raw = contribution
                .envelope()
                .len()
                .checked_add(
                    contribution
                        .changes()
                        .len()
                        .checked_mul(32)
                        .ok_or("native Message capacity overflow")?,
                )
                .and_then(|n| n.checked_add(op))
                .and_then(|n| n.checked_add(64))
                .ok_or("native Message capacity overflow")?;
            bytes
                .checked_add(
                    contribution
                        .retained_bytes()
                        .ok_or("native Message capacity overflow")?
                        .checked_mul(2)
                        .ok_or("native Message capacity overflow")?,
                )
                .and_then(|n| n.checked_add(raw.checked_mul(3)?))
                .ok_or_else(|| "native Message capacity overflow".into())
        },
    )
}

#[cfg(test)]
#[path = "native_message/tests.rs"]
mod tests;

struct PendingUpload {
    contribution: Arc<NativeExecutedContribution>,
    body: Option<Vec<u8>>,
    writer: Option<DocumentContributionUpload>,
    reference: Option<DocumentContributionReference>,
    offset: usize,
}

/// The shovel keeps this object BEFORE opening uploads or sending the op. A
/// lost response retains the exact signed bytes, original UUIDs and references;
/// retry never signs a replacement envelope or enters ordinary publication.
pub(super) struct PreparedNativeMessagePublication {
    op: Vec<u8>,
    mutation_digest: [u8; 32],
    uploads: Vec<PendingUpload>,
    saved: bool,
}

impl PreparedNativeMessagePublication {
    #[expect(
        clippy::future_not_send,
        reason = "actual Capnp signer stays on the shovel LocalSet"
    )]
    pub(super) async fn prepare(
        route: &SdkRoute,
        signer: &mut LocalSdkDocumentSigner,
        mutation: &DocumentMutation,
        contributions: &[Arc<NativeExecutedContribution>],
    ) -> Result<Self, String> {
        validate_contributions(route, contributions)?;
        let op = sign_complete_native_document(route, signer, mutation).await?;
        let uploads = build_uploads(contributions, &op)?;
        Ok(Self {
            op,
            mutation_digest: mutation_digest(mutation)?,
            uploads,
            saved: false,
        })
    }

    pub(super) fn matches_mutation(&self, mutation: &DocumentMutation) -> Result<(), String> {
        if mutation_digest(mutation)? != self.mutation_digest {
            return Err("pending native Message publication has different document bytes".into());
        }
        Ok(())
    }

    pub(super) fn saved(&self) -> bool {
        self.saved
    }

    pub(super) fn retained_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>()
            .checked_add(self.op.capacity())?
            .checked_add(
                self.uploads
                    .capacity()
                    .checked_mul(std::mem::size_of::<PendingUpload>())?,
            )?;
        for pending in &self.uploads {
            bytes = bytes
                .checked_add(pending.contribution.retained_bytes()?)?
                .checked_add(pending.body.as_ref().map_or(0, Vec::capacity))?;
        }
        Some(bytes)
    }

    #[expect(
        clippy::future_not_send,
        reason = "actual Capnp capabilities stay on the shovel LocalSet"
    )]
    pub(super) async fn publish(
        &mut self,
        route: &SdkRoute,
        state: &BridgeState,
    ) -> Result<(), super::DocumentPublicationError> {
        let result = self.publish_exact(route).await;
        result.map_err(|reason| {
            // The original remains retained in self. This fence never
            // reports absence, retries with a new UUID, or downgrades to
            // ordinary Person traffic after claimed admission fails.
            state.publication_failed("native Message publication", reason, true)
        })
    }

    #[expect(
        clippy::future_not_send,
        reason = "actual Capnp capabilities stay on the shovel LocalSet"
    )]
    async fn publish_exact(&mut self, route: &SdkRoute) -> Result<(), String> {
        let mut references = Vec::with_capacity(self.uploads.len());
        for pending in &mut self.uploads {
            pending.contribution.source().check()?;
            if pending.contribution.source().route() != &route.spec {
                return Err("pending Message publication belongs to another route".into());
            }
            if pending.reference.is_none() {
                upload_exact(route, pending).await?;
            }
            references.push(
                pending
                    .reference
                    .clone()
                    .ok_or("native Message upload is incomplete")?,
            );
        }
        delivery_marker!("owner_send", "in_flight");
        if route
            .channel
            .send_with_contributions(&self.op, &references)
            .await
            .is_err()
        {
            delivery_marker!("owner_send", "uncertain");
            return Err("native Message send outcome is uncertain".into());
        }
        self.saved = true;
        delivery_marker!("owner_send", "saved");
        Ok(())
    }
}

fn mutation_digest(mutation: &DocumentMutation) -> Result<[u8; 32], String> {
    if mutation.data().len() > MAX_SIGNED_OP_BYTES {
        return Err("native Message requires one bounded complete signed document".into());
    }
    let bytes = mutation
        .encode()
        .map_err(|_| "native Message document will not encode")?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

fn validate_contributions(
    route: &SdkRoute,
    contributions: &[Arc<NativeExecutedContribution>],
) -> Result<(), String> {
    if contributions.is_empty() || contributions.len() > MAX_REFERENCES {
        return Err("invalid native Message contribution count".into());
    }
    for (index, contribution) in contributions.iter().enumerate() {
        contribution.source().check()?;
        if contribution.source().route() != &route.spec
            || contributions[..index].iter().any(|prior| {
                prior.source().request().invocation == contribution.source().request().invocation
                    || prior
                        .changes()
                        .iter()
                        .any(|hash| contribution.changes().contains(hash))
            })
        {
            return Err("native Message contributions overlap or belong to another route".into());
        }
    }
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "actual Capnp signer stays on the shovel LocalSet"
)]
async fn sign_complete_native_document(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    mutation: &DocumentMutation,
) -> Result<Vec<u8>, String> {
    fn refused(reason: &'static str) -> String {
        delivery_marker!("sign", "definitive_refusal");
        reason.into()
    }

    fn uncertain(reason: &'static str) -> String {
        delivery_marker!("sign", "uncertain");
        reason.into()
    }

    delivery_marker!("sign", "in_flight");
    if !route.committed {
        return Err(refused(
            "native Message requires an installed committed document owner",
        ));
    }
    if !route
        .encryption
        .lock()
        .is_ok_and(|keys| matches!(keys.keys, crate::collaboration::route_keys::Keys::Plain))
    {
        return Err(refused(
            "native Message cannot bypass encrypted document admission",
        ));
    }
    if mutation.data().len() > MAX_SIGNED_OP_BYTES {
        return Err(refused(
            "native Message requires one bounded complete signed document",
        ));
    }
    let payload = mutation
        .encode()
        .map_err(|_| refused("native Message document will not encode"))?;
    if payload.len() > MAX_SIGNED_OP_BYTES {
        return Err(refused(
            "native Message requires one bounded complete signed document",
        ));
    }
    let workspace = route.spec.cluster_id.to_string();
    let record = route.spec.record.as_deref().unwrap_or("");
    let author = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.signing_public_key()
        }
        LocalSdkDocumentSigner::Host(identity) => identity.signing_public_key(),
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(_)) => {
            return Err(refused(
                "native Message requires a certified native document signer",
            ));
        }
    };
    let now = crate::time::wall();
    let mut op = SignedOp::new_unsigned(
        payload,
        collab_types::document_artifact_id(&route.spec.app_id, &workspace, record),
        collab_types::Ed25519PublicKey::from_bytes(author),
        collab_types::document_attestation_id(&route.spec.app_id, &workspace, record),
        collab_types::OpClass::Splice,
        now,
        collab_types::next_unordered_document_sequence(now),
    );
    let unsigned = postcard::to_stdvec(&op)
        .map_err(|_| refused("native Message unsigned document will not encode"))?;
    let signature = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => identity
            .sign_collaboration_op(&workspace, &route.spec.app_id, record, &unsigned)
            .map_err(|_| uncertain("native Message certified signing outcome is uncertain"))?,
        LocalSdkDocumentSigner::Host(identity) => identity
            .sign_collaboration_op(&workspace, &route.spec.app_id, record, &unsigned)
            .await
            .map_err(|_| uncertain("native Message host signing outcome is uncertain"))?,
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(_)) => {
            return Err(refused(
                "native Message requires a certified native document signer",
            ));
        }
    };
    op.signature = collab_types::Signature::from_bytes(signature);
    let bytes = postcard::to_stdvec(&op)
        .map_err(|_| refused("native Message signed document will not encode"))?;
    if bytes.len() > MAX_SIGNED_OP_BYTES {
        return Err(refused(
            "native Message signed document exceeds native frame capacity",
        ));
    }
    delivery_marker!("sign", "complete");
    Ok(bytes)
}

fn build_uploads(
    contributions: &[Arc<NativeExecutedContribution>],
    op: &[u8],
) -> Result<Vec<PendingUpload>, String> {
    let mut charged = op.len();
    for contribution in contributions {
        let raw = contribution
            .envelope()
            .len()
            .checked_add(
                contribution
                    .changes()
                    .len()
                    .checked_mul(32)
                    .ok_or("native Message upload capacity overflow")?,
            )
            .and_then(|bytes| bytes.checked_add(op.len()))
            .and_then(|bytes| bytes.checked_add(64))
            .ok_or("native Message upload capacity overflow")?;
        // Budget BEFORE cloning DTO vectors or reserving uploads. This covers
        // original capture, transient DTO copies and encoded upload bodies.
        charged = charged
            .checked_add(
                contribution
                    .retained_bytes()
                    .ok_or("native Message retention capacity overflow")?,
            )
            .and_then(|bytes| bytes.checked_add(raw.checked_mul(3)?))
            .ok_or("native Message upload capacity overflow")?;
    }
    if charged > MAX_UPLOAD_BYTES {
        return Err("native Message pending uploads exceed document capacity".into());
    }
    contributions
        .iter()
        .map(|contribution| {
            let body = postcard::to_stdvec(&DocumentContributionBody {
                envelope: contribution.envelope().to_vec(),
                changes: contribution.changes().to_vec(),
                signed_op: op.to_vec(),
            })
            .map_err(|_| "native Message upload will not encode")?;
            Ok(PendingUpload {
                contribution: contribution.clone(),
                body: Some(body),
                writer: None,
                reference: None,
                offset: 0,
            })
        })
        .collect()
}

#[expect(
    clippy::future_not_send,
    reason = "actual Capnp upload writer stays on the shovel LocalSet"
)]
async fn upload_exact(route: &SdkRoute, pending: &mut PendingUpload) -> Result<(), String> {
    let body = pending
        .body
        .as_ref()
        .ok_or("native Message upload body retired")?;
    if pending.writer.is_none() {
        delivery_marker!("upload_open", "in_flight");
        let writer = route
            .channel
            .open_document_contribution(
                pending.contribution.source().transport_source(),
                body.len(),
            )
            .await;
        let Ok(writer) = writer else {
            delivery_marker!("upload_open", "uncertain");
            return Err("native Message upload reservation outcome is uncertain".into());
        };
        pending.writer = Some(writer);
        delivery_marker!("upload_open", "complete");
    }
    let writer = pending
        .writer
        .as_mut()
        .ok_or("native Message upload writer absent")?;
    for chunk in body[pending.offset..].chunks(MAX_CHUNK_BYTES) {
        delivery_marker!("upload_write", "in_flight");
        if writer.write(chunk).await.is_err() {
            delivery_marker!("upload_write", "uncertain");
            return Err("native Message chunk outcome is uncertain".into());
        }
        pending.offset = pending
            .offset
            .checked_add(chunk.len())
            .ok_or("native Message upload offset overflow")?;
        delivery_marker!("upload_write", "complete");
    }
    pending.reference = Some(
        writer
            .reference()
            .map_err(|_| "native Message upload has no complete reference")?,
    );
    pending.body = None;
    Ok(())
}
