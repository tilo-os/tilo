//! The one entry through which the retained tree admits an exported effect or
//! view operation: [`start_operation`], answered by the widgets that provide
//! [`ResourceOperations`].

use std::collections::HashSet;

use super::Widget;
use super::capability::{capability, capability_mut};
use crate::invocation_scope::ResourceInvocationScope;
use automerge::{AutoCommit, ObjId};

/// Result of admitting an effect/view operation into a widget.
///
/// `Accepted` is deliberately distinct from success: the widget must later
/// report progress and exactly one terminal [`tilo_actions::InvocationUpdate`]
/// from [`ResourceOperations::poll_resource_operation`]. This keeps queued and streaming
/// work from being reported as completed merely because it entered a queue.
#[derive(Clone, Debug, PartialEq)]
pub enum ResourceOperationStart {
    Completed(tilo_actions::ActionOutput),
    Accepted,
}

impl ResourceOperationStart {
    /// The output of an operation that finished on return; `None` while it
    /// is still pending.
    pub fn completed(self) -> Option<tilo_actions::ActionOutput> {
        match self {
            Self::Completed(output) => Some(output),
            Self::Accepted => None,
        }
    }
}

/// What a handler matches on. Reachable only through
/// [`ResourceOperation::unscoped`] or [`ResourceOperation::with_custody`], so a
/// handler states whether it honours original-caller custody before it can
/// read what it was asked to do.
#[derive(Clone, Copy, Debug)]
pub struct OperationRequest<'a> {
    pub invocation: tilo_actions::InvocationId,
    pub kind: tilo_actions::OperationKind,
    pub type_id: &'a str,
    pub payload: &'a [u8],
}

/// Inert routing facts captured from the actual resolved record owner.
/// These are neither a certificate nor permission to execute a host job.
#[derive(Clone, Debug)]
pub struct OperationTarget {
    route: crate::collaboration::RouteSpec,
    address: tilo_actions::ActionAddress,
}
impl OperationTarget {
    pub(crate) fn new(
        route: crate::collaboration::RouteSpec,
        address: tilo_actions::ActionAddress,
    ) -> Self {
        Self { route, address }
    }
    pub fn route(&self) -> &crate::collaboration::RouteSpec {
        &self.route
    }
    pub fn address(&self) -> &tilo_actions::ActionAddress {
        &self.address
    }

    /// Submit an operation on the exact dispatched record route. The broker
    /// recovers original admission and compares the complete action parameters.
    ///
    /// # Errors
    /// Refuses another action, missing record routing or unavailable transport.
    pub fn external_moderation(
        &self,
        invocation: tilo_actions::InvocationId,
        operation: registry_types::ExternalModerationOperation,
    ) -> Result<crate::invocation_scope::ExternalModerationFuture, String> {
        runtime_collab::external_moderation::validate_action_operation(
            &self.address.action,
            &operation,
        )?;
        if self.address.resource.is_empty()
            || self.route.record.as_deref().is_none_or(str::is_empty)
        {
            return Err("external moderation requires the exact dispatched resource".into());
        }
        #[cfg(feature = "broker-transport")]
        {
            let receive = crate::collaboration::external_moderation::enqueue(
                &self.route.channel_id(),
                invocation,
                operation,
            )?;
            Ok(Box::pin(async move {
                receive
                    .await
                    .map_err(|_| "external moderation transport retired")?
            }))
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            let _ = invocation;
            Err("external moderation requires the admitted native broker transport".into())
        }
    }

