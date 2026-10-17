//! The desktop's action door for the Person/device selected in one room.
//!
//! A displayed principal string is never authority. Each session starts from
//! the identity broker's currently selected certified device and its signed
//! room-join presentation (re-signed only when the identity files change), and
//! lets the action broker verify current membership and policy before it lists
//! or invokes anything.

use std::sync::Arc;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvokeRequest, Pending,
};

use super::collab_broker::AppActionBroker;

#[derive(Clone)]
pub(crate) struct CertifiedActionSession {
    broker: AppActionBroker,
    caller: Caller,
    host: Arc<dyn ActionHost + Send + Sync>,
    presentation: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct ActionSessionSource {
    broker: AppActionBroker,
    #[cfg(test)]
    fixture: Option<super::remote_actions::BrokerActionHost>,
    #[cfg(test)]
    mock: Option<Arc<dyn ActionHost + Send + Sync>>,
    /// Sessions [`Self::open`] opened, shared by every clone.
    #[cfg(test)]
    opens: Arc<std::sync::atomic::AtomicUsize>,
}

pub(crate) enum DesktopActionSession {
    Certified(CertifiedActionSession),
    #[cfg(test)]
    Mock {
        host: Arc<dyn ActionHost + Send + Sync>,
        caller: Caller,
    },
    #[cfg(test)]
    Fixture {
        host: super::remote_actions::BrokerActionHost,
        caller: Caller,
    },
}

impl ActionSessionSource {
    pub(crate) fn certified_marker_agent(
        &self,
        room: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        self.broker.certified_agent_identity(room, actor)
    }

    pub(crate) fn new(broker: AppActionBroker) -> Self {
        Self {
            broker,
            #[cfg(test)]
            fixture: None,
            #[cfg(test)]
            mock: None,
            #[cfg(test)]
            opens: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(broker: AppActionBroker) -> Self {
        Self {
            fixture: Some(super::remote_actions::BrokerActionHost::new(broker.clone())),
            mock: None,
            broker,
            opens: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_mock(
        broker: AppActionBroker,
        host: Arc<dyn ActionHost + Send + Sync>,
    ) -> Self {
        Self {
            broker,
            fixture: None,
            mock: Some(host),
            opens: Arc::default(),
        }
    }
    /// How many sessions this source and its clones opened.
    #[cfg(test)]
    pub(crate) fn opens(&self) -> usize {
        self.opens.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A value that changes whenever [`Self::open`] for `room` could admit
    /// differently because this device's identity files changed: the room's
    /// selection, the device key, its certificate or the fleet. Only `stat`s,
    /// no session. `None` while that can't be known (a file can't be
    /// examined, or was written too recently to trust its stamp): the caller
    /// must assume it changed.
    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_self,
            reason = "a test fixture source answers without identity files"
        )
    )]
    pub(crate) fn identity_revision(&self, room: &str) -> Option<u64> {
        use std::hash::{Hash as _, Hasher as _};
        #[cfg(test)]
        if self.fixture.is_some() || self.mock.is_some() {
            return Some(0);
        }
        let stamp = tilo_identity_host::join_proof::current_device_stamp(room)
            .filter(tilo_identity_host::join_proof::CurrentDeviceStamp::settled)?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stamp.hash(&mut hasher);
        Some(hasher.finish())
    }

    pub(crate) fn open(
        &self,
        room: &str,
        expected_person: Option<&str>,
    ) -> Result<DesktopActionSession, String> {
        #[cfg(test)]
        self.opens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        if let Some(host) = &self.mock {
            return Ok(DesktopActionSession::Mock {
                host: Arc::clone(host),
                caller: Caller::local_person("synthetic").in_room(room),
            });
        }
        #[cfg(test)]
        if let Some(host) = &self.fixture {
            let person = expected_person.ok_or("fixture action Person is required")?;
            return Ok(DesktopActionSession::Fixture {
                host: host.clone(),
                caller: Caller::local_person(person).in_room(room),
            });
        }
        CertifiedActionSession::open(self.broker.clone(), room, expected_person)
            .map(DesktopActionSession::Certified)
    }
}

#[derive(Clone)]
pub(crate) enum ActionLifecycle {
    Certified(tilo_action_broker::collab_broker_capnp::CertifiedActionLifecycle),
    /// Compatibility for rows admitted without certificate custody. Never
    /// reinterpret these as a certified person's jobs or issue evidence.
    Uncertified {
        broker: AppActionBroker,
        actor: String,
    },
}

impl ActionLifecycle {
    pub(crate) fn statuses(&self) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        match self {
            Self::Certified(lifecycle) => lifecycle.statuses(),
            Self::Uncertified { broker, actor } => broker.statuses(actor),
        }
    }
    pub(crate) fn status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
        match self {
            Self::Certified(lifecycle) => lifecycle.status(id),
            Self::Uncertified { broker, actor } => broker.status_for_actor(id, actor),
        }
    }
    pub(crate) fn can_cancel(&self, id: u64) -> bool {
        match self {
            Self::Certified(lifecycle) => lifecycle.can_cancel(id),
            Self::Uncertified { broker, actor } => broker.can_cancel(id, actor),
        }
    }
    pub(crate) fn cancel(&self, id: u64) -> Result<(), String> {
        match self {
            Self::Certified(lifecycle) => lifecycle.cancel(id),
            Self::Uncertified { broker, actor } => broker.cancel(id, actor),
        }
    }
}

impl DesktopActionSession {
    pub(crate) fn lifecycle(&self) -> Result<ActionLifecycle, String> {
        match self {
            Self::Certified(session) => session
                .broker
                .state()
                .certified_action_lifecycle(&session.caller, &session.presentation)
                .map(ActionLifecycle::Certified),
            #[cfg(test)]
            Self::Fixture { host, caller } => Ok(ActionLifecycle::Uncertified {
                broker: host.broker_for_fixture(),
                actor: match caller.principal() {
                    tilo_actions::remote::CallerPrincipal::Person(actor) => actor.clone(),
                    _ => return Err("fixture requires a person".into()),
                },
            }),
            #[cfg(test)]
            Self::Mock { .. } => Err("mock has no job ledger".into()),
        }
    }

