//! Source tokens resolve only against immutable broker originals and the actual
//! native publisher stream. Neither the SDK nor a wire caller can mint these.
use super::{MessagePublisherStream, OriginalMessageFacts};
use crate::collab_broker_capnp::{
    CapnpBrokerState, CrdtRoute, InvocationEnvelope, PrivateWorkerRegistration,
    published_invocation::OriginalInvocationData,
};
use std::{collections::HashMap, sync::Arc};

macro_rules! delivery_marker {
    ($stage:literal, $outcome:literal) => {{
        tracing::debug!(
            target: "tilo_agent_delivery",
            concat!("native-delivery:", $stage, ":", $outcome)
        )
    }};
}

pub(super) struct MessageSource {
    pub(super) peer: u64,
    pub(super) route: CrdtRoute,
    pub(super) worker: PrivateWorkerRegistration,
    pub(super) original: Arc<OriginalInvocationData>,
    pub(super) invocation: InvocationEnvelope,
    pub(super) stream: Arc<MessagePublisherStream>,
    pub(super) lease: tilo_joined_room::AgentMessagePublisherLease,
    pub(super) facts: OriginalMessageFacts,
}

#[derive(Default)]
pub(crate) struct MessageSources {
    tokens: HashMap<[u8; 32], Arc<MessageSource>>,
    uploads: HashMap<[u8; 32], Arc<super::upload::RetainedUpload>>,
    budget: Arc<super::upload::UploadBudget>,
}

impl MessageSources {
    #[cfg(test)]
    pub(crate) fn retained_counts(&self) -> (usize, usize) {
        (self.tokens.len(), self.uploads.len())
    }
    #[cfg(test)]
    pub(crate) fn retained_reference(&self) -> [u8; 32] {
        assert_eq!(self.uploads.len(), 1);
        *self.uploads.keys().next().unwrap()
    }
    pub(crate) fn retire(&mut self, peer: Option<u64>) {
        self.uploads
            .retain(|_, upload| peer.is_some_and(|peer| upload.source.peer != peer));
        self.tokens
            .retain(|_, source| peer.is_some_and(|peer| source.peer != peer));
    }

    pub(crate) fn retire_stream(&mut self, peer: u64, stream: &Arc<MessagePublisherStream>) {
        self.uploads.retain(|_, upload| {
            upload.source.peer != peer || !Arc::ptr_eq(&upload.source.stream, stream)
        });
        self.tokens
            .retain(|_, source| source.peer != peer || !Arc::ptr_eq(&source.stream, stream));
    }
}

