//! The invoker a delivered invocation runs for on this thread, and the
//! one-shot replies the collaboration lanes answer an invocation with.
//! The toolkit's invocation scope (`ui::invocation_scope`) builds the door's
//! custody on these.

#[cfg(feature = "broker-transport")]
type ExternalModerationSender =
    tokio::sync::oneshot::Sender<Result<registry_types::ModerationCase, String>>;

/// Cloneable one-shot response custody for the collaboration request queue.
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

/// The host's answer to naming the paths an invocation is about to write
/// ([`ResourceInvocationScope::attribute_writes`]). Wait for it before
/// writing, so the versions those writes make record who caused them.
#[derive(Debug)]
pub struct WriteAttributionReceipt(std::sync::mpsc::Receiver<Result<(), String>>);

impl WriteAttributionReceipt {
    #[cfg_attr(
        not(feature = "broker-transport"),
        allow(dead_code, reason = "only the broker lane answers a write attribution")
    )]
    pub(crate) fn new(receive: std::sync::mpsc::Receiver<Result<(), String>>) -> Self {
        Self(receive)
    }

    /// Wait at most `timeout` for the host's answer. Whatever it is, the
    /// writes go ahead: a refusal or no answer leaves them the person's.
    ///
    /// # Errors
    /// Returns the host's refusal, or that it did not answer in time.
    pub fn wait(self, timeout: std::time::Duration) -> Result<(), String> {
        self.0
            .recv_timeout(timeout)
            .map_err(|_| "the host did not answer the write attribution".to_owned())?
    }
}

thread_local! {
    /// Each scope's invoker, with the broker invocation it was delivered
    /// under when it was one.
    static CURRENT_INVOKERS: std::cell::RefCell<
        Vec<(tilo_actions::ActionInvoker, Option<tilo_actions::InvocationId>)>,
    > = const { std::cell::RefCell::new(Vec::new()) };
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
        CURRENT_INVOKERS.with_borrow_mut(|invokers| invokers.push((invoker, None)));
        Self(())
    }

    /// Enter the invoker of `invocation`, which the broker delivered: a
    /// request the app makes for it meanwhile names it
    /// ([`delivered_invocation`]).
    pub fn delivered(
        invoker: tilo_actions::ActionInvoker,
        invocation: tilo_actions::InvocationId,
    ) -> Self {
        CURRENT_INVOKERS.with_borrow_mut(|invokers| invokers.push((invoker, Some(invocation))));
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
    CURRENT_INVOKERS.with_borrow(|invokers| invokers.last().map(|(invoker, _)| invoker.clone()))
}

/// The broker invocation being dispatched on this thread, when the broker
/// delivered it: what a request the app makes to the shell for it (opening
/// another app) names, so the shell admits it as that invocation's caller.
/// `None` for the local person acting, whose own press is the proof.
pub fn delivered_invocation() -> Option<tilo_actions::InvocationId> {
    CURRENT_INVOKERS.with_borrow(|invokers| invokers.last().and_then(|(_, invocation)| *invocation))
}

/// The participant whose view the current resolution builds, if any.
pub fn current_participant() -> Option<tilo_actions::ParticipantId> {
    CURRENT_INVOKERS.with_borrow(|invokers| {
        invokers
            .last()
            .and_then(|(invoker, _)| invoker.participant.clone())
    })
}
