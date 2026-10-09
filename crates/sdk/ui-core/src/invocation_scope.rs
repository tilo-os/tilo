//! Host-local invocation custody. No wire caller can manufacture these checks.

/// A typed host check response, suitable for an existing effect worker.
pub type ApprovedCheckFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<runtime_collab::approved_checks::Receipt, String>>
            + Send,
    >,
>;

/// Inert delivered inputs. Only the registered host can associate these with
/// an original admitted caller, contract and exact worker connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedInvocationRequest {
    pub wire_id: u64,
    pub invocation: tilo_actions::InvocationId,
    pub address: tilo_actions::ActionAddress,
    pub argument: Option<String>,
    pub actor: String,
    /// Whose view this invocation resolves against. Set only by the host that
    /// admitted `actor` (for an agent task, derived from the admitted task),
    /// never taken from a caller's envelope. It routes to view state and is
    /// never authority. Part of the exact request both ends compare.
    pub participant: Option<tilo_actions::ParticipantId>,
}
impl PublishedInvocationRequest {
    /// The invoker this request resolves for: the admitted actor plus the
    /// host-set participant.
    pub fn invoker(&self) -> tilo_actions::ActionInvoker {
        tilo_actions::ActionInvoker {
            actor: self.actor.clone(),
            participant: self.participant.clone(),
        }
    }

    /// The participant of a delivered invocation: the host-built delivery
    /// context names it for this exact `actor`, or there is none
    /// ([`tilo_actions::ActionInvoker::delivered`]).
    pub fn delivered_participant(
        actor: &str,
        context: Option<&tilo_actions::ActionContext>,
    ) -> Option<tilo_actions::ParticipantId> {
        tilo_actions::ActionInvoker::delivered(actor, context).participant
    }
}