impl CapnpBrokerState {
    pub(in crate::collab_broker_capnp) fn prepare_native_message_publication(
        &self,
        peer: u64,
        route: &CrdtRoute,
        op_bytes: &[u8],
        envelope: &collab_types::SignedOp,
        references: &[[u8; 32]],
    ) -> Result<super::NativeMessagePublication, String> {
        use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
        if op_bytes.is_empty()
            || op_bytes.len() > runtime_collab::document_contribution::MAX_SIGNED_OP_BYTES
            || references.is_empty()
            || references.len() > runtime_collab::document_contribution::MAX_REFERENCES
            || references.iter().enumerate().any(|(index, reference)| {
                references
                    .iter()
                    .take(index)
                    .any(|prior| prior == reference)
            })
            || envelope.encryption.is_some()
        {
            return Err("invalid native Message publication boundaries".into());
        }
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "Message admission unavailable")?;
        let (uploads, budget) = {
            let sources = self
                .message_sources
                .lock()
                .map_err(|_| "Message source registry unavailable")?;
            let uploads = references
                .iter()
                .map(|reference| {
                    sources
                        .uploads
                        .get(reference)
                        .cloned()
                        .ok_or("Message upload retired or unknown")
                })
                .collect::<Result<Vec<_>, _>>()?;
            (uploads, sources.budget.clone())
        };
        // Count retained upload bytes separately from the encoded copies needed
        // for decoding, host signing, and the final owner call. The charge and
        // upload Arcs remain in the closed request through its awaited result.
        let processing_bytes = uploads.iter().try_fold(
            op_bytes
                .len()
                .checked_mul(2)
                .ok_or("Message processing overflow")?,
            |bytes, upload| {
                upload
                    .total
                    .checked_mul(4)
                    .and_then(|extra| bytes.checked_add(extra))
                    .ok_or("Message processing overflow")
            },
        )?;
        let processing = budget.reserve(processing_bytes, false)?;
        let request = OwnerCommitRequest {
            room: route.cluster_id.clone(),
            operation: CommitOperation::Document {
                app: route.app_id.clone(),
                record: route.record_id.clone(),
                envelope: Box::new(envelope.clone()),
            },
        };
        let binding = request.binding()?;
        let publisher = uploads
            .first()
            .ok_or("Message contributions are empty")?
            .source
            .lease
            .clone();
        let mut admissions = Vec::with_capacity(uploads.len());
        for upload in &uploads {
            self.revalidate_message_source(peer, route, &upload.source)?;
            if upload.source.lease.digest() != publisher.digest() {
                return Err("Message contributions mix publisher custody".into());
            }
            let bytes = upload
                .bytes
                .lock()
                .map_err(|_| "Message upload unavailable")?;
            if bytes.len() != upload.total {
                return Err("Message upload is incomplete".into());
            }
            let body: runtime_collab::document_contribution::DocumentContributionBody =
                postcard::from_bytes(&bytes)
                    .map_err(|_| "Message contribution encoding is invalid")?;
            drop(bytes);
            admissions.push(upload.source.admission(body, op_bytes, binding.clone())?);
        }
        Ok(super::NativeMessagePublication {
            request,
            admissions,
            publisher,
            _uploads: uploads,
            processing: Arc::new(processing),
            host_attestations: std::sync::Mutex::new(None),
        })
    }

    pub(crate) fn reserve_native_contribution(
        &self,
        peer: u64,
        route: &CrdtRoute,
        token: [u8; 32],
        total: usize,
    ) -> Result<[u8; 32], String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "Message admission unavailable")?;
        let source = self
            .message_sources
            .lock()
            .map_err(|_| "Message source registry unavailable")?
            .tokens
            .get(&token)
            .cloned()
            .ok_or("Message source retired or unknown")?;
        self.revalidate_message_source(peer, route, &source)?;
        let mut sources = self
            .message_sources
            .lock()
            .map_err(|_| "Message source registry unavailable")?;
        if sources
            .uploads
            .values()
            .any(|upload| Arc::ptr_eq(&upload.source, &source))
        {
            return Err("Message source already has a retained upload".into());
        }
        let upload = super::upload::RetainedUpload::reserve(source, total, &sources.budget)?;
        let reference = fresh_token(&sources.uploads);
        sources.uploads.insert(reference, upload);
        drop(sources);
        Ok(reference)
    }

    fn revalidate_message_source(
        &self,
        peer: u64,
        route: &CrdtRoute,
        source: &MessageSource,
    ) -> Result<(), String> {
        if source.peer != peer || source.route != *route {
            return Err("Message contribution belongs to another publisher".into());
        }
        let current = self
            .message_publisher_stream(peer)
            .ok_or("Message publisher retired")?;
        if !Arc::ptr_eq(&current, &source.stream)
            || current.lease()?.digest() != source.lease.digest()
        {
            return Err("Message publisher custody changed".into());
        }
        let worker = self
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(&peer)
            .and_then(|worker| worker.worker_registration.clone());
        if worker.as_ref() != Some(&source.worker) || !source.worker.current() {
            return Err("Message publisher registration changed".into());
        }
        source.original.message_facts(self, &source.invocation)?;
        Ok(())
    }

    pub(crate) fn append_native_contribution(
        &self,
        peer: u64,
        route: &CrdtRoute,
        reference: [u8; 32],
        offset: usize,
        chunk: &[u8],
    ) -> Result<(), String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "Message admission unavailable")?;
        let upload = self
            .message_sources
            .lock()
            .map_err(|_| "Message source registry unavailable")?
            .uploads
            .get(&reference)
            .cloned()
            .ok_or("Message upload retired or unknown")?;
        self.revalidate_message_source(peer, route, &upload.source)?;
        upload.append(offset, chunk)
    }

    pub(crate) fn discard_incomplete_native_contribution(&self, reference: [u8; 32]) {
        if let Ok(mut sources) = self.message_sources.lock() {
            let complete = sources.uploads.get(&reference).is_some_and(|upload| {
                upload
                    .bytes
                    .lock()
                    .is_ok_and(|bytes| bytes.len() == upload.total)
            });
            if !complete {
                sources.uploads.remove(&reference);
            }
        }
    }

    fn native_message_guard(
        &self,
        peer: u64,
        delivered_stream: &Arc<MessagePublisherStream>,
    ) -> Result<
        (
            Arc<MessagePublisherStream>,
            tilo_joined_room::AgentMessagePublisherLease,
            PrivateWorkerRegistration,
        ),
        String,
    > {
        delivery_marker!("broker_guards", "in_flight");
        let stream = self.message_publisher_stream(peer).ok_or_else(|| {
            delivery_marker!("broker_stream_missing", "definitive_refusal");
            delivery_marker!("broker_guards", "definitive_refusal");
            "native Message stream unavailable"
        })?;
        if !Arc::ptr_eq(&stream, delivered_stream) {
            delivery_marker!("broker_stream_replaced", "definitive_refusal");
            delivery_marker!("broker_guards", "definitive_refusal");
            return Err("delivered Message publisher stream changed".into());
        }
        let lease = stream.lease().inspect_err(|error| {
            match error.as_str() {
                "Message publisher owner custody unavailable" => {
                    delivery_marker!("broker_lease_missing", "definitive_refusal");
                }
                "Message publisher stream retired" => {
                    delivery_marker!("broker_lease_retired", "definitive_refusal");
                }
                _ => delivery_marker!("broker_lease_unavailable", "definitive_refusal"),
            }
            delivery_marker!("broker_guards", "definitive_refusal");
        })?;
        let worker = self
            .host_workers
            .lock()
            .map_err(|_| {
                delivery_marker!("broker_worker_registry", "definitive_refusal");
                delivery_marker!("broker_guards", "definitive_refusal");
                "worker registry unavailable"
            })?
            .get(&peer)
            .and_then(|worker| worker.worker_registration.clone())
            .ok_or_else(|| {
                delivery_marker!("broker_worker_missing", "definitive_refusal");
                delivery_marker!("broker_guards", "definitive_refusal");
                "native Message worker unavailable"
            })?;
        if !worker.current() {
            delivery_marker!("broker_guards", "definitive_refusal");
            return Err("native Message launcher process or connection ended".into());
        }
        delivery_marker!("broker_guards", "complete");
        Ok((stream, lease, worker))
    }

    pub(in crate::collab_broker_capnp) fn native_message_source(
        &self,
        peer: u64,
        invocation: &InvocationEnvelope,
        delivered_stream: &Arc<MessagePublisherStream>,
    ) -> Result<Option<([u8; 32], String)>, String> {
        delivery_marker!("broker_original", "in_flight");
        let _admission = self.admission_lock.lock().map_err(|_| {
            delivery_marker!("broker_original", "definitive_refusal");
            "Message admission unavailable"
        })?;
        let original = {
            let registry = self.pending_invocations.lock().map_err(|_| {
                delivery_marker!("broker_original", "definitive_refusal");
                "invocation registry unavailable"
            })?;
            let Some(pending) = registry.get(&invocation.id) else {
                delivery_marker!("broker_original", "definitive_refusal");
                return Err("delivered invocation is no longer retained".into());
            };
            if pending.peer_id != peer || pending.update.invocation() != invocation.invocation {
                delivery_marker!("broker_original", "definitive_refusal");
                return Err("delivered Message original changed".into());
            }
            let original = pending.original.clone();
            drop(registry);
            original
        };
        delivery_marker!("broker_original", "complete");
        let Some(original) = original.filter(|original| original.is_addressed_message()) else {
            return Ok(None);
        };
        delivery_marker!("broker_facts", "in_flight");
        let facts = original.message_facts(self, invocation).inspect_err(|_| {
            delivery_marker!("broker_facts", "definitive_refusal");
        })?;
        delivery_marker!("broker_facts", "complete");
        let (stream, lease, worker) = self.native_message_guard(peer, delivered_stream)?;
        delivery_marker!("source_output", "in_flight");
        let spec = serde_json::to_string(&facts.spec).map_err(|_| {
            delivery_marker!("source_output", "definitive_refusal");
            "Message dispatch specification encoding failed"
        })?;
        if spec.len() > runtime_collab::document_contribution::MAX_SOURCE_SPEC_BYTES {
            delivery_marker!("source_output", "definitive_refusal");
            return Err("Message dispatch specification exceeds capacity".into());
        }
        let source = Arc::new(MessageSource {
            peer,
            route: worker.route.clone(),
            worker,
            original,
            invocation: invocation.clone(),
            stream,
            lease,
            facts,
        });
        let mut sources = self.message_sources.lock().map_err(|_| {
            delivery_marker!("source_output", "definitive_refusal");
            "Message source registry unavailable"
        })?;
        if let Some((token, _)) = sources.tokens.iter().find(|(_, retained)| {
            retained.peer == peer
                && retained.worker == source.worker
                && Arc::ptr_eq(&retained.original, &source.original)
                && Arc::ptr_eq(&retained.stream, &source.stream)
        }) {
            delivery_marker!("source_output", "complete");
            return Ok(Some((*token, spec)));
        }
        if sources.tokens.len() >= super::MAX_MESSAGE_SOURCES {
            delivery_marker!("source_output", "definitive_refusal");
            return Err("native Message source registry is full".into());
        }
        let token = fresh_token(&sources.tokens);
        sources.tokens.insert(token, source);
        drop(sources);
        delivery_marker!("source_output", "complete");
        Ok(Some((token, spec)))
    }
}

