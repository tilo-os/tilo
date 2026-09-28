//! Host-local invocation custody. No wire caller can manufacture these checks.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedInvocationSpec {
    pub kind: tilo_actions::OperationKind,
    pub contract: tilo_actions::ActionContract,
    pub params: Vec<tilo_actions::ActionParam>,
    pub input: tilo_actions::InputSpec,
    pub directory_revision: Option<String>,
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

/// Immutable job custody, constructed only from an actual admitted invocation
/// and the exact mounted record owner. InvocationId alone cannot construct it.
#[derive(Clone)]
pub struct ResourceInvocationScope {
    owner: HistoryOwner,
    admission: RecordAdmission,
    lease: PublishedInvocationLease,
    provenance: Rc<RefCell<InvocationProvenance>>,
    retired: Arc<AtomicBool>,
}

impl ResourceInvocationScope {
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
        })
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
                });
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
                if let Some(group) = contributions
                    .iter_mut()
                    .find(|group| group.lease.same_invocation(lease))
                {
                    group.hashes.insert(*hash);
                } else {
                    contributions.push(Contribution {
                        lease: lease.clone(),
                        hashes: [*hash].into(),
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
#[cfg(test)]
pub(crate) mod tests;