thread_local! {
    static CURRENT_INVOKERS: std::cell::RefCell<Vec<tilo_actions::ActionInvoker>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The invoker whose resource tree a delivered invocation resolves against,
/// for the duration of its resolution and dispatch on this thread.
///
/// Resource collection for resolution
/// ([`crate::widget::Widget::resources_for`]) and view operations read it,
/// so an invocation is resolved against the invoker's own view, rebuilt at
/// invoke time, never against a published snapshot. Scopes nest; dropping
/// the guard restores the enclosing invoker.
#[must_use = "the invoker applies only while the guard is alive"]
pub struct InvokerScope(());
impl InvokerScope {
    pub fn enter(invoker: tilo_actions::ActionInvoker) -> Self {
        CURRENT_INVOKERS.with_borrow_mut(|invokers| invokers.push(invoker));
        Self(())
    }
}
impl Drop for InvokerScope {
    fn drop(&mut self) {
        CURRENT_INVOKERS.with_borrow_mut(|invokers| {
            invokers.pop();
        });
    }
}

thread_local! {
    static VIEWERS: std::cell::RefCell<Vec<tilo_actions::ParticipantId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The participant a published tree is being built for while it lives:
/// what a surface offers bound to where someone is (their caret, their
/// Find hits) is that participant's, not the window's. It grants nothing:
/// an invocation still runs for its own invoker.
pub struct ViewerScope(());
impl ViewerScope {
    pub fn enter(participant: tilo_actions::ParticipantId) -> Self {
        VIEWERS.with_borrow_mut(|viewers| viewers.push(participant));
        Self(())
    }
}
impl Drop for ViewerScope {
    fn drop(&mut self) {
        VIEWERS.with_borrow_mut(|viewers| {
            viewers.pop();
        });
    }
}

/// Whose view what is being built or resolved now is: the participant of
/// the delivered invocation, else the participant a tree is published for.
/// `None` for the window's own person.
pub fn current_viewer() -> Option<tilo_actions::ParticipantId> {
    current_participant().or_else(|| VIEWERS.with_borrow(|viewers| viewers.last().cloned()))
}

/// Who an effect invocation runs for, kept while it runs: what its handler
/// and completion submit is applied for the same invoker, in the same
/// context, and only as far as the invoker was admitted.
#[derive(Clone, Debug)]
pub(crate) struct Continuation {
    pub(crate) invoker: tilo_actions::ActionInvoker,
    pub(crate) context: Option<tilo_actions::ActionContext>,
    /// What the invoker was admitted for: the effect's own requirement for
    /// its arguments, which the admitting host checked the invoker holds.
    pub(crate) admitted: tilo_actions::PermissionRequirement,
    /// Why an edit the effect submitted was refused: the effect then fails.
    pub(crate) refused: Option<tilo_actions::ActionFailure>,
    /// The edits it submitted that the host is still admitting, by request.
    pub(crate) outstanding: std::collections::HashSet<u64>,
    /// The effect's answer, held until those edits are admitted or refused:
    /// the wire id to send it under, and the answer.
    pub(crate) deferred: Option<(u64, tilo_actions::InvocationUpdate)>,
    pub(crate) terminal_guard: Option<TerminalPublicationGuard>,
}

impl Continuation {
    /// Enter the invoker's scope for one edit the effect submitted.
    pub(crate) fn enter(&self) -> (InvokerScope, ContinuationScope) {
        (
            InvokerScope::enter(self.invoker.clone()),
            ContinuationScope::enter(self.admitted),
        )
    }

    /// Why the effect fails when the edit it submitted was refused with
    /// `error`: a refusal keeps its own code.
    pub(crate) fn failure(error: &tilo_actions::DoorError) -> tilo_actions::ActionFailure {
        let (code, message) = match error {
            tilo_actions::DoorError::Unavailable { code, reason } => (code.clone(), reason.clone()),
            error => (
                crate::bound_dispatch::door_error_code(error).to_owned(),
                error.to_string(),
            ),
        };
        tilo_actions::ActionFailure {
            code,
            message,
            details: None,
            retryable: false,
        }
    }

    /// The effect's terminal `update`, failed when an edit it submitted was
    /// refused.
    pub(crate) fn settle(
        &self,
        update: tilo_actions::InvocationUpdate,
    ) -> tilo_actions::InvocationUpdate {
        match (update, &self.refused) {
            (tilo_actions::InvocationUpdate::Succeeded { invocation, .. }, Some(error)) => {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: error.clone(),
                }
            }
            (update, _) => update,
        }
    }
}

thread_local! {
    static CONTINUATION_BOUNDS: std::cell::RefCell<Vec<tilo_actions::PermissionRequirement>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// An edit an effect submitted on its invoker's behalf is being applied on
/// this thread: it may need no more than the invoker was admitted for.
#[must_use = "the bound applies only while the guard is alive"]
pub(crate) struct ContinuationScope(());
impl ContinuationScope {
    pub(crate) fn enter(admitted: tilo_actions::PermissionRequirement) -> Self {
        CONTINUATION_BOUNDS.with_borrow_mut(|bounds| bounds.push(admitted));
        Self(())
    }
}
impl Drop for ContinuationScope {
    fn drop(&mut self) {
        CONTINUATION_BOUNDS.with_borrow_mut(|bounds| {
            bounds.pop();
        });
    }
}

/// Refuse an edit that needs `needed` while applying an effect's edit for
/// an invoker admitted for less: an effect never lends its invoker more
/// than they hold.
///
/// # Errors
/// [`tilo_actions::DoorError::Unavailable`] with code `permission_denied`.
pub(crate) fn check_continuation(
    spec: &tilo_actions::ActionSpec,
    needed: tilo_actions::PermissionRequirement,
) -> Result<(), tilo_actions::DoorError> {
    let admitted = CONTINUATION_BOUNDS.with_borrow(|bounds| bounds.last().copied());
    match admitted {
        Some(admitted) if !admitted.covers(needed) => Err(tilo_actions::DoorError::Unavailable {
            code: "permission_denied".into(),
            reason: format!("the invoker may not {} here", spec.label.to_lowercase()),
        }),
        _ => Ok(()),
    }
}

/// The invoker of the delivered invocation being resolved on this thread.
/// `None` outside any delivered invocation (the local person acting).
pub fn current_invoker() -> Option<tilo_actions::ActionInvoker> {
    CURRENT_INVOKERS.with_borrow(|invokers| invokers.last().cloned())
}

/// The participant whose view the current resolution builds, if any.
pub fn current_participant() -> Option<tilo_actions::ParticipantId> {
    CURRENT_INVOKERS.with_borrow(|invokers| {
        invokers
            .last()
            .and_then(|invoker| invoker.participant.clone())
    })
}

/// Whether `actor` may invoke the View operation `contract` here. The local
/// person always may. A remote actor may only when it is the current host-set
/// invoker, it has its own participant view, and the operation declares that
/// it changes only the invoking participant's view
/// ([`tilo_actions::PARTICIPANT_VIEW_TAG`]). Every other View operation would
/// move somebody else's private view and stays refused.
pub fn view_operation_permitted(
    actor: &str,
    local_author: &str,
    contract: &tilo_actions::ActionContract,
) -> bool {
    actor == local_author
        || (contract.is_participant_view()
            && current_invoker()
                .is_some_and(|invoker| invoker.actor == actor && invoker.participant.is_some()))
}

/// Inert declaration retained by the actual host at original admission. This
/// is not authority, and is never decoded from a caller's wire request.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedInvocationSpec {
    pub kind: tilo_actions::OperationKind,
    pub contract: tilo_actions::ActionContract,
    pub params: Vec<tilo_actions::ActionParam>,
    pub input: tilo_actions::InputSpec,
    pub directory_revision: Option<String>,
    /// Explicit request for the current handler at one exact semantic address,
    /// retaining its complete public contract rather than an old callback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_binding: Option<tilo_actions::remote::FreshActionBinding>,
}

/// An opaque check minted by the actual host for this exact invocation.
/// Hosts must retain original caller authority and resample their registered
/// policy time on every check. A positive result is never a reusable decision.
#[derive(Clone)]
pub struct PublishedInvocationLease {
    request: PublishedInvocationRequest,
    route: crate::collaboration::RouteSpec,
    check: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    owner_token: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
    dispatch_spec: Option<PublishedInvocationSpec>,
}
impl PublishedInvocationLease {
    pub fn new(
        request: PublishedInvocationRequest,
        route: crate::collaboration::RouteSpec,
        check: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Self {
        Self {
            request,
            route,
            check,
            owner_token: None,
            dispatch_spec: None,
        }
    }
    /// Attach once from the host's immutable original offered declaration.
    /// The worker separately compares its current resolved handler contract.
    pub fn with_dispatch_spec(mut self, spec: PublishedInvocationSpec) -> Result<Self, String> {
        if self.dispatch_spec.is_some() {
            return Err("original invocation dispatch spec cannot be replaced".into());
        }
        self.dispatch_spec = Some(spec);
        Ok(self)
    }
    /// Attach the concrete host's privately constructed immutable original
    /// invocation facts. Presence is not authorization: the final owner must
    /// validate its own private type against already-held current authority.
    pub fn with_owner_token(
        mut self,
        token: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Self, String> {
        if self.owner_token.is_some() {
            return Err("original invocation owner token cannot be replaced".into());
        }
        self.owner_token = Some(token);
        Ok(self)
    }
    pub fn check(&self) -> Result<(), String> {
        (self.check)()
    }
    pub(crate) fn dispatch_spec(&self) -> Result<&PublishedInvocationSpec, String> {
        self.dispatch_spec.as_ref().ok_or_else(|| {
            "required invocation has no original admitted dispatch specification".into()
        })
    }
    pub(crate) fn matches(&self, request: &PublishedInvocationRequest) -> bool {
        self.request == *request
    }
    pub(crate) fn same_invocation(&self, other: &Self) -> bool {
        self.request == other.request
            && self.route == other.route
            && self.dispatch_spec == other.dispatch_spec
            && std::sync::Arc::ptr_eq(&self.check, &other.check)
            && match (&self.owner_token, &other.owner_token) {
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}
impl std::fmt::Debug for PublishedInvocationLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublishedInvocationLease(..)")
    }
}

pub type PublishedInvocationAuthorizer = std::sync::Arc<
    dyn Fn(&PublishedInvocationRequest) -> Result<PublishedInvocationLease, String> + Send + Sync,
>;

#[derive(Default)]
pub(crate) struct PublishedInvocationRuntime {
    pub(crate) required: bool,
    pub(crate) authorizer: Option<PublishedInvocationAuthorizer>,
    pub(crate) failure: Option<String>,
}

use crate::{
    collaboration::RouteSpec,
    history_mode::HistoryOwner,
    record::{RecordAdmission, RecordCheckout, RecordId, RecordStore},
};
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Local evidence of the target resolved at dispatch, before encoding/execution.
///
/// The SDK captures this from the original caller's scope and actual record
/// loan. It contains no action arguments, result, anchor or pixels. It is not
/// a receipt of completion, portable authentication, or permission to execute.
/// A consumer must separately qualify any anchor and successful presentation
/// against these committed heads and the current owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationLocusBasis {
    invocation: tilo_actions::InvocationId,
    actor: String,
    participant: Option<tilo_actions::ParticipantId>,
    publication: tilo_actions::ActionAddress,
    target: crate::bound_dispatch::BoundActionAddress,
    owner_lifetime: uuid::Uuid,
    checkout: RecordId,
    route: RouteSpec,
    heads: Vec<ChangeHash>,
}

impl InvocationLocusBasis {
    pub fn invocation(&self) -> tilo_actions::InvocationId {
        self.invocation
    }
    pub fn actor(&self) -> &str {
        &self.actor
    }
    pub fn participant(&self) -> Option<&tilo_actions::ParticipantId> {
        self.participant.as_ref()
    }
    /// Original host-admitted address; it may be a publication alias.
    pub fn publication(&self) -> &tilo_actions::ActionAddress {
        &self.publication
    }
    /// Exact component-local address resolved by the SDK's publication registry.
    pub fn target(&self) -> &tilo_actions::ActionAddress {
        &self.target.address
    }
    pub fn instance(&self) -> crate::application::InstanceId {
        self.target.instance_id
    }
    pub fn owner_lifetime(&self) -> uuid::Uuid {
        self.owner_lifetime
    }
    /// Actual local checkout identity, not an inferred wire record ID.
    pub fn checkout(&self) -> &RecordId {
        &self.checkout
    }
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }
    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads
    }
}