    /// Request one approved ID on this dispatch target's acknowledged record route.
    /// The broker recovers actual caller/target authority from the invocation.
    ///
    /// # Errors
    /// Refuses other action types, absent routing, malformed IDs or unavailable transport.
    pub fn approved_check(
        &self,
        invocation: tilo_actions::InvocationId,
        check_id: &str,
    ) -> Result<crate::invocation_scope::ApprovedCheckFuture, String> {
        if self.address.action != runtime_collab::approved_checks::ACTION_ID
            || self.address.resource.is_empty()
            || self.route.record.as_deref().is_none_or(str::is_empty)
        {
            return Err("approved check requires the exact dispatched project resource".into());
        }
        #[cfg(feature = "broker-transport")]
        {
            let request = runtime_collab::approved_checks::Request {
                version: 1,
                invocation: invocation.0.to_string(),
                check_id: check_id.into(),
            };
            let receive =
                crate::collaboration::approved_checks::enqueue(&self.route.channel_id(), request)?;
            Ok(Box::pin(async move {
                receive
                    .await
                    .map_err(|_| "approved-check transport retired")?
            }))
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            let _ = (invocation, check_id);
            Err("approved project checks require the admitted native broker transport".into())
        }
    }
    /// Observe reported phases for the exact dispatched project check.
    ///
    /// # Errors
    /// Refuses other action types, absent routing, malformed IDs or unavailable transport.
    pub fn approved_check_reported(
        &self,
        invocation: tilo_actions::InvocationId,
        check_id: &str,
    ) -> Result<crate::invocation_scope::ApprovedCheckReportedWork, String> {
        if self.address.action != runtime_collab::approved_checks::ACTION_ID
            || self.address.resource.is_empty()
            || self.route.record.as_deref().is_none_or(str::is_empty)
        {
            return Err("approved check requires the exact dispatched project resource".into());
        }
        #[cfg(feature = "broker-transport")]
        {
            crate::invocation_scope::reported_check_work(
                &self.route.channel_id(),
                runtime_collab::approved_checks::Request {
                    version: 1,
                    invocation: invocation.0.to_string(),
                    check_id: check_id.into(),
                },
                None,
            )
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            let _ = (invocation, check_id);
            Err("approved project checks require the admitted native broker transport".into())
        }
    }
}

/// An exported effect/view operation offered to the retained tree.
///
/// A *required* operation — a hosted invocation admitted for its original
/// caller — carries that caller's [`ResourceInvocationScope`]. A handler not
/// written against the scope reads the operation through [`Self::unscoped`],
/// which hides a required operation from it, so routing continues past it
/// exactly as if it had declined.
#[derive(Clone, Copy)]
pub struct ResourceOperation<'a> {
    request: OperationRequest<'a>,
    scope: Option<&'a ResourceInvocationScope>,
    target: Option<&'a OperationTarget>,
}

impl std::fmt::Debug for ResourceOperation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceOperation")
            .field("request", &self.request)
            .field("scoped", &self.scope.is_some())
            .finish()
    }
}

impl<'a> ResourceOperation<'a> {
    /// An operation the host admitted without hosted custody: local input,
    /// the palette, or a directed invocation already admitted by the broker.
    pub fn local(
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &'a str,
        payload: &'a [u8],
    ) -> Self {
        Self {
            request: OperationRequest {
                invocation,
                kind,
                type_id,
                payload,
            },
            scope: None,
            target: None,
        }
    }

    /// A required operation bound to its original caller's custody.
    pub fn scoped(
        scope: &'a ResourceInvocationScope,
        kind: tilo_actions::OperationKind,
        type_id: &'a str,
        payload: &'a [u8],
    ) -> Self {
        Self {
            request: OperationRequest {
                invocation: scope.invocation(),
                kind,
                type_id,
                payload,
            },
            scope: Some(scope),
            target: None,
        }
    }

    pub(crate) fn with_target(mut self, target: Option<&'a OperationTarget>) -> Self {
        self.target = target;
        self
    }

    /// Exact inert record routing facts when the dispatcher observed an owner.
    pub fn target(&self) -> Option<&OperationTarget> {
        self.target
    }

    pub fn invocation(&self) -> tilo_actions::InvocationId {
        self.request.invocation
    }

    pub fn kind(&self) -> tilo_actions::OperationKind {
        self.request.kind
    }

    pub fn is_scoped(&self) -> bool {
        self.scope.is_some()
    }

    /// The operation for a handler that carries no original-caller custody.
    /// `None` for a required operation: such a handler must decline it.
    pub fn unscoped(&self) -> Option<OperationRequest<'a>> {
        self.scope.is_none().then_some(self.request)
    }

    /// The operation for a custody-aware handler. When the scope is present
    /// the handler must retain it with any job it starts and route every
    /// later effect and document mutation through its checked callbacks.
    pub fn with_custody(&self) -> (OperationRequest<'a>, Option<&'a ResourceInvocationScope>) {
        (self.request, self.scope)
    }

    /// Recheck the original caller and owner lifetime of a required
    /// operation. A local operation carries no scope to check.
    pub fn check(&self) -> Result<(), String> {
        self.scope.map_or(Ok(()), ResourceInvocationScope::check)
    }

    /// Offer this operation to `widget`.
    pub fn start(
        &self,
        widget: &mut (impl Widget + ?Sized),
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        start_operation(widget, self, doc, path)
    }
}

