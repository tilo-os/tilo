//! The invoker a delivered invocation runs for on this thread, the
//! one-shot replies the collaboration endpoints answer an invocation with,
//! and the leases minted for an invocation once it was admitted, which a
//! server checks without any toolkit.
//! The toolkit's invocation scope (`ui::invocation_scope`) builds the API's
//! ownership on these.

#[cfg(feature = "broker-transport")]
type ExternalModerationSender =
    tokio::sync::oneshot::Sender<Result<registry_types::ModerationCase, String>>;

/// Cloneable one-shot response ownership for the collaboration request queue.
#[cfg(feature = "broker-transport")]
#[derive(Clone, Debug)]
pub struct ExternalModerationReply(
    std::sync::Arc<std::sync::Mutex<Option<ExternalModerationSender>>>,
);
#[cfg(feature = "broker-transport")]
impl ExternalModerationReply {
    pub fn is_closed(&self) -> bool {
        self.0.lock().map_or(true, |reply| {
            reply
                .as_ref()
                .is_none_or(tokio::sync::oneshot::Sender::is_closed)
        })
    }
    pub fn send(&self, result: Result<registry_types::ModerationCase, String>) {
        if let Ok(mut reply) = self.0.lock()
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(result);
        }
    }
    pub(crate) fn new() -> (
        Self,
        tokio::sync::oneshot::Receiver<Result<registry_types::ModerationCase, String>>,
    ) {
        let (send, receive) = tokio::sync::oneshot::channel();
        (
            Self(std::sync::Arc::new(std::sync::Mutex::new(Some(send)))),
            receive,
        )
    }
}

thread_local! {
    /// Each scope's invoker, with the broker invocation it was delivered
    /// under when it was one.
    static CURRENT_INVOKERS: std::cell::RefCell<Vec<Invoked>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// One scope's invoker, the broker invocation it was delivered under, and the person's
/// approve that invocation was sent again with.
struct Invoked {
    invoker: tilo_actions::ActionInvoker,
    invocation: Option<tilo_actions::InvocationId>,
    approval: Option<tilo_actions::remote::asked::Approval>,
}

/// The invoker whose resource tree a delivered invocation resolves against,
/// for the duration of its resolution and dispatch on this thread.
///
/// Resource collection for resolution
/// ([`crate::resources::PublishesResources::resources_for`]) and view operations read it,
/// so an invocation is resolved against the invoker's own view, rebuilt at
/// invoke time, never against a published snapshot. Scopes nest; dropping
/// the guard restores the enclosing invoker.
#[must_use = "the invoker applies only while the guard is alive"]
pub struct InvokerScope(());
impl InvokerScope {
    pub fn enter(invoker: tilo_actions::ActionInvoker) -> Self {
        CURRENT_INVOKERS.with_borrow_mut(|invokers| {
            invokers.push(Invoked {
                invoker,
                invocation: None,
                approval: None,
            });
        });
        Self(())
    }

    /// Enter the invoker of `invocation`, which the broker delivered: a
    /// request the app makes for it meanwhile names it
    /// ([`delivered_invocation`]). `approval` is the person's approve it was sent
    /// again with, as the host that admitted it set it ([`delivered_approval`]).
    pub fn delivered(
        invoker: tilo_actions::ActionInvoker,
        invocation: tilo_actions::InvocationId,
        approval: Option<tilo_actions::remote::asked::Approval>,
    ) -> Self {
        CURRENT_INVOKERS.with_borrow_mut(|invokers| {
            invokers.push(Invoked {
                invoker,
                invocation: Some(invocation),
                approval,
            });
        });
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
    CURRENT_INVOKERS.with_borrow(|invokers| invokers.last().map(|invoked| invoked.invoker.clone()))
}

/// The broker invocation being dispatched on this thread, when the broker
/// delivered it: what a request the app makes to the shell for it (opening
/// another app) names, so the shell admits it as that invocation's caller.
/// `None` for the local person acting, whose own press is the proof.
pub fn delivered_invocation() -> Option<tilo_actions::InvocationId> {
    CURRENT_INVOKERS.with_borrow(|invokers| invokers.last().and_then(|invoked| invoked.invocation))
}

/// The person's approve of the asked record the delivered invocation being dispatched on this
/// thread waited in: `Some` only for that call sent again once they approved it, as the host
/// that admitted it says. What a handler that refused the call for want of their yes reads to
/// run it now.
pub fn delivered_approval() -> Option<tilo_actions::remote::asked::Approval> {
    CURRENT_INVOKERS
        .with_borrow(|invokers| invokers.last().and_then(|invoked| invoked.approval.clone()))
}

/// The presence whose view the current resolution builds, if any.
pub fn current_presence() -> Option<tilo_actions::PresenceId> {
    CURRENT_INVOKERS.with_borrow(|invokers| {
        invokers
            .last()
            .and_then(|invoked| invoked.invoker.presence.clone())
    })
}