#[derive(Default)]
enum InvocationLocusState {
    #[default]
    Unobserved,
    Unavailable,
    Captured(Box<InvocationLocusBasis>),
}

const MAX_LOCUS_HEADS: usize = 128;
const MAX_LOCUS_FIELD_BYTES: usize = 4096;
const MAX_LOCUS_BASIS_BYTES: usize = 16 * 1024;

/// Immutable job custody, constructed only from an actual admitted invocation
/// and the exact mounted record owner. InvocationId alone cannot construct it.
#[derive(Clone)]
pub struct ResourceInvocationScope {
    owner: HistoryOwner,
    admission: RecordAdmission,
    lease: PublishedInvocationLease,
    provenance: Rc<RefCell<InvocationProvenance>>,
    retired: Arc<AtomicBool>,
    locus_target: Option<crate::bound_dispatch::BoundActionAddress>,
    locus_basis: Rc<RefCell<InvocationLocusState>>,
}

/// Private observation/publication guard. The shared effect scope remains
/// retired; this cannot loan a record, enter an effect or revive execution.
/// The callback shares the original host admission for the bounded terminal
/// lifetime; dropping this guard releases that handle without changing custody.
#[derive(Clone)]
pub(crate) struct TerminalPublicationGuard {
    owner: HistoryOwner,
    check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}
impl std::fmt::Debug for TerminalPublicationGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TerminalPublicationGuard(..)")
    }
}
impl TerminalPublicationGuard {
    pub(crate) fn check(&self) -> Result<(), String> {
        self.owner
            .check_invocation_lifetime()
            .and_then(|()| (self.check)())
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        // Only shared owner/check handles: no request, prompt, declaration,
        // provenance or executable scope is cloned into this private guard.
        std::mem::size_of::<Self>()
    }
}

impl ResourceInvocationScope {
    pub(crate) fn terminal_publication_guard(&self) -> TerminalPublicationGuard {
        TerminalPublicationGuard {
            owner: self.owner.clone(),
            check: self.lease.check.clone(),
        }
    }
    pub(crate) fn for_owner(
        owner: HistoryOwner,
        lease: PublishedInvocationLease,
    ) -> Result<Self, String> {
        owner.validate().map_err(|error| error.to_string())?;
        if lease.owner_token.is_none() {
            return Err("required invocation has no concrete final-owner custody token".into());
        }
        if owner.broker_route().as_ref() != Some(&lease.route) {
            return Err("invocation belongs to a different canonical record route".into());
        }
        let (store, checkout) = owner.store_checkout();
        let admission = store
            .admission(checkout)
            .map_err(|error| error.to_string())?;
        let provenance = store
            .invocation_provenance(&checkout.id)
            .map_err(|error| error.to_string())?;
        if provenance.borrow().route.as_ref() != Some(&lease.route) {
            return Err(
                "canonical invocation provenance was not registered before construction".into(),
            );
        }
        lease.check()?;
        Ok(Self {
            owner,
            admission,
            lease,
            provenance,
            retired: Arc::new(AtomicBool::new(false)),
            locus_target: None,
            locus_basis: Rc::new(RefCell::new(InvocationLocusState::Unobserved)),
        })
    }

    /// Retain the exact target while the SDK still holds its original alias
    /// resolution. Later dispatch cannot qualify a different same-owner action.
    pub(crate) fn for_bound_target(
        target: crate::bound_dispatch::BoundActionTarget,
        lease: PublishedInvocationLease,
    ) -> Result<Self, String> {
        if target.address.instance_id != target.binding.instance_id {
            return Err("invocation target instance disagrees with owner".into());
        }
        let mut scope = Self::for_owner(HistoryOwner::component(target.binding), lease)?;
        if target.address.address.resource.len() <= MAX_LOCUS_FIELD_BYTES
            && target.address.address.action.len() <= MAX_LOCUS_FIELD_BYTES
        {
            scope.locus_target = Some(target.address);
        } else {
            *scope.locus_basis.borrow_mut() = InvocationLocusState::Unavailable;
        }
        Ok(scope)
    }

    /// Read the immutable pre-execution observation while original custody is
    /// live. No basis means this invocation could not be observed truthfully.
    /// This never commits, borrows a document, refreshes timestamps or executes.
    pub fn locus_basis(&self) -> Result<Option<InvocationLocusBasis>, String> {
        self.check()?;
        Ok(match &*self.locus_basis.borrow() {
            InvocationLocusState::Captured(basis) => Some(basis.as_ref().clone()),
            _ => None,
        })
    }