impl MessageSource {
    fn admission(
        &self,
        body: runtime_collab::document_contribution::DocumentContributionBody,
        signed_op: &[u8],
        document: tilo_peer_state::owner_commit::CommitBinding,
    ) -> Result<tilo_joined_room::AgentMessageAdmissionData, String> {
        use tilo_actions::agent_message::{AgentIntent, AgentMessageMutation, AgentMessagePayload};
        type InertMessage = AgentMessagePayload<
            serde::de::IgnoredAny,
            serde::de::IgnoredAny,
            serde::de::IgnoredAny,
            serde::de::IgnoredAny,
        >;
        if body.signed_op != signed_op
            || body.changes.is_empty()
            || body.changes.len() > 4096
            || body.changes.contains(&[0; 32])
            || body
                .changes
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err("Message contribution differs from actual signed op or range".into());
        }
        // ActionEnvelope and ActionContext intentionally omit absent fields
        // for legacy JSON compatibility. A positional codec cannot preserve
        // those optional correlation fields; the nested envelope uses JSON.
        let action: tilo_actions::ActionEnvelope = serde_json::from_slice(&body.envelope)
            .map_err(|_| "Message executed envelope is invalid")?;
        if action.action.type_id != "chat.action"
            || action
                .context
                .as_ref()
                .and_then(|context| context.source_invocation)
                != Some(self.facts.request.invocation)
            || action
                .context
                .as_ref()
                .and_then(|context| context.executed_action)
                != Some(action.id)
        {
            return Err("Message executed envelope belongs to another original or action".into());
        }
        let typed: AgentMessageMutation<InertMessage> =
            serde_json::from_slice(&action.action.payload)
                .map_err(|_| "Message executed action is not canonical typed input")?;
        let AgentMessageMutation::PostAgentMessage {
            workspace,
            agent,
            message,
        } = typed;
        let original_text = original_message_text(
            &self.facts.spec.params,
            self.facts.request.argument.as_deref(),
        )?;
        if workspace != self.route.cluster_id
            || agent != self.facts.agent
            || message.intent != AgentIntent::Message
            || message.text != original_text
        {
            return Err("Message typed input differs from actual original target or text".into());
        }
        let executed_actor = action.actor().to_bytes().to_vec();
        Ok(tilo_joined_room::AgentMessageAdmissionData {
            invocation: self.facts.request.invocation.0,
            caller: self.facts.caller.clone(),
            action: action.action.payload,
            document,
            publisher_digest: self.lease.digest(),
            operation: self.facts.operation.clone(),
            executed_action: action.id,
            executed_actor,
            changes: body.changes,
        })
    }
}