    pub(crate) fn stage_private_draft(
        &self,
        draft: runtime_collab::private_draft::Draft,
    ) -> Result<uuid::Uuid, String> {
        match self {
            Self::Certified(session) => session.broker.state().stage_private_draft(
                &session.caller,
                &session.presentation,
                draft,
            ),
            #[cfg(test)]
            _ => Err("private draft transfer requires certified native worker custody".into()),
        }
    }

    /// Invoke through the session's original certified host and caller. Reads
    /// that complete synchronously need no artificial broker-job mapping.
    pub(crate) fn invoke(
        &self,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        match self {
            Self::Certified(session) => session.host.invoke(&session.caller, request),
            #[cfg(test)]
            Self::Mock { host, caller } => host.invoke(caller, request),
            #[cfg(test)]
            Self::Fixture { host, caller } => host.invoke(caller, request),
        }
    }

    pub(crate) fn observe(
        &self,
        invocation: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        match self {
            Self::Certified(session) => session.host.observe(&session.caller, invocation),
            #[cfg(test)]
            Self::Mock { host, caller } => host.observe(caller, invocation),
            #[cfg(test)]
            Self::Fixture { host, caller } => host.observe(caller, invocation),
        }
    }

    pub(crate) fn next_change(
        &self,
        invocation: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        match self {
            Self::Certified(session) => session.host.next_change(&session.caller, invocation),
            #[cfg(test)]
            Self::Mock { host, caller } => host.next_change(caller, invocation),
            #[cfg(test)]
            Self::Fixture { host, caller } => host.next_change(caller, invocation),
        }
    }

    pub(crate) fn query(
        &self,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        match self {
            Self::Certified(session) => session.query(query),
            #[cfg(test)]
            Self::Mock { host, caller } => host.query(caller, query),
            #[cfg(test)]
            Self::Fixture { host, caller } => host.query(caller, query),
        }
    }