    /// Best-effort metadata at actual bound dispatch; failure never changes the
    /// action result. Once unavailable it cannot be repaired by a later event.
    pub(crate) fn capture_locus_basis(
        &self,
        target: &crate::bound_dispatch::BoundActionTarget,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        document: &AutoCommit,
    ) {
        let request = &self.lease.request;
        let valid = self.locus_target.as_ref() == Some(&target.address)
            && self
                .owner
                .same_owner(&HistoryOwner::component(target.binding.clone()))
            && self.owner.lifetime_id() == Some(target.binding.lifetime_id)
            && invocation == request.invocation
            && actor == request.actor
            && current_invoker().as_ref() == Some(&request.invoker())
            && self.check_read_loan(document).is_ok();
        if !valid {
            *self.locus_basis.borrow_mut() = InvocationLocusState::Unavailable;
            return;
        }
        if !matches!(*self.locus_basis.borrow(), InvocationLocusState::Unobserved) {
            return;
        }
        // Never call get_heads here: observation must not close an open edit.
        let mut heads = document.base_heads();
        if document.pending_ops() != 0 || heads.len() > MAX_LOCUS_HEADS {
            *self.locus_basis.borrow_mut() = InvocationLocusState::Unavailable;
            return;
        }
        let fields = [
            request.actor.as_str(),
            request
                .participant
                .as_ref()
                .map_or("", tilo_actions::ParticipantId::as_str),
            request.address.resource.as_str(),
            request.address.action.as_str(),
            target.address.address.resource.as_str(),
            target.address.address.action.as_str(),
            target.binding.checkout.id.as_str(),
            self.lease.route.app_id.as_str(),
            self.lease.route.record.as_deref().unwrap_or(""),
        ];
        let bytes = fields
            .iter()
            .try_fold(128usize + heads.len() * 32, |sum, field| {
                (field.len() <= MAX_LOCUS_FIELD_BYTES)
                    .then(|| sum.checked_add(field.len()))
                    .flatten()
            });
        if bytes.is_none_or(|bytes| bytes > MAX_LOCUS_BASIS_BYTES) || self.check().is_err() {
            *self.locus_basis.borrow_mut() = InvocationLocusState::Unavailable;
            return;
        }
        heads.sort_unstable();
        *self.locus_basis.borrow_mut() =
            InvocationLocusState::Captured(Box::new(InvocationLocusBasis {
                invocation,
                actor: request.actor.clone(),
                participant: request.participant.clone(),
                publication: request.address.clone(),
                target: target.address.clone(),
                owner_lifetime: target.binding.lifetime_id,
                checkout: target.binding.checkout.id.clone(),
                route: self.lease.route.clone(),
                heads,
            }));
    }

    /// Compare inert original admission facts with this exact current handler.
    /// This does not invoke a policy callback or acquire an owner/store lock.
    pub(crate) fn check_dispatch_spec(
        &self,
        current: &tilo_actions::ActionSpec,
    ) -> Result<(), tilo_actions::DoorError> {
        let refused = |reason: String| tilo_actions::DoorError::Unavailable {
            code: "published_invocation_contract_changed".into(),
            reason,
        };
        let original = self.lease.dispatch_spec().map_err(refused)?;
        let input_matches = match (&original.input, &current.input) {
            (
                tilo_actions::InputSpec::None,
                tilo_actions::ActionInput::Immediate(_)
                | tilo_actions::ActionInput::Deferred { .. },
            ) => true,
            (
                tilo_actions::InputSpec::Text {
                    prompt,
                    placeholder,
                    collects,
                },
                tilo_actions::ActionInput::Prompted {
                    prompt: actual_prompt,
                    placeholder: actual_placeholder,
                    collects: actual_collects,
                    ..
                },
            ) => {
                prompt == actual_prompt
                    && placeholder == actual_placeholder
                    && collects == actual_collects
            }
            _ => false,
        };
        if original.kind != current.kind
            || original.contract != current.contract
            || original.params != current.params()
            || !input_matches
        {
            return Err(refused(
                "current handler differs from the original admitted declaration".into(),
            ));
        }
        Ok(())
    }

    pub fn invocation(&self) -> tilo_actions::InvocationId {
        self.lease.request.invocation
    }

    /// Queue only a check ID under this exact admitted effect's record route.
    /// The host resolves commands and execution bounds and rechecks custody.
    ///
    /// # Errors
    /// Refuses absent transport, changed custody, malformed IDs or a full lane.
    pub fn approved_check(&self, check_id: &str) -> Result<ApprovedCheckFuture, String> {
        self.check()?;
        #[cfg(feature = "broker-transport")]
        {
            let scope = self.effect_scope()?;
            let request = runtime_collab::approved_checks::Request {
                version: 1,
                invocation: self.invocation().0.to_string(),
                check_id: check_id.into(),
            };
            let receive = crate::collaboration::approved_checks::enqueue(
                &self.route().channel_id(),
                request,
            )?;
            Ok(Box::pin(async move {
                let receipt = receive
                    .await
                    .map_err(|_| "approved-check transport retired")??;
                scope.with_effect(|| Ok(receipt))
            }))
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            let _ = check_id;
            Err("approved project checks require the admitted native broker transport".into())
        }
    }

    /// The canonical record route construction verified against the owner.
    pub(crate) fn route(&self) -> &RouteSpec {
        &self.lease.route
    }

