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
    InvocationState, InvokeRequest, Pending,
};

use super::collab_broker::AppActionBroker;

#[derive(Clone)]
pub(crate) struct CertifiedActionSession {
    broker: AppActionBroker,
    caller: Caller,
    host: Arc<dyn ActionHost + Send + Sync>,
    presentation: Vec<u8>,
    custody: ActionSessionCustody,
}

/// Opaque identity custody used only to isolate private desktop UI state.
/// It is never rendered, logged, or accepted as invocation authority.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ActionSessionCustody(ActionSessionCustodyKind);

#[derive(Clone, PartialEq, Eq, Hash)]
enum ActionSessionCustodyKind {
    Certified {
        person: [u8; 32],
        device: [u8; 32],
    },
    #[cfg(test)]
    Fixture(String),
    #[cfg(test)]
    Mock(usize),
}

pub(crate) struct DesktopActionClient {
    pub(crate) actions: ui::remote_actions::RemoteActions,
    pub(crate) custody: ActionSessionCustody,
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
        broker: AppActionBroker,
        host: super::remote_actions::BrokerActionHost,
        caller: Caller,
        actor: String,
    },
}

impl ActionSessionSource {
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
        self.open_classified(room, expected_person)
            .map_err(|(detail, _)| detail)
    }

    /// Open the same certified session while retaining a content-free failure
    /// class for the shell's bounded native-service diagnostic. The detailed
    /// error remains private to the caller and is never put in the trace.
    pub(crate) fn open_classified(
        &self,
        room: &str,
        expected_person: Option<&str>,
    ) -> Result<DesktopActionSession, (String, &'static str)> {
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
            let person = expected_person.ok_or_else(|| {
                (
                    "fixture action Person is required".into(),
                    "projected_person_unavailable",
                )
            })?;
            return Ok(DesktopActionSession::Fixture {
                broker: self.broker.clone(),
                host: host.clone(),
                caller: Caller::local_person(person).in_room(room),
                actor: person.to_string(),
            });
        }
        CertifiedActionSession::open_classified(self.broker.clone(), room, expected_person)
            .map(DesktopActionSession::Certified)
    }
}

impl DesktopActionSession {
    #[cfg(test)]
    pub(crate) fn custody(&self) -> ActionSessionCustody {
        match self {
            Self::Certified(session) => session.custody.clone(),
            Self::Mock { host, .. } => ActionSessionCustody(ActionSessionCustodyKind::Mock(
                Arc::as_ptr(host).cast::<()>() as usize,
            )),
            Self::Fixture { actor, .. } => {
                ActionSessionCustody(ActionSessionCustodyKind::Fixture(actor.clone()))
            }
        }
    }

    /// Use this already-validated desktop session through the shared SDK
    /// action client while preserving its exact certified invocation origin.
    #[must_use]
    pub(crate) fn action_client(&self) -> DesktopActionClient {
        match self {
            Self::Certified(session) => DesktopActionClient {
                actions: ui::remote_actions::RemoteActions::with_host(
                    Arc::clone(&session.host),
                    session.caller.clone(),
                ),
                custody: session.custody.clone(),
            },
            #[cfg(test)]
            Self::Mock { host, caller } => DesktopActionClient {
                actions: ui::remote_actions::RemoteActions::with_host(
                    Arc::clone(host),
                    caller.clone(),
                ),
                custody: self.custody(),
            },
            #[cfg(test)]
            Self::Fixture {
                host,
                caller,
                actor,
                ..
            } => DesktopActionClient {
                actions: ui::remote_actions::RemoteActions::with_host(
                    Arc::new(host.clone()),
                    caller.clone(),
                ),
                custody: ActionSessionCustody(ActionSessionCustodyKind::Fixture(actor.clone())),
            },
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
            Self::Fixture { host, caller, .. } => host.query(caller, query),
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
            Self::Fixture { host, caller, .. } => host.invoke_job(caller, request),
        }
    }

    pub(crate) fn observe(
        &self,
        invocation: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        match self {
            Self::Certified(session) => session.observe(invocation),
            #[cfg(test)]
            Self::Mock { host, caller } => host.observe(caller, invocation),
            #[cfg(test)]
            Self::Fixture { host, caller, .. } => host.observe(caller, invocation),
        }
    }