    pub(crate) fn invoke_job(
        &self,
        request: InvokeRequest,
    ) -> Pending<Result<(u64, InvocationReceipt), ActionHostError>> {
        match self {
            Self::Certified(session) => session.invoke_job(request),
            #[cfg(test)]
            Self::Mock { host, caller } => host
                .invoke(caller, request)
                .map(|result| result.map(|receipt| (0, receipt))),
            #[cfg(test)]
            Self::Fixture { host, caller } => host.invoke_job(caller, request),
        }
    }
}

impl CertifiedActionSession {
    /// Bind a fresh execution to the selected Person and device in `room`.
    ///
    /// `expected_person` is only a consistency fence for the UI projection;
    /// the certified device presentation remains the authority.
    ///
    /// # Errors
    ///
    /// Refuses an invalid room, unavailable/revoked selected identity,
    /// mismatched projected Person, or action-broker admission failure.
    pub(crate) fn open(
        broker: AppActionBroker,
        room: &str,
        expected_person: Option<&str>,
    ) -> Result<Self, String> {
        let parsed = uuid::Uuid::parse_str(room).map_err(|_| "invalid action room")?;
        if parsed.is_nil() {
            return Err("invalid action room".into());
        }
        // Minted from disk and the device key only when the identity, key,
        // certificate or fleet files changed; the broker verifies it below
        // against current membership and policy either way.
        let joined = tilo_identity_host::join_proof::trusted_current_join_proof(room)
            .map_err(|error| format!("selected workspace identity unavailable: {error}"))?;
        let certificate = &joined.certificate;
        let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
            certificate.user_public_key,
        );
        if expected_person.is_some_and(|expected| expected != person) {
            return Err("displayed workspace Person differs from selected identity".into());
        }
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_owned(),
            device_certificate: serde_json::to_vec(certificate)
                .map_err(|error| format!("encoding selected device certificate failed: {error}"))?,
            proof: joined.proof.to_vec(),
        }
        .encode()
        .map_err(|error| format!("encoding certified action identity failed: {error}"))?;
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                .in_room(room)
                .in_new_execution();
        let host = broker.certified_local_action_host(&caller, &presentation)?;
        Ok(Self {
            broker,
            caller,
            host,
            presentation,
        })
    }

    pub(crate) fn query(
        &self,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.host.query(&self.caller, query)
    }

    /// The host and the certified caller it admits, for a caller that runs
    /// its own catalogue reads and invocations off the shell's thread.
    pub(crate) fn door(self) -> (Arc<dyn ActionHost + Send + Sync>, Caller) {
        (self.host, self.caller)
    }

    #[cfg(test)]
    pub(crate) fn cancel(
        &self,
        invocation: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.host.cancel(&self.caller, invocation)
    }

    pub(crate) fn invoke_job(
        &self,
        request: InvokeRequest,
    ) -> Pending<Result<(u64, InvocationReceipt), ActionHostError>> {
        let broker = self.broker.clone();
        let caller = self.caller.clone();
        let invocation = request.invocation;
        self.host.invoke(&self.caller, request).map(move |result| {
            let receipt = result?;
            let job = broker
                .state()
                .remote_job(&caller, &invocation)
                .ok_or(ActionHostError::UnknownInvocation)?;
            Ok((job, receipt))
        })
    }
}

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_actions::remote::{InvocationState, InvokeArgument, PendingPoll, RemoteActionTarget};
    use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

    struct Probe {
        caller: Caller,
        calls: AtomicUsize,
    }

    impl Probe {
        fn check(&self, caller: &Caller) {
            assert_eq!(caller.principal(), self.caller.principal());
            assert_eq!(caller.room(), self.caller.room());
            assert_eq!(caller.user_public_key(), self.caller.user_public_key());
            assert_eq!(caller.device_public_key(), self.caller.device_public_key());
            assert_eq!(caller.execution_id(), self.caller.execution_id());
            self.calls.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl ActionHost for Probe {
        fn query(
            &self,
            caller: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            self.check(caller);
            Pending::ready(Err(ActionHostError::NotPermitted))
        }
        fn invoke(
            &self,
            caller: &Caller,
            _: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            self.check(caller);
            Pending::ready(Err(ActionHostError::NotPermitted))
        }
        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.check(caller);
            Some(InvocationState::new(
                *id,
                InvocationUpdate::Accepted { invocation: *id },
            ))
        }
        fn next_change(&self, caller: &Caller, _: &InvocationId) -> Option<Pending<()>> {
            self.check(caller);
            Some(Pending::ready(()))
        }
        fn cancel(&self, _: &Caller, _: &InvocationId) -> Result<(), ActionHostError> {
            Err(ActionHostError::NotPermitted)
        }
    }

    #[test]
    fn session_forwarders_preserve_pinned_caller_and_exact_refusal() {
        let caller = Caller::authenticated([11; 32], [12; 32])
            .in_room("room")
            .in_new_execution();
        let probe = Arc::new(Probe {
            caller: caller.clone(),
            calls: AtomicUsize::new(0),
        });
        let session = DesktopActionSession::Mock {
            host: probe.clone(),
            caller,
        };
        let invocation = InvocationId::new();
        let mut answer = session.invoke(InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: "provider".into(),
                address: ActionAddress::new("resource", "read"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: Some("revision".into()),
            on_behalf_of: None,
            appearance: None,
            wait_ms: None,
        });
        assert!(matches!(
            crate::shell::search::invocation_wake::poll(&mut answer, std::task::Waker::noop()),
            PendingPoll::Ready(Err(ActionHostError::NotPermitted))
        ));
        assert_eq!(
            session.observe(&invocation).unwrap().invocation(),
            invocation
        );
        let mut changed = session.next_change(&invocation).unwrap();
        drop(session);
        assert!(matches!(
            crate::shell::search::invocation_wake::poll(&mut changed, std::task::Waker::noop()),
            PendingPoll::Ready(())
        ));
        assert_eq!(probe.calls.load(Ordering::Relaxed), 3);
    }
}