    /// Irreversibly retire this job and all derived worker-effect custody.
    /// Cancellation grants no authority and cannot affect another job.
    pub fn cancel(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Check the original caller and the actual mounted owner's lifetime.
    pub fn check(&self) -> Result<(), String> {
        if self.retired.load(Ordering::Acquire) {
            return Err("original invocation scope has retired".into());
        }
        let result = self
            .owner
            .check_invocation_lifetime()
            .and_then(|()| self.lease.check());
        if result.is_err() {
            self.retired.store(true, Ordering::Release);
        }
        result?;
        // The original policy callback may wait for its owner. A cancellation
        // or concurrent failed check completed during that wait must win.
        if self.retired.load(Ordering::Acquire) {
            return Err("original invocation scope has retired".into());
        }
        Ok(())
    }

    /// A delayed job must enter this callback immediately before its effect.
    pub fn with_effect<R>(&self, effect: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
        self.check()?;
        effect()
    }

    /// Derive worker-thread effect custody from this actual admitted scope.
    /// The host's original lease checks exact worker/origin liveness and current
    /// caller permission. This carries no record loan and cannot mutate a Doc.
    pub fn effect_scope(&self) -> Result<ResourceEffectScope, String> {
        self.check()?;
        Ok(ResourceEffectScope {
            lease: self.lease.clone(),
            retired: self.retired.clone(),
        })
    }

    /// Verify the original caller and that `document` is the record store's
    /// current loan of this scope's exact record.
    pub(crate) fn check_read_loan(&self, document: &AutoCommit) -> Result<(), String> {
        self.check()?;
        let (store, checkout) = self.owner.store_checkout();
        if !record_loan_matches(store, checkout, document)
            || !self.admission.matches(document, checkout)
        {
            return Err("invocation does not hold the actual canonical record loan".into());
        }
        Ok(())
    }

    /// Mutate only the actual currently borrowed canonical record. The loan
    /// witness identifies storage; this explicit scope supplies caller custody.
    /// Partial changes on an Err retain their original contribution as well.
    pub fn with_document_mutation<R>(
        &self,
        document: &mut AutoCommit,
        mutate: impl FnOnce(&mut AutoCommit) -> Result<R, String>,
    ) -> Result<R, String> {
        self.check()?;
        let (store, checkout) = self.owner.store_checkout();
        if !record_loan_matches(store, checkout, document)
            || !self.admission.matches(document, checkout)
        {
            return Err("invocation does not hold the actual canonical record loan".into());
        }
        self.provenance
            .borrow_mut()
            .begin_mutation(document, &self.lease)?;
        let result = mutate(document);
        self.provenance
            .borrow_mut()
            .end_mutation(document, &self.lease)?;
        // Retain metadata even if authority elapsed inside the callback.
        result.and_then(|value| self.check().map(|()| value))
    }

    /// Retain only an actual successful executor action's exact change range.
    /// This private SDK seam is descriptive: the final owner still validates
    /// the original invocation, typed operation and resulting document fact.
    pub(crate) fn retain_executed_action(
        &self,
        document: &mut AutoCommit,
        envelope: &tilo_actions::ActionEnvelope,
    ) -> Result<(), String> {
        self.check_read_loan(document)?;
        let hashes = document
            .get_changes(&envelope.base_change_hashes())
            .into_iter()
            .map(|change| change.hash())
            .collect();
        self.provenance
            .borrow_mut()
            .record_action(hashes, &self.lease, envelope)
    }
}

impl std::fmt::Debug for ResourceInvocationScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResourceInvocationScope(..)")
    }
}

#[derive(Clone, Debug)]
pub struct ResourceEffectScope {
    lease: PublishedInvocationLease,
    retired: Arc<AtomicBool>,
}
impl ResourceEffectScope {
    /// Recheck immediately before the actual delayed worker-side effect.
    /// Call without holding the final owner's Store guard; this callback may
    /// acquire that owner to evaluate the original caller's current permission.
    pub fn with_effect<R>(&self, effect: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
        if self.retired.load(Ordering::Acquire) {
            return Err("original invocation scope has retired".into());
        }
        if let Err(error) = self.lease.check() {
            self.retired.store(true, Ordering::Release);
            return Err(error);
        }
        // Admission linearizes at this final observation after the callback.
        // Cancellation does not interrupt an effect already admitted here.
        if self.retired.load(Ordering::Acquire) {
            return Err("original invocation scope has retired".into());
        }
        effect()
    }
}

/// Record-owned metadata. Required mode is installed before the app factory,
/// from the actual admitted record; it can never reset its trusted baseline.
#[derive(Default)]
pub(crate) struct InvocationProvenance {
    route: Option<RouteSpec>,
    baseline: Vec<ChangeHash>,
    observed: BTreeSet<ChangeHash>,
    contributions: HashMap<ChangeHash, RetainedContribution>,
    // Only explicit scoped callbacks enter this stack. Before entering a nested
    // scope, settle the outer callback's actual segment under its own lease.
    // No worker-global or implicit callback can obtain attribution from it.
    mutations: Vec<MutationSpan>,
}

struct RetainedContribution {
    lease: PublishedInvocationLease,
    committed: Arc<AtomicBool>,
    action: Option<Arc<RetainedAction>>,
}

struct RetainedAction {
    envelope: tilo_actions::ActionEnvelope,
    encoded_bytes: usize,
}
struct MutationSpan {
    cursor: Vec<ChangeHash>,
    lease: PublishedInvocationLease,
}

impl InvocationProvenance {
    pub(crate) fn is_required(&self) -> bool {
        self.route.is_some()
    }
    fn settle_mutation(&mut self, document: &mut AutoCommit) -> Result<(), String> {
        let Some(span) = self.mutations.last() else {
            return Ok(());
        };
        let lease = span.lease.clone();
        let before = span.cursor.clone();
        let heads = document.get_heads();
        let changed = document
            .get_changes(&before)
            .into_iter()
            .map(|change| change.hash())
            .collect();
        self.record(changed, &lease)?;
        if let Some(span) = self.mutations.last_mut() {
            span.cursor = heads;
        }
        Ok(())
    }

    fn begin_mutation(
        &mut self,
        document: &mut AutoCommit,
        lease: &PublishedInvocationLease,
    ) -> Result<(), String> {
        self.settle_mutation(document)?;
        self.prune_committed();
        // Commit an earlier unscoped transaction only so validation can reject
        // it; it is never assigned the incoming caller's authority.
        let cursor = document.get_heads();
        self.validate_existing(document)?;
        // Collapse only after every post-baseline change is known and no
        // original uncommitted contributor or active span remains.
        if self.contributions.is_empty() && self.mutations.is_empty() {
            self.baseline.clone_from(&cursor);
            self.observed.clear();
        }
        self.mutations.push(MutationSpan {
            cursor,
            lease: lease.clone(),
        });
        Ok(())
    }