fn original_message_text(
    params: &[tilo_actions::ActionParam],
    argument: Option<&str>,
) -> Result<String, String> {
    let argument = argument.ok_or("original Message text is unavailable")?;
    let bound = if let Some(json) = argument.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
        let supplied = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
            .map_err(|_| "original Message arguments are not canonical JSON")?;
        tilo_actions::ActionArgs::bind_json(params, &supplied)
            .map_err(|_| "original Message arguments do not match its specification")?
    } else {
        tilo_actions::ActionArgs::bind(
            params,
            [(
                tilo_actions::PROMPTED_PARAM.to_owned(),
                tilo_actions::ArgValue::Text(argument.to_owned()),
            )],
        )
        .map_err(|_| "original Message text does not match its specification")?
    };
    bound
        .text(tilo_actions::PROMPTED_PARAM)
        .map(str::to_owned)
        .ok_or_else(|| "original Message specification has no prompted text".into())
}

fn fresh_token<T>(entries: &HashMap<[u8; 32], T>) -> [u8; 32] {
    loop {
        let mut token = [0; 32];
        for part in token.chunks_exact_mut(16) {
            part.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        }
        if !entries.contains_key(&token) {
            return token;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::original_message_text;
    use tilo_actions::{ARGUMENTS_JSON_PREFIX, ActionParam, PROMPTED_PARAM, ParamType};

    fn params() -> Vec<ActionParam> {
        vec![ActionParam::required(
            PROMPTED_PARAM,
            ParamType::Text,
            "Message",
        )]
    }

    #[test]
    fn original_message_text_validates_legacy_and_typed_arguments() {
        assert_eq!(
            original_message_text(&params(), Some("Hello")).unwrap(),
            "Hello"
        );
        let typed = format!("{ARGUMENTS_JSON_PREFIX}{{\"value\":\"Hello\"}}");
        assert_eq!(
            original_message_text(&params(), Some(&typed)).unwrap(),
            "Hello"
        );

        for json in [
            "{}",
            "{\"value\":1}",
            "{\"value\":\"Hello\",\"extra\":true}",
        ] {
            let malformed = format!("{ARGUMENTS_JSON_PREFIX}{json}");
            assert!(original_message_text(&params(), Some(&malformed)).is_err());
        }
    }
}
