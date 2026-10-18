//! Private person authority comes from the native app's certified transport.
use super::{CapnpChannelHandle, InvocationOwner, PrivateWorkerRegistration};
use crate::services::person_chat::PersonChatCommand;
use capnp::capability::Promise;
use runtime_collab::raw::{channel_handle, person_chat_stream};
use std::{cell::RefCell, rc::Rc};

/// Original app actor, never an Agent attestor promoted into a person.
struct OriginalPrivateAppActor(tilo_user_identity::DeviceCert);

/// A successful owner open establishes a view, not an ongoing invocation.
/// Retain the exact original person and actual native worker binding.
pub(super) struct SelectedView {
    actor: OriginalPrivateAppActor,
    worker: PrivateWorkerRegistration,
}
type ViewKey = (String, uuid::Uuid);
pub(super) type SelectedViews = std::collections::HashMap<ViewKey, Rc<SelectedView>>;
const MAX_SELECTED_VIEWS: usize = 32;

impl CapnpChannelHandle {
    fn private_stream_actor(
        &self,
        peer: &str,
        original: &[u8],
        selected: Option<&Rc<SelectedView>>,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if peer.len() != 64 {
            return Err("invalid private conversation peer".into());
        }
        let actor = if original.is_empty() {
            let command = serde_json::to_vec(&PersonChatCommand::Read {
                public_key: peer.to_owned(),
            })
            .map_err(|_| "invalid private conversation peer")?;
            self.person_chat_actor(&command, original)?
        } else {
            let key = view_key(peer, original)?;
            let views = self.person_chat_views.borrow();
            let view = views.get(&key).ok_or("private selected view has closed")?;
            if selected.is_some_and(|selected| !Rc::ptr_eq(selected, view)) {
                return Err("private selected view was replaced".into());
            }
            // Never substitute this worker's local person for the original.
            let (_, worker) = self.private_chat_worker()?;
            if worker != view.worker {
                return Err("private selected view worker binding changed".into());
            }
            view.actor.0.clone()
        };
        Ok(actor)
    }

    fn private_changes(
        &self,
        peer: &str,
        original: &[u8],
        selected: Option<&Rc<SelectedView>>,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        let actor = self.private_stream_actor(peer, original, selected)?;
        self.state
            .services
            .person_chat
            .as_ref()
            .ok_or("private person owner unavailable")?
            .subscribe(&self.route().cluster_id, &actor, peer)
    }

    pub(super) fn subscribe_person_chat_dispatch(
        self: Rc<Self>,
        params: &channel_handle::SubscribePersonChatParams,
        mut results: channel_handle::SubscribePersonChatResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from((|| {
            let params = params.get()?;
            let peer = params.get_public_key()?.to_str()?.to_owned();
            let original = params.get_on_behalf_of()?.to_vec();
            let selected = if original.is_empty() {
                None
            } else {
                let key = view_key(&peer, &original).map_err(capnp::Error::failed)?;
                Some(
                    self.person_chat_views
                        .borrow()
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| {
                            capnp::Error::failed("private selected view unavailable".into())
                        })?,
                )
            };
            let changes = self
                .private_changes(&peer, &original, selected.as_ref())
                .map_err(capnp::Error::failed)?;
            let stream: person_chat_stream::Client = capnp_rpc::new_client(PrivateChanges {
                channel: self,
                peer,
                original,
                selected,
                changes: RefCell::new(Some(changes)),
                initial: std::cell::Cell::new(true),
            });
            results.get().set_stream(stream);
            Ok(())
        })())
    }

    fn person_chat_actor(
        &self,
        bytes: &[u8],
        original: &[u8],
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if bytes.len() > 24 * 1024 || !matches!(original.len(), 0 | 16) {
            return Err("invalid private person request size".into());
        }
        let command: PersonChatCommand =
            serde_json::from_slice(bytes).map_err(|_| "invalid private person command")?;
        let (peer, registration) = self.private_chat_worker()?;
        let actor = if original.is_empty() {
            OriginalPrivateAppActor(registration.certificate)
        } else {
            let id = uuid::Uuid::from_slice(original)
                .map_err(|_| "invalid original private invocation")?;
            let pending = self
                .state
                .pending_invocations
                .lock()
                .map_err(|_| "original private invocation unavailable")?;
            let mut matching = pending
                .values()
                .filter(|entry| entry.peer_id == peer && entry.update.invocation().0 == id);
            let entry = matching
                .next()
                .ok_or("original private invocation is not retained")?;
            if matching.next().is_some()
                || entry.update.is_terminal()
                || entry.cancellation_requested_at.is_some()
                || matches!(entry.owner, InvocationOwner::NativeTask { .. })
            {
                return Err(
                    "native tasks and cancelled invocations cannot enter the person door".into(),
                );
            }
            let original = entry
                .original
                .clone()
                .ok_or("original private app actor unavailable")?;
            drop(pending);
            if let Some(token) = original.private_draft_token() {
                let (actor, draft) = self.private_draft_authority(token, id.as_bytes())?;
                let runtime_collab::private_draft::Target::Person { public_key } = draft.target
                else {
                    return Err("task draft cannot enter a person conversation".into());
                };
                if !matches!(&command,
                    PersonChatCommand::Open { public_key: peer } | PersonChatCommand::Read { public_key: peer }
                        if peer == &public_key)
                {
                    return Err("private draft authorizes only its exact pair open".into());
                }
                return Ok(actor);
            }
            OriginalPrivateAppActor(original.person_chat_caller(
                &self.state,
                peer,
                &registration,
                &command,
            )?)
        };
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
            .iter()
            .find_map(|(peer, worker)| {
                (worker.worker_registration.as_ref() == Some(registration)).then_some(*peer)
            })
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
        let command: PersonChatCommand =
            serde_json::from_slice(bytes).map_err(|_| "invalid private person command")?;
        let selected = match &command {
            PersonChatCommand::Open { public_key } if !original.is_empty() => {
                let key = view_key(public_key, original)?;
                let views = self.person_chat_views.borrow();
                if views.len() >= MAX_SELECTED_VIEWS && !views.contains_key(&key) {
                    return Err("too many private selected views".into());
                }
                let (_, worker) = self.private_chat_worker()?;
                Some((
                    key,
                    Rc::new(SelectedView {
                        actor: OriginalPrivateAppActor(actor.clone()),
                        worker,
                    }),
                ))
            }
            _ => None,
        };
        let route = self.route();
        let service = self
            .state
            .services
            .person_chat
            .as_ref()
            .ok_or("private person owner unavailable")?;
        // The owner's serialized transaction checks fresh room membership,
        // certificate revocation and participants before reading or committing.
        let reply = service.command(&route.cluster_id, &actor, bytes)?;
        if reply.len() > 1024 * 1024 {
            return Err("private conversation reply exceeds bound".into());
        }
        // No lease on failed owner open. General commands still require a live
        // original; the lease grants only pair-scoped stream events and reads.
        if let Some((key, view)) = selected {
            self.person_chat_views.borrow_mut().insert(key, view);
        }
        Ok(reply)
    }
}