    fn end_mutation(
        &mut self,
        document: &mut AutoCommit,
        lease: &PublishedInvocationLease,
    ) -> Result<(), String> {
        if !self
            .mutations
            .last()
            .is_some_and(|span| span.lease.same_invocation(lease))
        {
            return Err("invocation mutation stack lost its exact contributor".into());
        }
        self.settle_mutation(document)?;
        self.mutations.pop();
        if let Some(outer) = self.mutations.last_mut() {
            outer.cursor = document.base_heads();
        }
        Ok(())
    }
    pub(crate) fn require(
        &mut self,
        route: RouteSpec,
        document: &mut AutoCommit,
    ) -> Result<(), String> {
        if let Some(current) = &self.route {
            if current != &route {
                return Err("record invocation route cannot be rebound".into());
            }
            return self.validate_existing(document);
        }
        if document.pending_ops() != 0 {
            return Err("canonical baseline has pending unadmitted edits".into());
        }
        self.baseline = document.base_heads();
        self.route = Some(route);
        Ok(())
    }

    fn validate_existing(&self, document: &mut AutoCommit) -> Result<(), String> {
        if self.route.is_none() {
            return Err("required record provenance is absent".into());
        }
        if document.get_changes(&self.baseline).iter().any(|change| {
            let hash = change.hash();
            !self.observed.contains(&hash) && !self.contributions.contains_key(&hash)
        }) {
            return Err("canonical changes have no original invocation contributor".into());
        }
        Ok(())
    }

    fn record(
        &mut self,
        hashes: Vec<ChangeHash>,
        lease: &PublishedInvocationLease,
    ) -> Result<(), String> {
        if self.route.as_ref() != Some(&lease.route) {
            return Err("contribution route mismatch".into());
        }
        for hash in hashes {
            // Nested scoped work retains its own caller, never the outer caller.
            self.contributions
                .entry(hash)
                .or_insert_with(|| RetainedContribution {
                    lease: lease.clone(),
                    committed: Arc::new(AtomicBool::new(false)),
                    action: None,
                });
        }
        Ok(())
    }

    fn record_action(
        &mut self,
        hashes: Vec<ChangeHash>,
        lease: &PublishedInvocationLease,
        envelope: &tilo_actions::ActionEnvelope,
    ) -> Result<(), String> {
        let encoded_bytes = serde_json::to_vec(envelope)
            .map_err(|_| "executed action correlation encoding failed")?
            .len();
        // Match the existing document transport ceiling, rather than silently
        // narrowing otherwise admitted typed document operations.
        if encoded_bytes > collab_types::MAX_DOCUMENT_TRANSFER_BYTES {
            return Err("executed action correlation exceeds document capacity".into());
        }
        let mut identities = std::collections::HashSet::new();
        let mut retained_bytes = encoded_bytes;
        for action in self
            .contributions
            .values()
            .filter_map(|entry| entry.action.as_ref())
        {
            if identities.insert(Arc::as_ptr(action)) {
                retained_bytes = retained_bytes
                    .checked_add(action.encoded_bytes)
                    .ok_or("executed action correlation capacity exhausted")?;
            }
        }
        if retained_bytes > collab_types::MAX_DOCUMENT_TRANSFER_BYTES {
            return Err("retained executed action correlations exceed document capacity".into());
        }
        self.record(hashes.clone(), lease)?;
        for hash in &hashes {
            let retained = self
                .contributions
                .get(hash)
                .ok_or("executed action lost its exact contribution")?;
            if !retained.lease.same_invocation(lease) || retained.action.is_some() {
                return Err("executed action overlaps another contribution".into());
            }
        }
        let action = Arc::new(RetainedAction {
            envelope: envelope.clone(),
            encoded_bytes,
        });
        for hash in hashes {
            let retained = self
                .contributions
                .get_mut(&hash)
                .ok_or("executed action lost its exact contribution")?;
            retained.action = Some(action.clone());
        }
        Ok(())
    }