/// Offer `op` to `widget` for a caller that grants no document access. A
/// handler that writes anyway is refused. Its bytes are retained: rolling
/// back would also discard the caller's own uncommitted edits.
pub(crate) fn start_without_document_writes(
    widget: &mut (impl Widget + ?Sized),
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    // `get_heads` would commit the caller's open transaction.
    let before = crate::doc::settle_mark(doc);
    let started = start_operation(widget, op, doc, path);
    if crate::doc::settle_mark(doc) != before {
        return Err("a read-only operation wrote the document; bytes were retained".into());
    }
    started
}

/// Offer `op` to each retained child in order and return the first start.
/// This is the default routing; a [`ResourceOperations::start_operation`]
/// that handles some types itself calls it for the rest.
pub fn start_in_children<W: Widget + ?Sized>(
    widget: &mut W,
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    op.check()?;
    start_in(widget.children_mut(), op, doc, path)
}

/// [`start_in_children`] once the children are in hand: not generic, so it
/// is compiled once rather than for every widget type that routes by default.
pub(crate) fn start_in(
    children: Vec<&mut (dyn Widget + '_)>,
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    for child in children {
        // A nested canonical owner is an independent destination. A required
        // operation reaches it only through that owner's checked dispatch frame.
        if op.is_scoped() && child.has_separate_document_owner() {
            continue;
        }
        if let Some(started) = start_operation(child, op, doc, path)? {
            return Ok(Some(started));
        }
    }
    Ok(None)
}

/// A widget that answers exported effect or view operations itself, or
/// routes them its own way. Declared with [`crate::provides!`]. A widget
/// without it, like each method here that is not overridden, routes by
/// default: a deferred local history answers its native Edit, then the
/// retained children are asked in order.
pub trait ResourceOperations: Widget {
    /// Effect/view operation type ids this widget can actually dispatch.
    ///
    /// Implementors of [`Self::start_operation`] list their locally handled
    /// types here, beside it. The host combines these declarations across
    /// the retained tree and refuses to publish an action whose type has no
    /// handler, preventing a catalogue entry that can only fail at runtime.
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        local_history_handlers(self)
    }

    /// Every type this widget and its subtree dispatch.
    fn resource_operation_handlers(&self) -> HashSet<String> {
        subtree_handlers(self, self.local_resource_operation_handlers())
    }

    /// Admit an exported effect or view operation. Document operations never
    /// enter it.
    ///
    /// Return `Ok(None)` to decline so routing continues;
    /// [`ResourceOperationStart::Completed`] with the output the action's
    /// `ActionContract` promises when the work is done on return; or
    /// [`ResourceOperationStart::Accepted`] for work that later reports
    /// through [`Self::poll_resource_operation`].
    ///
    /// Read the operation through [`ResourceOperation::unscoped`] unless the
    /// handler honours a required invocation's custody; a custody-aware
    /// handler reads it through [`ResourceOperation::with_custody`], retains
    /// the scope with any job it starts and uses its checked callbacks at each
    /// later effect or document mutation. Hand operations a widget does not
    /// own to [`start_in_children`].
    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        start_by_default(self, op, doc, path)
    }

    /// Poll one admitted operation without blocking the UI thread.
    ///
    /// `cancellation_requested` is a cooperative signal from the action
    /// broker. Cancellable handlers should stop at a safe boundary and return
    /// `Cancelled`; non-cancellable handlers may ignore it.
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        poll_by_default(self, invocation, cancellation_requested, doc, path)
    }
}

/// The types `widget` dispatches itself.
pub fn local_resource_operation_handlers(
    widget: &(impl Widget + ?Sized),
) -> &'static [&'static str] {
    match capability::<dyn ResourceOperations>(widget) {
        Some(operations) => operations.local_resource_operation_handlers(),
        None => local_history_handlers(widget),
    }
}

/// Every type `widget` and its subtree dispatch.
pub fn resource_operation_handlers(widget: &(impl Widget + ?Sized)) -> HashSet<String> {
    match capability::<dyn ResourceOperations>(widget) {
        Some(operations) => operations.resource_operation_handlers(),
        None => subtree_handlers(widget, local_history_handlers(widget)),
    }
}

/// Offer `op` to `widget`: [`ResourceOperations::start_operation`].
pub fn start_operation(
    widget: &mut (impl Widget + ?Sized),
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    if let Some(operations) = capability_mut::<dyn ResourceOperations>(widget) {
        return operations.start_operation(op, doc, path);
    }
    start_by_default(widget, op, doc, path)
}

