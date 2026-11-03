//! Private person authority comes from the native app's certified transport.
use super::{JoinedChannel, PrivateWorkerRegistration};
use crate::services::person_chat::PersonChatCommand;

impl JoinedChannel {
    fn person_chat_actor(
        &self,
        bytes: &[u8],
        original: &[u8],
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if bytes.len() > 24 * 1024 {
            return Err("invalid private person request size".into());
        }
        // What the door answers is the window's own private snapshot: which
        // pairs it is in, a pair's announcement, the old store's import and
        // its emptying. No action carries those, so an original invocation
        // asks for none of them.
        if !original.is_empty() {
            return Err("an original invocation asks no private conversation operation".into());
        }
        serde_json::from_slice::<PersonChatCommand>(bytes)
            .map_err(|_| "invalid private person command")?;
        let (_, registration) = self.private_chat_worker()?;
        let actor = OriginalPrivateAppActor(registration.certificate);
        if actor.0.capabilities.foreign {
            return Err("foreign person actors cannot enter private conversations".into());
        }
        Ok(actor.0)
    }

    pub(super) fn private_chat_worker(&self) -> Result<(u64, PrivateWorkerRegistration), String> {
        if !self.state.is_accepting() {
            return Err("private person host has closed".into());
        }
        let route = self.route();
        if route.app_id != "so.tilo.chat" || self.verified_identity.agent_id.is_some() {
            return Err("private person conversations require the native Chat person actor".into());
        }
        let admitted = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
            .map_err(|error| error.to_string())?;
        if admitted.agent_id.is_some() || admitted.verified.capped {
            return Err("delegated actors cannot enter the private person door".into());
        }
        let registration = admitted
            .worker_registration
            .as_ref()
            .ok_or("private person conversations require a certified native app worker")?;
        if registration.route != route
            || admitted.certificate.as_ref() != Some(&registration.certificate)
        {
            return Err("private person app actor binding changed".into());
        }
        let peer = self
            .state
            .host_workers
            .lock()
            .map_err(|_| "private app registry unavailable")?
            .peer_of(registration)
            .ok_or("private Chat app worker has closed")?;
        Ok((peer, registration.clone()))
    }

    pub(super) fn person_chat_dispatch(
        &self,
        bytes: &[u8],
        original: &[u8],
    ) -> Result<Vec<u8>, String> {
        // The native channel carries both closed vocabularies. Draft payloads
        // never pass through the person-message owner or its persistence.
        if bytes.len() <= 256
            && serde_json::from_slice::<runtime_collab::private_draft::Request>(bytes).is_ok()
        {
            return self.private_draft_dispatch(bytes, original);
        }
        let actor = self.person_chat_actor(bytes, original)?;
        let route = self.route();
        let service = self
            .state
            .services
            .person_chat
            .as_ref()
            .ok_or("private person owner unavailable")?;
        // The owner's serialized transaction checks fresh workspace membership,
        // certificate revocation and participants before answering.
        let reply = service.command(&route.cluster_id, &actor, bytes)?;
        if reply.len() > 1024 * 1024 {
            return Err("private conversation reply exceeds bound".into());
        }
        Ok(reply)
    }
}

/// Original app actor, never an Agent attestor promoted into a person.
struct OriginalPrivateAppActor(tilo_user_identity::DeviceCert);
