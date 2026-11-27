//! Where a host dispatched an operation: the exact record route and action it resolved, which
//! the toolkit carries to the widget that answers it ([`ResourceOperation::with_target`]), and
//! the moderation an approved Store action submits through that record's host.
//!
//! [`ResourceOperation::with_target`]: tilo_ui_base::widget::ResourceOperation::with_target

/// A host moderation receipt, suitable for an existing action worker.
pub type ExternalModerationFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<registry_types::ModerationCase, String>> + Send>,
>;

/// Inert routing facts captured from the actual resolved record owner.
/// These are neither a certificate nor permission to execute a host job.
#[derive(Clone, Debug)]
pub struct OperationTarget {
    route: ::collaboration::RouteSpec,
    address: tilo_action_types::ActionAddress,
}
impl OperationTarget {
    pub fn new(
        route: ::collaboration::RouteSpec,
        address: tilo_action_types::ActionAddress,
    ) -> Self {
        Self { route, address }
    }
    pub fn route(&self) -> &::collaboration::RouteSpec {
        &self.route
    }
    pub fn address(&self) -> &tilo_action_types::ActionAddress {
        &self.address
    }

    /// Submit an operation on the exact dispatched record route. The broker
    /// recovers original admission and compares the complete action parameters.
    ///
    /// # Errors
    /// Refuses another action, missing record routing or unavailable transport.
    pub fn external_moderation(
        &self,
        invocation: tilo_action_types::InvocationId,
        operation: registry_types::ExternalModerationOperation,
    ) -> Result<ExternalModerationFuture, String> {
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
}
