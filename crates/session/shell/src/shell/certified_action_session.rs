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
}

#[derive(Clone)]
pub(crate) struct ActionSessionSource {
    broker: AppActionBroker,
    #[cfg(test)]
    fixture: Option<super::remote_actions::BrokerActionHost>,
    /// Sessions [`Self::open`] opened, shared by every clone.
    #[cfg(test)]
    opens: Arc<std::sync::atomic::AtomicUsize>,
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
            #[cfg(test)]
            opens: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(broker: AppActionBroker) -> Self {
        Self {
            fixture: Some(super::remote_actions::BrokerActionHost::new(broker.clone())),
            broker,
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
    pub(crate) fn identity_revision(&self, room: &str) -> Option<u64> {
        use std::hash::{Hash as _, Hasher as _};
        #[cfg(test)]
        if self.fixture.is_some() {
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
}