/// Poll `widget` for an admitted operation:
/// [`ResourceOperations::poll_resource_operation`].
pub fn poll_resource_operation(
    widget: &mut (impl Widget + ?Sized),
    invocation: tilo_actions::InvocationId,
    cancellation_requested: bool,
    doc: &AutoCommit,
    path: &ObjId,
) -> Option<tilo_actions::InvocationUpdate> {
    if let Some(operations) = capability_mut::<dyn ResourceOperations>(widget) {
        return operations.poll_resource_operation(invocation, cancellation_requested, doc, path);
    }
    poll_by_default(widget, invocation, cancellation_requested, doc, path)
}

fn local_history_handlers(widget: &(impl Widget + ?Sized)) -> &'static [&'static str] {
    if crate::undo::has_deferred_local_history(widget) {
        &[crate::undo::EDIT_OPERATION]
    } else {
        &[]
    }
}

pub(crate) fn subtree_handlers(widget: &(impl Widget + ?Sized), local: &[&str]) -> HashSet<String> {
    let mut handlers = local
        .iter()
        .map(|handler| (*handler).to_string())
        .collect::<HashSet<_>>();
    // The actual widget's registered readers also declare their selectors.
    // Descriptor availability and qualified owner admission remain separate.
    handlers.extend(
        crate::semantic_document_read::readers_of(widget)
            .iter()
            .map(|reader| reader.operation_type().to_string()),
    );
    widget.for_each_child(&mut |child| handlers.extend(resource_operation_handlers(child)));
    handlers
}

pub(crate) fn start_by_default(
    widget: &mut (impl Widget + ?Sized),
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    if let Some(started) = crate::undo::start_local_history(widget, op)? {
        return Ok(Some(started));
    }
    start_in_children(widget, op, doc, path)
}

pub(crate) fn poll_by_default(
    widget: &mut (impl Widget + ?Sized),
    invocation: tilo_actions::InvocationId,
    cancellation_requested: bool,
    doc: &AutoCommit,
    path: &ObjId,
) -> Option<tilo_actions::InvocationUpdate> {
    if let Some(update) =
        crate::undo::poll_local_history(widget, invocation, cancellation_requested)
    {
        return Some(update);
    }
    widget.children_mut().into_iter().find_map(|child| {
        poll_resource_operation(child, invocation, cancellation_requested, doc, path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Button, Decorator};
    use automerge::transaction::Transactable;

    /// Answers `"probe.write"` by writing `written` into the document.
    struct Writer {
        inner: Button,
    }

    impl Decorator for Writer {
        crate::provides!(dyn crate::widget::ResourceOperations; inner = inner);
        type Inner = Button;

        fn inner(&self) -> &Button {
            &self.inner
        }

        fn inner_mut(&mut self) -> &mut Button {
            &mut self.inner
        }
    }

    impl crate::widget::ResourceOperations for Writer {
        fn start_operation(
            &mut self,
            op: &ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            _: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            if op
                .unscoped()
                .is_none_or(|request| request.type_id != "probe.write")
            {
                return Ok(None);
            }
            doc.document_mut()
                .put(automerge::ROOT, "written", true)
                .map_err(|error| error.to_string())?;
            Ok(Some(ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    fn offer(
        widget: &mut Writer,
        doc: &mut crate::widget::Edits<'_>,
        type_id: &str,
    ) -> Result<bool, String> {
        let op = ResourceOperation::local(
            tilo_actions::InvocationId::new(),
            tilo_actions::OperationKind::View,
            type_id,
            b"null",
        );
        start_without_document_writes(widget, &op, doc, &automerge::ROOT).map(|s| s.is_some())
    }

    #[test]
    fn a_read_only_caller_keeps_its_open_edits_and_refuses_a_writing_handler() {
        let mut widget = Writer {
            inner: Button::new("probe", "Probe"),
        };
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "typing", "pending").unwrap();
        let pending = doc.pending_ops();

        assert_eq!(
            offer(
                &mut widget,
                &mut crate::widget::Edits::lend(&mut doc),
                "probe.other"
            ),
            Ok(false)
        );
        assert_eq!(
            doc.pending_ops(),
            pending,
            "observing the document must not commit the caller's open transaction"
        );

        let refused = offer(
            &mut widget,
            &mut crate::widget::Edits::lend(&mut doc),
            "probe.write",
        );
        assert!(refused.is_err(), "a handler that wrote must be refused");
        assert!(
            doc.pending_ops() > pending,
            "the caller's own edits are retained rather than rolled back"
        );
    }
}
