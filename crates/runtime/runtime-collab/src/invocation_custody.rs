//! Original invocation custody: what a host promises an application worker
//! about the invocation it delivered, and the worker's check that the
//! delivery is the one the host retained.
//!
//! None of this grants authority. The host keeps the original caller,
//! contract and exact worker connection privately; everything here is inert
//! evidence of what it admitted, and the lease is an opaque check the host
//! mints for one exact invocation. A positive result is never a reusable
//! decision: a host must retain original caller authority and resample its
//! registered policy time on every check.

use crate::route::RouteSpec;

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
        let invoker = tilo_actions::ActionInvoker::actor(self.actor.clone());
        match &self.participant {
            Some(participant) => invoker.with_participant(participant.clone()),
            None => invoker,
        }
    }
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
    route: RouteSpec,
    check: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    owner_token: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
    dispatch_spec: Option<PublishedInvocationSpec>,
}

impl PublishedInvocationLease {
    pub fn new(
        request: PublishedInvocationRequest,
        route: RouteSpec,
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
    /// The inert delivered inputs this lease was minted for.
    #[must_use]
    pub fn request(&self) -> &PublishedInvocationRequest {
        &self.request
    }
    /// The route the host retained this invocation for.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }
    /// The concrete host's private original facts, when it attached them.
    /// Presence is not authorization.
    #[must_use]
    pub fn owner_token(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        self.owner_token.as_deref()
    }
    /// The declaration the host admitted with this invocation, which the
    /// worker compares its current resolved handler against.
    ///
    /// # Errors
    ///
    /// A required invocation whose host attached no declaration.
    pub fn dispatch_spec(&self) -> Result<&PublishedInvocationSpec, String> {
        self.dispatch_spec.as_ref().ok_or_else(|| {
            "required invocation has no original admitted dispatch specification".into()
        })
    }
    /// Whether this lease was minted for exactly `request`.
    #[must_use]
    pub fn matches(&self, request: &PublishedInvocationRequest) -> bool {
        self.request == *request
    }
    /// Whether both leases name one invocation: the same request, route,
    /// admitted declaration, host check and host facts.
    #[must_use]
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

/// The host's decision for one delivery: a lease for exactly the request it
/// was handed, or why the delivery is not the original it retained.
pub type PublishedInvocationAuthorizer = std::sync::Arc<
    dyn Fn(&PublishedInvocationRequest) -> Result<PublishedInvocationLease, String> + Send + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::InputSpec;

    fn request(wire_id: u64) -> PublishedInvocationRequest {
        PublishedInvocationRequest {
            wire_id,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(wire_id))),
            address: tilo_actions::ActionAddress::new("test.counter", "bump"),
            argument: None,
            actor: "person:original".into(),
            participant: None,
        }
    }

    fn spec() -> PublishedInvocationSpec {
        PublishedInvocationSpec {
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            params: Vec::new(),
            input: InputSpec::None,
            directory_revision: Some("revision-1".into()),
        }
    }

    fn route() -> RouteSpec {
        RouteSpec::record("test.counter", uuid::Uuid::from_u128(1), "record-1")
    }

    fn lease(request: PublishedInvocationRequest) -> PublishedInvocationLease {
        PublishedInvocationLease::new(request, route(), std::sync::Arc::new(|| Ok(())))
    }

    #[test]
    fn a_lease_answers_only_for_the_exact_request_it_was_minted_for() {
        let original = request(7);
        let lease = lease(original.clone()).with_dispatch_spec(spec()).unwrap();
        assert!(lease.matches(&original));
        for changed in [
            PublishedInvocationRequest {
                wire_id: 8,
                ..original.clone()
            },
            PublishedInvocationRequest {
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(99)),
                ..original.clone()
            },
            PublishedInvocationRequest {
                address: tilo_actions::ActionAddress::new("test.counter", "reset"),
                ..original.clone()
            },
            PublishedInvocationRequest {
                argument: Some(r#"{"by":1}"#.into()),
                ..original.clone()
            },
            PublishedInvocationRequest {
                actor: "agent:borrowed".into(),
                ..original.clone()
            },
            PublishedInvocationRequest {
                participant: Some(tilo_actions::ParticipantId::for_execution(
                    uuid::Uuid::from_u128(5),
                )),
                ..original
            },
        ] {
            assert!(
                !lease.matches(&changed),
                "a changed field must not read as the original request"
            );
        }
    }

    #[test]
    fn the_original_spec_and_owner_token_attach_once_and_tell_leases_apart() {
        let original = request(7);
        let declared = lease(original.clone()).with_dispatch_spec(spec()).unwrap();
        assert_eq!(declared.dispatch_spec().unwrap(), &spec());
        assert!(
            declared.clone().with_dispatch_spec(spec()).is_err(),
            "the host's original declaration cannot be replaced"
        );
        assert!(
            lease(original.clone()).dispatch_spec().is_err(),
            "a lease without the admitted declaration says so"
        );
        let token = std::sync::Arc::new(original.clone());
        let owned = declared.clone().with_owner_token(token.clone()).unwrap();
        assert!(
            owned.clone().with_owner_token(token).is_err(),
            "the host's original facts cannot be replaced"
        );
        assert!(
            !owned.same_invocation(&declared),
            "a lease with the host's private facts is not one without them"
        );
        assert!(
            owned.same_invocation(&owned.clone()),
            "the same host lease names the same invocation"
        );
        assert!(
            !owned.same_invocation(
                &lease(original)
                    .with_owner_token(std::sync::Arc::new(()))
                    .unwrap()
            ),
            "different host facts are different invocations"
        );
    }

    #[test]
    fn a_check_is_the_host_s_answer_now_never_a_stored_decision() {
        let allowed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sampled = allowed.clone();
        let lease = PublishedInvocationLease::new(
            request(7),
            route(),
            std::sync::Arc::new(move || {
                sampled
                    .load(std::sync::atomic::Ordering::Acquire)
                    .then_some(())
                    .ok_or_else(|| "original caller is no longer authorized".to_string())
            }),
        );
        lease.check().expect("the host allows it now");
        allowed.store(false, std::sync::atomic::Ordering::Release);
        assert_eq!(
            lease.check().unwrap_err(),
            "original caller is no longer authorized",
            "the lease resamples the host instead of remembering an answer"
        );
    }
}