    fn prune_committed(&mut self) {
        self.contributions.retain(|hash, contribution| {
            if contribution.committed.load(Ordering::Acquire) {
                self.observed.insert(*hash);
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn observe_admitted(&mut self, hashes: &[ChangeHash]) {
        if self.route.is_some() {
            self.observed.extend(hashes.iter().copied());
        }
    }

    pub(crate) fn capture(
        &self,
        document: &mut AutoCommit,
        from: &[ChangeHash],
        mutation: &collab_types::DocumentMutation,
    ) -> Result<Option<DocumentAuthorization>, String> {
        let Some(route) = &self.route else {
            return Ok(None);
        };
        if document.pending_ops() != 0 {
            return Err("publication has unfinished changes".into());
        }
        self.validate_existing(document)?;
        let exported = document
            .get_changes(from)
            .into_iter()
            .map(|change| change.hash())
            .collect::<BTreeSet<_>>();
        let mut contributions: Vec<Contribution> = vec![];
        let mut acknowledgements = vec![];
        for hash in &exported {
            if let Some(retained) = self.contributions.get(hash) {
                if retained.committed.load(Ordering::Acquire) {
                    continue;
                }
                acknowledgements.push(retained.committed.clone());
                let lease = &retained.lease;
                if let Some(group) = contributions.iter_mut().find(|group| {
                    group.lease.same_invocation(lease)
                        && match (&group.action, &retained.action) {
                            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                            (None, None) => true,
                            _ => false,
                        }
                }) {
                    group.hashes.insert(*hash);
                } else {
                    contributions.push(Contribution {
                        lease: lease.clone(),
                        hashes: [*hash].into(),
                        action: retained.action.clone(),
                    });
                }
            }
        }
        Ok(Some(DocumentAuthorization(Arc::new(
            DocumentAuthorizationInner {
                route: route.clone(),
                body: *blake3::hash(&mutation.encode().map_err(|error| error.to_string())?)
                    .as_bytes(),
                from: from.to_vec(),
                to: document.base_heads(),
                exported,
                contributions,
                acknowledgements,
            },
        ))))
    }
}

struct Contribution {
    lease: PublishedInvocationLease,
    hashes: BTreeSet<ChangeHash>,
    action: Option<Arc<RetainedAction>>,
}
struct DocumentAuthorizationInner {
    route: RouteSpec,
    body: [u8; 32],
    from: Vec<ChangeHash>,
    to: Vec<ChangeHash>,
    exported: BTreeSet<ChangeHash>,
    contributions: Vec<Contribution>,
    #[cfg_attr(
        not(feature = "broker-transport"),
        allow(dead_code, reason = "only the broker transport acknowledges commits")
    )]
    acknowledgements: Vec<Arc<AtomicBool>>,
}

/// Immutable local publication evidence. It is never serialized or minted from
/// wire fields. Captured original guards outlive terminal invocation tracking.
#[derive(Clone)]
pub struct DocumentAuthorization(Arc<DocumentAuthorizationInner>);

/// Borrowed descriptive facts from the actual retained SDK execution span.
/// Only the final owner can validate the opaque original token; neither these
/// fields nor a typed action alone grants document/input admission authority.
pub struct OwnerActionContribution<'a> {
    pub request: &'a PublishedInvocationRequest,
    pub route: &'a RouteSpec,
    pub original: &'a (dyn std::any::Any + Send + Sync),
    pub changes: &'a BTreeSet<ChangeHash>,
    pub action: Option<&'a tilo_actions::ActionEnvelope>,
}
impl std::fmt::Debug for DocumentAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DocumentAuthorization(..)")
    }
}
impl DocumentAuthorization {
    /// Inert captured frontiers for the concrete bounded body/range evaluator.
    /// These are not a substitute for its actual parsed causal-coverage proof.
    pub fn frontiers(&self) -> (&[ChangeHash], &[ChangeHash]) {
        (&self.0.from, &self.0.to)
    }
    /// Check the concrete committer's returned operation against the exact
    /// captured body (including its encoded causal dependencies) and route.
    /// Actual bounded graph/basis admission is still the final owner's job.
    #[cfg_attr(
        not(feature = "broker-transport"),
        allow(dead_code, reason = "only the broker transport commits documents")
    )]
    pub(crate) fn check_committed_operation(
        &self,
        route: &RouteSpec,
        operation: &collab_types::SignedOp,
    ) -> Result<(), String> {
        let room = route.cluster_id.to_string();
        let record = route.record.as_deref().unwrap_or("");
        if route != &self.0.route
            || operation.encryption.is_some()
            || operation.op_class != collab_types::OpClass::Splice
            || operation.artifact
                != collab_types::document_artifact_id(&route.app_id, &room, record)
            || operation.attestation_id
                != collab_types::document_attestation_id(&route.app_id, &room, record)
            || blake3::hash(&operation.payload).as_bytes() != &self.0.body
        {
            return Err("scoped owner returned a different document operation".into());
        }
        Ok(())
    }

    // Private SDK-only confirmation, after exact scoped durable success and
    // matching receipt handoff. A handler or terminal receipt cannot set it.
    #[cfg_attr(
        not(feature = "broker-transport"),
        allow(dead_code, reason = "only the broker transport commits documents")
    )]
    pub(crate) fn acknowledge_committed(&self) {
        for acknowledgement in &self.0.acknowledgements {
            acknowledgement.store(true, Ordering::Release);
        }
    }

    /// This verifies body binding and current contributors, not graph coverage.
    /// The concrete owner must ALSO call validate_owner_base with the actual
    /// bounded admission/parser's complete mutation range under its owner turn.
    pub fn check(
        &self,
        route: &RouteSpec,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), String> {
        self.check_binding(route, mutation)?;
        for group in &self.0.contributions {
            if &group.lease.route != route {
                return Err("document contributor route mismatch".into());
            }
            group.lease.check()?;
        }
        Ok(())
    }

    /// Structural binding only: safe under the concrete owner's existing lock.
    /// This never invokes a lifecycle callback or opens authority/storage.
    pub fn check_binding(
        &self,
        route: &RouteSpec,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), String> {
        if route != &self.0.route
            || blake3::hash(&mutation.encode().map_err(|error| error.to_string())?).as_bytes()
                != &self.0.body
        {
            return Err("document authorization body or route mismatch".into());
        }
        Ok(())
    }

    /// `actual` comes from the bounded parser/admission of the exact body, not
    /// these metadata fields. Unattributed changes must already exist in the
    /// actual committed owner history. An omitted or invented range refuses.
    pub fn validate_owner_base(
        &self,
        actual: &[ChangeHash],
        mut already_committed: impl FnMut(&ChangeHash) -> bool,
    ) -> Result<(), String> {
        let actual = actual.iter().copied().collect::<BTreeSet<_>>();
        if actual != self.0.exported {
            return Err("document authorization omits or invents actual changes".into());
        }
        for hash in &actual {
            let contributor = self
                .0
                .contributions
                .iter()
                .find(|group| group.hashes.contains(hash));
            if contributor.is_none() && !already_committed(hash) {
                return Err(
                    "document change has neither original caller nor committed owner basis".into(),
                );
            }
        }
        Ok(())
    }

    /// The final owner validates each ORIGINAL private token against its
    /// already-held peer/ledger/time, including these exact delivery inputs and
    /// route. A successful downcast alone must never return authorization.
    /// This method invokes no ordinary lifecycle/Store-reopening callback.
    pub fn validate_owner_contributors(
        &self,
        mut validate: impl FnMut(
            &PublishedInvocationRequest,
            &RouteSpec,
            &(dyn std::any::Any + Send + Sync),
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        for group in &self.0.contributions {
            if group.lease.route != self.0.route {
                return Err("document contributor route mismatch".into());
            }
            let token = group
                .lease
                .owner_token
                .as_ref()
                .ok_or("required contributor has no final-owner custody token")?;
            validate(&group.lease.request, &group.lease.route, token.as_ref())?;
        }
        Ok(())
    }

    /// Validate original contributor custody together with exact executed
    /// action/change correlation. Untyped callbacks remain visible with None;
    /// an owner must never infer a typed action from their labels or JSON.
    ///
    /// # Errors
    /// Missing/mismatched original custody, or any owner validator refusal.
    pub fn validate_owner_action_contributors(
        &self,
        mut validate: impl FnMut(OwnerActionContribution<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        for group in &self.0.contributions {
            if group.lease.route != self.0.route {
                return Err("document contributor route mismatch".into());
            }
            let original = group
                .lease
                .owner_token
                .as_ref()
                .ok_or("required contributor has no final-owner custody token")?;
            validate(OwnerActionContribution {
                request: &group.lease.request,
                route: &group.lease.route,
                original: original.as_ref(),
                changes: &group.hashes,
                action: group.action.as_ref().map(|action| &action.envelope),
            })?;
        }
        Ok(())
    }

    #[cfg_attr(
        not(feature = "broker-transport"),
        allow(dead_code, reason = "only the broker transport commits documents")
    )]
    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<DocumentAuthorizationInner>();
        bytes = bytes
            .checked_add(self.0.from.capacity().checked_mul(32)?)?
            .checked_add(self.0.to.capacity().checked_mul(32)?)?;
        bytes = bytes
            .checked_add(self.0.exported.len().checked_mul(64)?)?
            .checked_add(
                self.0
                    .acknowledgements
                    .capacity()
                    .checked_mul(std::mem::size_of::<Arc<AtomicBool>>())?,
            )?;
        for group in &self.0.contributions {
            bytes = bytes
                .checked_add(std::mem::size_of::<Contribution>())?
                .checked_add(group.hashes.len().checked_mul(64)?)?;
            if let Some(action) = &group.action {
                bytes = bytes
                    .checked_add(std::mem::size_of::<RetainedAction>())?
                    .checked_add(action.encoded_bytes)?;
            }
            let request = &group.lease.request;
            for len in [
                request.address.resource.capacity(),
                request.address.action.capacity(),
                request.actor.capacity(),
                request.argument.as_ref().map_or(0, String::capacity),
                request
                    .participant
                    .as_ref()
                    .map_or(0, |participant| participant.as_str().len()),
            ] {
                bytes = bytes.checked_add(len)?;
            }
        }
        Some(bytes)
    }
}

