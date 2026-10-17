//! The one entry through which the retained tree admits an exported effect or
//! view operation: [`Widget::start_operation`].

use super::Widget;
use crate::invocation_scope::ResourceInvocationScope;
use automerge::ObjId;

/// Result of admitting an effect/view operation into a widget.
///
/// `Accepted` is deliberately distinct from success: the widget must later
/// report progress and exactly one terminal [`tilo_actions::InvocationUpdate`]
/// from [`Widget::poll_resource_operation`]. This keeps queued and streaming
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
        if self.address.action != operation.action()
            || self.address.resource.is_empty()
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
    ///
    /// # Errors
    ///
    /// Returns the scope's refusal once its caller or owner has retired.
    pub fn check(&self) -> Result<(), String> {
        self.scope.map_or(Ok(()), ResourceInvocationScope::check)
    }

    /// Offer this operation to `widget`.
    ///
    /// # Errors
    ///
    /// Returns the first admission or synchronous-operation error.
    pub fn start(
        &self,
        widget: &mut (impl Widget + ?Sized),
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        widget.start_operation(self, doc, path)
    }
}

/// Offer `op` to `widget` for a caller that grants no document access. A
/// handler that writes anyway is refused. Its bytes are retained: rolling
/// back would also discard the caller's own uncommitted edits.
///
/// # Errors
///
/// Returns the handler's error, or a refusal when it wrote the document.
pub(crate) fn start_without_document_writes(
    widget: &mut (impl Widget + ?Sized),
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    // `get_heads` would commit the caller's open transaction.
    let before = crate::doc::settle_mark(doc);
    let started = widget.start_operation(op, doc, path);
    if crate::doc::settle_mark(doc) != before {
        return Err("a read-only operation wrote the document; bytes were retained".into());
    }
    started
}

/// Offer `op` to each retained child in order and return the first start.
/// This is [`Widget::start_operation`]'s routing; an override that handles
/// some types itself calls it for the rest.
///
/// # Errors
///
/// Returns a required operation's retired scope, or the first child error.
pub fn start_in_children<W: Widget + ?Sized>(
    widget: &mut W,
    op: &ResourceOperation<'_>,
    doc: &mut crate::widget::Edits<'_>,
    path: &ObjId,
) -> Result<Option<ResourceOperationStart>, String> {
    op.check()?;
    for child in widget.children_mut() {
        // A nested canonical owner is an independent destination. A required
        // operation reaches it only through that owner's checked dispatch frame.
        if op.is_scoped() && child.has_separate_document_owner() {
            continue;
        }
        if let Some(started) = child.start_operation(op, doc, path)? {
            return Ok(Some(started));
        }
    }
    Ok(None)
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
        type Inner = Button;

        fn inner(&self) -> &Button {
            &self.inner
        }

        fn inner_mut(&mut self) -> &mut Button {
            &mut self.inner
        }

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
