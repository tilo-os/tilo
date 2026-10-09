//! Supplemental native Message correlation, never an invocation permission.
//! Required HostDocuments provenance remains separate and unchanged.
use super::{PublishedInvocationRequest, PublishedInvocationSpec, current_invoker};
use crate::{collaboration::RouteSpec, history_mode::owner::HistoryOwner};
use automerge::AutoCommit;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_CHANGES: usize = 4096;

#[cfg(all(test, feature = "broker-transport"))]
pub(crate) mod tests;

/// Only an actual native invocation-stream read constructs this wrapper. Its
/// opaque token is resolved by the broker; none of these descriptive fields
/// grant the original caller or the application additional authority.
#[derive(Clone)]
pub struct NativeInvocationSource {
    route: RouteSpec,
    request: PublishedInvocationRequest,
    source: runtime_collab::document_contribution::DocumentContributionSource,
    spec: PublishedInvocationSpec,
    retired: Arc<AtomicBool>,
}

impl std::fmt::Debug for NativeInvocationSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeInvocationSource(..)")
    }
}

impl NativeInvocationSource {
    #[cfg(feature = "broker-transport")]
    pub(crate) fn from_delivery(
        route: RouteSpec,
        delivery: &runtime_collab::Invocation,
        context: Option<&tilo_actions::ActionContext>,
        lifetime: &NativeMessageStreamLifetime,
    ) -> Result<Option<Self>, String> {
        let Some(source) = &delivery.document_source else {
            return Ok(None);
        };
        let spec: PublishedInvocationSpec = serde_json::from_str(source.spec_json())
            .map_err(|_| "invalid native Message source declaration")?;
        if spec.kind != tilo_actions::OperationKind::Document {
            return Err("native Message source is not a document action".into());
        }
        Ok(Some(Self {
            route,
            request: PublishedInvocationRequest {
                wire_id: delivery.id,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                    delivery.invocation_uuid,
                )),
                address: tilo_actions::ActionAddress::new(
                    delivery.resource.clone(),
                    delivery.action.clone(),
                ),
                argument: delivery.argument.clone(),
                actor: delivery.actor.clone(),
                participant: PublishedInvocationRequest::delivered_participant(
                    &delivery.actor,
                    context,
                ),
            },
            source: source.clone(),
            spec,
            retired: lifetime.retired.clone(),
        }))
    }

    pub(crate) fn route(&self) -> &RouteSpec {
        &self.route
    }

    #[cfg(feature = "broker-transport")]
    pub(crate) fn request(&self) -> &PublishedInvocationRequest {
        &self.request
    }

    pub(crate) fn spec(&self) -> &PublishedInvocationSpec {
        &self.spec
    }

    #[cfg(feature = "broker-transport")]
    pub(crate) fn transport_source(
        &self,
    ) -> &runtime_collab::document_contribution::DocumentContributionSource {
        &self.source
    }

    pub(crate) fn matches(&self, request: &PublishedInvocationRequest) -> Result<(), String> {
        self.check()?;
        if &self.request != request {
            return Err("native Message delivery differs from its immutable source".into());
        }
        Ok(())
    }

    pub(crate) fn enter_delivery(self) -> NativeDeliveryScope {
        NativeDeliveryScope(DELIVERED.with_borrow_mut(|current| current.replace(self)))
    }

    pub(crate) fn current_delivery() -> Option<Self> {
        DELIVERED.with_borrow(Clone::clone)
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if self.retired.load(Ordering::Acquire) {
            Err("native Message invocation stream retired".into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>();
        for length in [
            self.route.app_id.capacity(),
            self.route.record.as_ref().map_or(0, String::capacity),
            self.request.address.resource.capacity(),
            self.request.address.action.capacity(),
            self.request.actor.capacity(),
            self.request.argument.as_ref().map_or(0, String::capacity),
            self.request
                .participant
                .as_ref()
                .map_or(0, |id| id.as_str().len()),
            self.source.spec_json().len(),
        ] {
            bytes = bytes.checked_add(length)?;
        }
        // Conservatively charge parsed declarations, including collection
        // capacity, rather than counting only their borrowed wire strings.
        bytes.checked_add(self.source.spec_json().len().checked_mul(4)?)
    }
}

/// The actual stream task owns this guard, including early failure and abort.
/// Retiring it stops fresh input effects, not historical receipt observation.
#[cfg(feature = "broker-transport")]
pub(crate) struct NativeMessageStreamLifetime {
    retired: Arc<AtomicBool>,
}

#[cfg(feature = "broker-transport")]
impl NativeMessageStreamLifetime {
    pub(crate) fn new() -> Self {
        Self {
            retired: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[cfg(feature = "broker-transport")]
impl Drop for NativeMessageStreamLifetime {
    fn drop(&mut self) {
        self.retired.store(true, Ordering::Release);
    }
}

/// Exact successful executor output. Applications cannot construct it from a
/// claimed action label, row, hash range, UUID or token. The final owner still
/// verifies the signed operation and actual winning request-row facts.
pub struct NativeExecutedContribution {
    source: NativeInvocationSource,
    envelope: Vec<u8>,
    changes: Vec<[u8; 32]>,
}

impl std::fmt::Debug for NativeExecutedContribution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep the retained payload opaque in every host configuration. Only
        // bounded sizes, never tokens, document bytes or change hashes, escape.
        formatter
            .debug_struct("NativeExecutedContribution")
            .field("source", &self.source)
            .field("envelope_bytes", &self.envelope.len())
            .field("change_count", &self.changes.len())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "broker-transport")]
impl NativeExecutedContribution {
    pub(crate) fn source(&self) -> &NativeInvocationSource {
        &self.source
    }

    pub(crate) fn envelope(&self) -> &[u8] {
        &self.envelope
    }

    pub(crate) fn changes(&self) -> &[[u8; 32]] {
        &self.changes
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        self.source
            .retained_bytes()?
            .checked_add(self.envelope.capacity())?
            .checked_add(self.changes.capacity().checked_mul(32)?)?
            .checked_add(std::mem::size_of::<Self>())
    }
}

struct CaptureState {
    contribution: Option<Arc<NativeExecutedContribution>>,
    refusal: Option<String>,
    changed: bool,
}

#[derive(Clone)]
pub(crate) struct NativeMessageCapture {
    owner: HistoryOwner,
    admission: super::RecordAdmission,
    source: NativeInvocationSource,
    state: Rc<RefCell<CaptureState>>,
}

thread_local! {
    static ACTIVE: RefCell<Option<NativeMessageCapture>> = const { RefCell::new(None) };
    static DELIVERED: RefCell<Option<NativeInvocationSource>> = const { RefCell::new(None) };
}

impl NativeMessageCapture {
    pub(crate) fn new(owner: HistoryOwner, source: NativeInvocationSource) -> Result<Self, String> {
        source.check()?;
        owner
            .validate()
            .map_err(|_| "native Message owner unavailable")?;
        if owner.broker_route().as_ref() != Some(source.route()) {
            return Err("native Message source belongs to another mounted record".into());
        }
        let (store, checkout) = owner.store_checkout();
        let admission = store
            .admission(checkout)
            .map_err(|_| "native Message record admission unavailable")?;
        Ok(Self {
            owner,
            admission,
            source,
            state: Rc::new(RefCell::new(CaptureState {
                contribution: None,
                refusal: None,
                changed: false,
            })),
        })
    }

    pub(crate) fn enter(&self) -> ActiveCapture {
        ActiveCapture(ACTIVE.with_borrow_mut(|active| active.replace(self.clone())))
    }

    pub(crate) fn has_changes(&self) -> bool {
        self.state.borrow().changed
    }

    /// Validate the current handler's encoded Message before it can mutate a
    /// document. Fresh resolution deliberately chooses the current callback,
    /// so its encoded destination must still match the immutable broker source.
    pub(crate) fn validate_encoded(
        document: &AutoCommit,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        ACTIVE.with_borrow(|active| {
            let Some(capture) = active else { return Ok(()) };
            capture.validate_fresh_message(document, encoded)
        })
    }

    fn validate_fresh_message(
        &self,
        document: &AutoCommit,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        let Some(binding) = &self.source.spec.fresh_binding else {
            return Ok(());
        };
        self.source.check()?;
        self.owner
            .check_invocation_lifetime()
            .map_err(|_| "native Message owner retired")?;
        let (store, checkout) = self.owner.store_checkout();
        if !super::record_loan_matches(store, checkout, document)
            || !self.admission.matches(document, checkout)
            || current_invoker().as_ref() != Some(&self.source.request.invoker())
        {
            return Err("fresh Message owner or invoker differs from its source".into());
        }
        type InertMessage = tilo_actions::agent_message::AgentMessagePayload<
            serde::de::IgnoredAny,
            serde::de::IgnoredAny,
            serde::de::IgnoredAny,
            serde::de::IgnoredAny,
        >;
        let identity = binding
            .descriptor
            .semantic_identity
            .as_ref()
            .filter(|identity| identity.kind == "chat.agent")
            .ok_or("fresh Message semantic target unavailable")?;
        let agent = identity
            .key
            .strip_prefix("chat.person:")
            .filter(|agent| !agent.is_empty())
            .ok_or("fresh Message semantic target is not canonical")?;
        let text =
            original_message_text(&self.source.spec, self.source.request.argument.as_deref())?;
        let typed: tilo_actions::agent_message::AgentMessageMutation<InertMessage> =
            serde_json::from_slice(&encoded.payload)
                .map_err(|_| "fresh Message encoded action is not canonical typed input")?;
        let tilo_actions::agent_message::AgentMessageMutation::PostAgentMessage {
            workspace,
            agent: encoded_agent,
            message,
        } = typed;
        if encoded.type_id != "chat.action"
            || self.source.route.app_id != "so.tilo.chat"
            || self
                .source
                .route
                .record
                .as_deref()
                .is_some_and(|record| !record.is_empty())
            || workspace != self.source.route.cluster_id.to_string()
            || encoded_agent != agent
            || message.intent != tilo_actions::agent_message::AgentIntent::Message
            || message.text != text
        {
            return Err("fresh Message encoded destination differs from its source".into());
        }
        Ok(())
    }

    pub(crate) fn route(&self) -> &RouteSpec {
        self.source.route()
    }

    /// Called only after actual AppliedLocal, before history follow-up actions.
    /// A refusal is sticky so the caller must block this claimed publication,
    /// rather than publish the changed document through the ordinary fallback.
    pub(crate) fn retain_applied(
        document: &mut AutoCommit,
        envelope: &tilo_actions::ActionEnvelope,
    ) -> Result<(), String> {
        ACTIVE.with_borrow(|active| {
            let Some(capture) = active else { return Ok(()) };
            let result = capture.retain(document, envelope);
            if let Err(reason) = &result {
                capture
                    .state
                    .borrow_mut()
                    .refusal
                    .get_or_insert_with(|| reason.clone());
            }
            result
        })
    }

    fn retain(
        &self,
        document: &mut AutoCommit,
        envelope: &tilo_actions::ActionEnvelope,
    ) -> Result<(), String> {
        // A nested independently admitted invocation keeps its own authority;
        // it is never relabelled as the enclosing Message's contribution.
        if envelope
            .context
            .as_ref()
            .and_then(|context| context.source_invocation)
            != Some(self.source.request.invocation)
        {
            return Ok(());
        }
        if !document
            .get_changes(&envelope.base_change_hashes())
            .is_empty()
        {
            self.state.borrow_mut().changed = true;
        }
        self.source.check()?;
        self.owner
            .check_invocation_lifetime()
            .map_err(|_| "native Message owner retired")?;
        let (store, checkout) = self.owner.store_checkout();
        if !super::record_loan_matches(store, checkout, document)
            || !self.admission.matches(document, checkout)
            || current_invoker().as_ref() != Some(&self.source.request.invoker())
            || envelope.action.type_id != "chat.action"
            || !envelope.context.as_ref().is_some_and(|context| {
                context.source_invocation == Some(self.source.request.invocation)
                    && context.executed_action == Some(envelope.id)
            })
        {
            return Err("native Message executor correlation differs from its source".into());
        }
        let mut state = self.state.borrow_mut();
        if state.refusal.is_some() || state.contribution.is_some() {
            return Err("native Message source already has an execution contribution".into());
        }
        let committed = document.get_changes(&envelope.base_change_hashes());
        if committed
            .iter()
            .any(|change| change.actor_id() != &envelope.actor())
        {
            return Err("native Message range includes a different executor writer".into());
        }
        let mut changes: Vec<_> = committed
            .into_iter()
            .map(|change| change.hash().0)
            .collect();
        changes.sort_unstable();
        changes.dedup();
        if changes.is_empty() || changes.len() > MAX_CHANGES {
            return Err("native Message executor has no bounded committed range".into());
        }
        let encoded = serde_json::to_vec(envelope)
            .map_err(|_| "native Message executor correlation encoding failed")?;
        let bytes = self
            .source
            .retained_bytes()
            .and_then(|bytes| bytes.checked_add(encoded.capacity()))
            .and_then(|bytes| bytes.checked_add(changes.capacity().checked_mul(32)?))
            .ok_or("native Message execution retention overflow")?;
        if bytes > collab_types::MAX_DOCUMENT_TRANSFER_BYTES {
            return Err("native Message execution exceeds document capacity".into());
        }
        // Check before cloning source metadata. The envelope is encoded once;
        // its shared Arc then keeps the same charge across queue uncertainty.
        state.contribution = Some(Arc::new(NativeExecutedContribution {
            source: self.source.clone(),
            envelope: encoded,
            changes,
        }));
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Arc<NativeExecutedContribution>, String> {
        let mut state = self.state.borrow_mut();
        if let Some(refusal) = state.refusal.take() {
            return Err(refusal);
        }
        self.source.check()?;
        state
            .contribution
            .take()
            .ok_or_else(|| "native Message completed without a typed execution contribution".into())
    }
}

fn original_message_text(
    spec: &PublishedInvocationSpec,
    argument: Option<&str>,
) -> Result<String, String> {
    let argument = argument.ok_or("original Message text is unavailable")?;
    let bound = if let Some(json) = argument.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
        let supplied = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
            .map_err(|_| "original Message arguments are not canonical JSON")?;
        tilo_actions::ActionArgs::bind_json(&spec.params, &supplied)
            .map_err(|_| "original Message arguments do not match its specification")?
    } else {
        tilo_actions::ActionArgs::bind(
            &spec.params,
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

pub(crate) struct ActiveCapture(Option<NativeMessageCapture>);

impl Drop for ActiveCapture {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|active| *active = self.0.take());
    }
}

pub(crate) struct NativeDeliveryScope(Option<NativeInvocationSource>);

impl Drop for NativeDeliveryScope {
    fn drop(&mut self) {
        DELIVERED.with_borrow_mut(|current| *current = self.0.take());
    }
}
