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

/// The presence whose view the current resolution builds, if any.
pub fn current_presence() -> Option<tilo_actions::PresenceId> {
    CURRENT_INVOKERS.with_borrow(|invokers| {
        invokers
            .last()
            .and_then(|(invoker, _)| invoker.presence.clone())
    })
}

/// Inert delivered inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedInvocationRequest {
    pub wire_id: u64,
    pub invocation: tilo_actions::InvocationId,
    pub address: tilo_actions::ActionAddress,
    pub argument: Option<String>,
    pub actor: tilo_actions::Actor,
    /// Whose view this invocation resolves against. Set only by the device that admitted `actor` (for an
    /// agent task, derived from the admitted task), never taken from a caller's envelope. It routes to
    /// view state and is never authority.
    pub presence: Option<tilo_actions::PresenceId>,
}
impl PublishedInvocationRequest {
    /// The invoker this request resolves for: the admitted actor plus the device-set presence.
    pub fn invoker(&self) -> tilo_actions::ActionInvoker {
        let invoker = tilo_actions::ActionInvoker::admitted(&self.actor.clone().into());
        match &self.presence {
            Some(presence) => invoker.with_presence(presence.clone()),
            None => invoker,
        }
    }
}

/// Inert declaration retained by the actual device at original admission. This is not authority, and is
/// never decoded from a caller's wire request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedInvocationSpec {
    pub kind: tilo_actions::OperationKind,
    pub contract: tilo_actions::ActionContract,
    pub params: Vec<tilo_actions::ActionParam>,
    pub input: tilo_actions::InputSpec,
    pub directory_revision: Option<String>,
}

/// An opaque check minted by the actual device for this exact invocation. Devices must retain original
/// caller authority and resample their registered policy time on every check. A positive result is
/// never a reusable decision.
#[derive(Clone)]
pub struct PublishedInvocationLease {
    request: PublishedInvocationRequest,
    route: crate::RouteSpec,
    check: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    owner_token: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
    dispatch_spec: Option<PublishedInvocationSpec>,
}
impl PublishedInvocationLease {
    pub fn new(
        request: PublishedInvocationRequest,
        route: crate::RouteSpec,
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
    /// Attach once from the device's immutable original offered declaration.
    pub fn with_dispatch_spec(mut self, spec: PublishedInvocationSpec) -> Result<Self, String> {
        if self.dispatch_spec.is_some() {
            return Err("original invocation dispatch spec cannot be replaced".into());
        }
        self.dispatch_spec = Some(spec);
        Ok(self)
    }
    /// Attach the concrete device's privately constructed immutable original invocation facts. Presence
    /// is not authorization: the final owner must validate its own private type against already-held
    /// current authority.
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
    /// The delivered request this lease was minted for.
    pub fn request(&self) -> &PublishedInvocationRequest {
        &self.request
    }
    /// The route the invocation was admitted on.
    pub fn route(&self) -> &crate::RouteSpec {
        &self.route
    }
    /// The device's private record of the original invocation, if it attached one.
    pub fn owner_token(&self) -> Option<&std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.owner_token.as_ref()
    }
    /// The declaration the device admitted this invocation under.
    pub fn dispatch_spec(&self) -> Result<&PublishedInvocationSpec, String> {
        self.dispatch_spec.as_ref().ok_or_else(|| {
            "required invocation has no original admitted dispatch specification".into()
        })
    }
    /// Whether this lease was minted for `request`.
    pub fn matches(&self, request: &PublishedInvocationRequest) -> bool {
        self.request == *request
    }
    /// Whether `other` is this very lease: the same request, route, check and owner token.
    pub fn same_invocation(&self, other: &Self) -> bool {
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
