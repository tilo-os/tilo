//! The desktop's action door for the Person/device selected in one room.
//!
//! A displayed principal string is never authority. Each session starts from
//! the identity broker's currently selected certified device, signs the
//! existing room-join presentation, and lets the action broker verify current
//! membership and policy before it lists or invokes anything.

use ed25519_dalek::Signer as _;
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
}

#[derive(Clone)]
pub(crate) struct ActionSessionSource {
    broker: AppActionBroker,
    #[cfg(test)]
    fixture: Option<super::remote_actions::BrokerActionHost>,
}

pub(crate) enum DesktopActionSession {
    Certified(CertifiedActionSession),
    #[cfg(test)]
    Fixture {
        host: super::remote_actions::BrokerActionHost,
        caller: Caller,
    },
}

impl ActionSessionSource {
    pub(crate) fn new(broker: AppActionBroker) -> Self {
        Self {
            broker,
            #[cfg(test)]
            fixture: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(broker: AppActionBroker) -> Self {
        Self {
            fixture: Some(super::remote_actions::BrokerActionHost::new(broker.clone())),
            broker,
        }
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
        if let Some(host) = &self.fixture {
            let person = expected_person.ok_or_else(|| {
                (
                    "fixture action Person is required".into(),
                    "projected_person_unavailable",
                )
            })?;
            return Ok(DesktopActionSession::Fixture {
                host: host.clone(),
                caller: Caller::local_person(person).in_room(room),
            });
        }
        CertifiedActionSession::open_classified(self.broker.clone(), room, expected_person)
            .map(DesktopActionSession::Certified)
    }
}

impl DesktopActionSession {
    pub(crate) fn query(
        &self,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        match self {
            Self::Certified(session) => session.query(query),
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
            Self::Fixture { host, caller } => host.invoke_job(caller, request),
        }
    }

    pub(crate) fn observe(
        &self,
        invocation: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        match self {
            Self::Certified(session) => session.observe(invocation),
            #[cfg(test)]
            Self::Fixture { host, caller } => host.observe(caller, invocation),
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
        let (key, certificate) =
            tilo_identity_host::broker::trusted_current_device(room).map_err(|error| {
                (
                    format!("selected workspace identity unavailable: {error}"),
                    "identity_unavailable",
                )
            })?;
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
            device_certificate: serde_json::to_vec(&certificate).map_err(|error| {
                (
                    format!("encoding selected device certificate failed: {error}"),
                    "identity_unavailable",
                )
            })?,
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(room))
                .to_bytes()
                .to_vec(),
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
        })
    }

    pub(crate) fn query(
        &self,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.host.query(&self.caller, query)
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