    pub(crate) fn job_status(&self, job: u64) -> Option<tilo_actions::InvocationUpdate> {
        match self {
            Self::Certified(session) => {
                session
                    .broker
                    .certified_job_status(&session.caller, &session.presentation, job)
            }
            #[cfg(test)]
            Self::Mock { .. } => None,
            #[cfg(test)]
            Self::Fixture { broker, actor, .. } => broker.status_for_actor(job, actor),
        }
    }

    /// Revalidate one job while retaining only a closed refusal class for
    /// local diagnostics. The public action response remains deliberately
    /// opaque about both admission and invocation ownership.
    pub(crate) fn job_status_classified(
        &self,
        job: u64,
    ) -> Result<tilo_actions::InvocationUpdate, &'static str> {
        match self {
            Self::Certified(session) => session.broker.certified_job_status_classified(
                &session.caller,
                &session.presentation,
                job,
            ),
            #[cfg(test)]
            Self::Mock { .. } => Err("job_binding_unavailable"),
            #[cfg(test)]
            Self::Fixture { broker, actor, .. } => broker
                .status_for_actor(job, actor)
                .ok_or("job_binding_unavailable"),
        }
    }

    #[must_use]
    pub(crate) fn can_cancel_job(&self, job: u64) -> bool {
        match self {
            Self::Certified(session) => {
                session
                    .broker
                    .certified_job_can_cancel(&session.caller, &session.presentation, job)
            }
            #[cfg(test)]
            Self::Mock { .. } => false,
            #[cfg(test)]
            Self::Fixture { broker, actor, .. } => broker.can_cancel(job, actor),
        }
    }

    pub(crate) fn cancel_job(&self, job: u64) -> Result<(), ActionHostError> {
        match self {
            Self::Certified(session) => {
                session
                    .broker
                    .certified_job_cancel(&session.caller, &session.presentation, job)
            }
            #[cfg(test)]
            Self::Mock { .. } => Err(ActionHostError::NotCancellable {
                reason: "mock sessions have no retained broker job".into(),
            }),
            #[cfg(test)]
            Self::Fixture { broker, actor, .. } => broker
                .cancel(job, actor)
                .map_err(|reason| ActionHostError::NotCancellable { reason }),
        }
    }
}

impl CertifiedActionSession {
    /// Retain the genuine certified Person caller and device for `room`.
    ///
    /// `expected_person` is only a consistency fence for the UI projection;
    /// the certified device presentation remains the authority.
    ///
    /// # Errors
    ///
    /// Refuses an invalid room, unavailable/revoked selected identity,
    /// mismatched projected Person, or action-broker admission failure.
    fn open_classified(
        broker: AppActionBroker,
        room: &str,
        expected_person: Option<&str>,
    ) -> Result<Self, (String, &'static str)> {
        let parsed = uuid::Uuid::parse_str(room)
            .map_err(|_| ("invalid action room".into(), "invalid_room"))?;
        if parsed.is_nil() {
            return Err(("invalid action room".into(), "invalid_room"));
        }
        // Minted from disk and the device key only when the identity, key,
        // certificate or fleet files changed; the broker verifies it below
        // against current membership and policy either way.
        let joined =
            tilo_identity_host::join_proof::trusted_current_join_proof(room).map_err(|error| {
                (
                    format!("selected workspace identity unavailable: {error}"),
                    "identity_unavailable",
                )
            })?;
        let certificate = &joined.certificate;
        let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
            certificate.user_public_key,
        );
        if expected_person.is_some_and(|expected| expected != person) {
            return Err((
                "displayed workspace Person differs from selected identity".into(),
                "projected_person_mismatch",
            ));
        }
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_owned(),
            device_certificate: serde_json::to_vec(certificate).map_err(|error| {
                (
                    format!("encoding selected device certificate failed: {error}"),
                    "identity_unavailable",
                )
            })?,
            proof: joined.proof.to_vec(),
        }
        .encode()
        .map_err(|error| {
            (
                format!("encoding certified action identity failed: {error}"),
                "identity_unavailable",
            )
        })?;
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                .in_room(room)
                .in_new_execution();
        let host = broker
            .certified_local_action_host(&caller, &presentation)
            .map_err(|detail| (detail, "broker_admission_refused"))?;
        Ok(Self {
            broker,
            caller,
            host,
            presentation,
            custody: ActionSessionCustody(ActionSessionCustodyKind::Certified {
                person: certificate.user_public_key,
                device: certificate.device_public_key,
            }),
        })
    }

    pub(crate) fn query(
        &self,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.host.query(&self.caller, query)
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

    fn observe(&self, invocation: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.host.observe(&self.caller, invocation)
    }
}