fn view_key(peer: &str, original: &[u8]) -> Result<ViewKey, String> {
    Ok((
        peer.to_owned(),
        uuid::Uuid::from_slice(original).map_err(|_| "invalid original private invocation")?,
    ))
}

struct PrivateChanges {
    channel: Rc<CapnpChannelHandle>,
    peer: String,
    original: Vec<u8>,
    selected: Option<Rc<SelectedView>>,
    changes: RefCell<Option<tokio::sync::watch::Receiver<u64>>>,
    initial: std::cell::Cell<bool>,
}
impl Drop for PrivateChanges {
    fn drop(&mut self) {
        if let (Some(selected), Ok(key)) = (&self.selected, view_key(&self.peer, &self.original)) {
            let mut views = self.channel.person_chat_views.borrow_mut();
            if views
                .get(&key)
                .is_some_and(|view| Rc::ptr_eq(view, selected))
            {
                views.remove(&key);
            }
        }
    }
}
impl person_chat_stream::Server for PrivateChanges {
    fn read(
        self: Rc<Self>,
        _params: person_chat_stream::ReadParams,
        mut results: person_chat_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from((|| {
            // Revalidate the exact selected actor/worker on every private fetch.
            // Never re-enter terminal invocation dispatch or borrow a local actor.
            let actor = self
                .channel
                .private_stream_actor(&self.peer, &self.original, self.selected.as_ref())
                .map_err(capnp::Error::failed)?;
            let command = serde_json::to_vec(&PersonChatCommand::Read {
                public_key: self.peer.clone(),
            })
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
            let reply = self
                .channel
                .state
                .services
                .person_chat
                .as_ref()
                .ok_or_else(|| capnp::Error::failed("private person owner unavailable".into()))?
                .command(&self.channel.route().cluster_id, &actor, &command)
                .map_err(capnp::Error::failed)?;
            // The owner reauthorizes the pair and bounds serialization; enforce
            // the wire bound again before copying into the capability reply.
            if reply.len() > 1024 * 1024 {
                return Err(capnp::Error::failed(
                    "private conversation reply exceeds bound".into(),
                ));
            }
            results.get().set_reply(&reply);
            Ok(())
        })())
    }

    fn next(
        self: Rc<Self>,
        _params: person_chat_stream::NextParams,
        mut results: person_chat_stream::NextResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            self.channel
                .private_changes(&self.peer, &self.original, self.selected.as_ref())
                .map_err(capnp::Error::failed)?;
            let mut changes = self
                .changes
                .borrow_mut()
                .take()
                .ok_or_else(|| capnp::Error::failed("private next already pending".into()))?;
            if !self.initial.replace(false) {
                changes.changed().await.map_err(|_| {
                    capnp::Error::failed("private conversation owner closed".into())
                })?;
            }
            // Waiting grants no authority: certify both people again after wake.
            self.channel
                .private_changes(&self.peer, &self.original, self.selected.as_ref())
                .map_err(capnp::Error::failed)?;
            let generation = *changes.borrow_and_update();
            *self.changes.borrow_mut() = Some(changes);
            results.get().set_generation(generation);
            Ok(())
        })
    }
}