struct LoanIdentity {
    store: RecordStore,
    record: RecordId,
    document: *const AutoCommit,
}
thread_local! {
    static RECORD_LOANS: RefCell<Vec<LoanIdentity>> = const { RefCell::new(Vec::new()) };
}
struct RecordLoan;
impl Drop for RecordLoan {
    fn drop(&mut self) {
        RECORD_LOANS.with_borrow_mut(|loans| {
            loans.pop();
        });
    }
}
pub(crate) fn with_record_loan<R>(
    store: &RecordStore,
    record: &RecordId,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    RECORD_LOANS.with_borrow_mut(|loans| {
        loans.push(LoanIdentity {
            store: store.clone(),
            record: record.clone(),
            document,
        })
    });
    let _loan = RecordLoan;
    callback(document)
}
fn record_loan_matches(
    store: &RecordStore,
    checkout: &RecordCheckout,
    document: &AutoCommit,
) -> bool {
    RECORD_LOANS.with_borrow(|loans| {
        loans.last().is_some_and(|loan| {
            loan.store.same_owner(store)
                && loan.record == checkout.id
                && std::ptr::eq(loan.document, document)
        })
    })
}

/// The record a disclosed read names: its room when it is shared in one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordStamp {
    pub(crate) room: Option<uuid::Uuid>,
    pub(crate) app_id: String,
    pub(crate) record_id: String,
}

impl From<&RouteSpec> for RecordStamp {
    fn from(route: &RouteSpec) -> Self {
        Self {
            room: Some(route.cluster_id),
            app_id: route.app_id.clone(),
            record_id: route.record.clone().unwrap_or_default(),
        }
    }
}

struct MountedDocument {
    stamp: RecordStamp,
    document: *const AutoCommit,
}
thread_local! {
    static MOUNTED_DOCUMENTS: RefCell<Vec<MountedDocument>> = const { RefCell::new(Vec::new()) };
}
struct MountedLoan;
impl Drop for MountedLoan {
    fn drop(&mut self) {
        MOUNTED_DOCUMENTS.with_borrow_mut(|mounted| {
            mounted.pop();
        });
    }
}

/// Lend `document` to `callback`, identified as the record `stamp` names.
/// Without a stamp the document is lent unidentified, and nothing may
/// disclose it as a record.
fn with_identified_document<R>(
    stamp: Option<RecordStamp>,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    let Some(stamp) = stamp else {
        return callback(document);
    };
    MOUNTED_DOCUMENTS.with_borrow_mut(|mounted| {
        mounted.push(MountedDocument { stamp, document });
    });
    let _loan = MountedLoan;
    callback(document)
}

/// Lend the app's mounted primary document to `callback`, identified as the
/// canonical record on `route`. Without a current primary route the document
/// is lent unidentified.
pub(crate) fn with_mounted_document<R>(
    route: Option<RouteSpec>,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    with_identified_document(route.as_ref().map(RecordStamp::from), document, callback)
}

/// Lend a component's record document to `callback`, identified only when it
/// is the record store's current loan of exactly `checkout`'s document: on
/// its canonical `route` when it has one, else as `app_id`'s local record.
pub(crate) fn with_loaned_record<R>(
    store: &RecordStore,
    checkout: &RecordCheckout,
    app_id: &str,
    route: Option<RouteSpec>,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    let stamp = record_loan_matches(store, checkout, document).then(|| {
        route.as_ref().map_or_else(
            || RecordStamp {
                room: None,
                app_id: app_id.to_string(),
                record_id: checkout.id.as_str().to_string(),
            },
            RecordStamp::from,
        )
    });
    with_identified_document(stamp, document, callback)
}

/// The record the innermost identified document names, when it is exactly
/// `document`.
pub(crate) fn identified_record(document: &AutoCommit) -> Option<RecordStamp> {
    MOUNTED_DOCUMENTS.with_borrow(|mounted| {
        mounted
            .last()
            .filter(|mount| std::ptr::eq(mount.document, document))
            .map(|mount| mount.stamp.clone())
    })
}

/// A scoped chooser or URI request rechecks this admitted caller on the host's
/// worker immediately before the effect.
impl crate::portal::EffectGate for ResourceEffectScope {
    fn with_effect<R>(&self, effect: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
        ResourceEffectScope::with_effect(self, effect)
    }
}

#[cfg(test)]
mod host_effect_tests;
pub(crate) mod native_message;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) use native_message::NativeMessageCapture;
#[cfg(feature = "broker-transport")]
pub(crate) use native_message::NativeMessageStreamLifetime;
pub use native_message::{NativeExecutedContribution, NativeInvocationSource};
