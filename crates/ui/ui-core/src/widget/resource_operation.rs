//! The one entry through which the retained tree admits an exported effect or
//! view operation: [`start_operation`], answered by the widgets that provide
//! [`ResourceOperations`].

use std::collections::HashSet;

use super::Widget;
use super::capability::{capability, capability_mut};
use automerge::{AutoCommit, ObjId};
use tilo_resource::Refusal;

/// Result of admitting an effect/view operation into a widget.
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

/// The code of the refusal a view operation gives when it changed what the view shows and
/// reads the view only once that is laid out ([`lay_out_first`]).
pub const LAY_OUT_FIRST: &str = "lay_out_first";

thread_local! {
    /// Whether the host starting the operation on this thread lays out what a view operation
    /// shows before it is read ([`host_lays_out_first`]).
    static LAYS_OUT_FIRST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the host starting this operation lays the view out between a view operation's
/// showing something and its reading it: a view operation that shows an item first (a
/// render of one, the lines of one) then refuses with [`lay_out_first`], the host runs its
/// frame, and starts the operation again, which reads the view laid out. A host that cannot
/// (a live window mid-event) reads the view as it stands.
#[must_use]
pub fn host_lays_out_first() -> bool {
    LAYS_OUT_FIRST.with(std::cell::Cell::get)
}

/// What a view operation answers when it changed what the view shows and the host lays that
/// out before the operation reads it ([`host_lays_out_first`]).
#[must_use]
pub fn lay_out_first() -> Refusal {
    Refusal::coded(
        LAY_OUT_FIRST,
        "the view lays out what it shows before it is read",
    )
}

/// Whether `error` is the refusal [`lay_out_first`] answers with.
#[must_use]
pub fn asks_to_lay_out_first(error: &tilo_actions::ApiError) -> bool {
    matches!(error, tilo_actions::ApiError::Refused { code, .. } if code == LAY_OUT_FIRST)
}

/// Run `start` (a host starting an operation) as a host that lays a view out between a view
/// operation's showing something and its reading it ([`host_lays_out_first`]).
pub fn laying_out_first<T>(start: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            LAYS_OUT_FIRST.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(LAYS_OUT_FIRST.with(|cell| cell.replace(true)));
    start()
}

/// What a handler matches on.
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
    route: ::collaboration::RouteSpec,
    address: tilo_actions::ActionAddress,
}
impl OperationTarget {
    pub(crate) fn new(
        route: ::collaboration::RouteSpec,
        address: tilo_actions::ActionAddress,
    ) -> Self {
        Self { route, address }
    }
    pub fn route(&self) -> &::collaboration::RouteSpec {
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
            || self.route.document.as_deref().is_none_or(str::is_empty)
        {
            return Err("external moderation requires the exact dispatched resource".into());
        }
        #[cfg(feature = "broker-transport")]
        {
            let receive = ::collaboration::external_moderation::enqueue(
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
            || self.route.document.as_deref().is_none_or(str::is_empty)
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
                ::collaboration::approved_checks::enqueue(&self.route.channel_id(), request)?;
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
            || self.route.document.as_deref().is_none_or(str::is_empty)
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
#[derive(Clone, Copy)]
pub struct ResourceOperation<'a> {
    request: OperationRequest<'a>,
    target: Option<&'a OperationTarget>,
    /// The resource the operation was resolved against: the app's own, a
    /// child, or a live row.
    resource: Option<&'a str>,
}

impl std::fmt::Debug for ResourceOperation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceOperation")
            .field("request", &self.request)
            .finish()
    }
}

impl<'a> ResourceOperation<'a> {
    /// An operation the host admitted: local input, the palette, or a
    /// directed invocation already admitted by the broker.
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
            target: None,
            resource: None,
        }
    }

    pub(crate) fn with_target(mut self, target: Option<&'a OperationTarget>) -> Self {
        self.target = target;
        self
    }

    pub(crate) fn with_resource(mut self, resource: &'a str) -> Self {
        self.resource = Some(resource);
        self
    }

    pub fn target(&self) -> Option<&OperationTarget> {
        self.target
    }

    /// The resource the operation was resolved against: the app's own, a
    /// child, or a live row.
    pub fn resource(&self) -> Option<&'a str> {
        self.resource
    }

    pub fn invocation(&self) -> tilo_actions::InvocationId {
        self.request.invocation
    }

    pub fn kind(&self) -> tilo_actions::OperationKind {
        self.request.kind
    }

    /// What the operation asks for.
    pub fn request(&self) -> OperationRequest<'a> {
        self.request
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

/// Offer `op` to `widget` for a caller that grants no document access. A handler that writes
/// anyway is refused.
#[cfg(test)]
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
pub fn start_in_children<W: Widget + ?Sized>(
    widget: &mut W,
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
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
        if let Some(started) = start_operation(child, op, doc, path)? {
            return Ok(Some(started));
        }
    }
    Ok(None)
}

/// A widget that answers exported effect or view operations itself, or routes them its own way.
/// Declared with [`crate::provides!`].
pub trait ResourceOperations: Widget {
    /// Effect/view operation type ids this widget can actually dispatch. Implementors of
    /// [`Self::start_operation`] list their locally handled types here, beside it.
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        local_history_handlers(self)
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        subtree_handlers(self, self.local_resource_operation_handlers())
    }

    /// Admit an exported effect or view operation. Document operations never enter it.
    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        start_by_default(self, op, doc, path)
    }

    /// [`Self::start_operation`], where the refusal is the handler's own: one
    /// a caller decides on rather than reads keeps its code
    /// ([`Refusal::coded`]), so an answer that something is there already
    /// reaches whoever asked as the code it gave, not as words. A handler
    /// that refuses with a code overrides this; the default says the same in
    /// words only.
    fn start_operation_refused(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, Refusal> {
        self.start_operation(op, doc, path).map_err(Refusal::new)
    }

    /// Poll one admitted operation without blocking the UI thread. `cancellation_requested` is
    /// a cooperative signal from the action broker.
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

/// Offer `op` to `widget`, keeping the refusal the handler gave:
/// [`ResourceOperations::start_operation_refused`].
pub fn start_operation_refused(
    widget: &mut (impl Widget + ?Sized),
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, Refusal> {
    if let Some(operations) = capability_mut::<dyn ResourceOperations>(widget) {
        return operations.start_operation_refused(op, doc, path);
    }
    start_by_default(widget, op, doc, path).map_err(Refusal::new)
}

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

pub(crate) fn local_history_handlers(widget: &(impl Widget + ?Sized)) -> &'static [&'static str] {
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
            if op.request().type_id != "probe.write" {
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
