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
}

#[derive(Clone)]
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
    pub(crate) fn observe(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        match self {
            Self::Certified(session) => {
                if let Err(reason) = session.recheck_selected() {
                    tracing::debug!(invocation = %invocation.0, room = session.caller.room(), %reason,
                        "certified job observation refused selected identity");
                    return None;
                }
                let state = session.host.observe(&session.caller, &invocation);
                if state.is_none() {
                    tracing::debug!(invocation = %invocation.0, room = session.caller.room(),
                        "certified job observation refused original session custody");
                }
                state.map(|state| state.update)
            }
            #[cfg(test)]
            Self::Fixture { host, caller } => {
                host.observe(caller, &invocation).map(|state| state.update)
            }
        }
    }

    pub(crate) fn cancel(&self, invocation: tilo_actions::InvocationId) -> Result<(), String> {
        match self {
            Self::Certified(session) => {
                session.recheck_selected()?;
                session
                    .host
                    .cancel(&session.caller, &invocation)
                    .map_err(|error| error.to_string())
            }
            #[cfg(test)]
            Self::Fixture { host, caller } => host
                .cancel(caller, &invocation)
                .map_err(|error| error.to_string()),
        }
    }

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
    fn recheck_selected(&self) -> Result<(), String> {
        let room = self.caller.room().ok_or("certified job room unavailable")?;
        let (_, certificate) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|_| "selected certified job identity unavailable")?;
        if self.caller.user_public_key() != Some(&certificate.user_public_key)
            || self.caller.device_public_key() != Some(&certificate.device_public_key)
        {
            tracing::debug!(room,
                original_device = ?self.caller.device_public_key().map(hex::encode),
                selected_device = hex::encode(certificate.device_public_key),
                "certified job selected device differs from its original caller");
            return Err("selected certified job device changed".into());
        }
        Ok(())
    }

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
        let (key, certificate) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| format!("selected workspace identity unavailable: {error}"))?;
        let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
            certificate.user_public_key,
        );
        if expected_person.is_some_and(|expected| expected != person) {
            return Err("displayed workspace Person differs from selected identity".into());
        }
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_owned(),
            device_certificate: serde_json::to_vec(&certificate)
                .map_err(|error| format!("encoding selected device certificate failed: {error}"))?,
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(room))
                .to_bytes()
                .to_vec(),
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
