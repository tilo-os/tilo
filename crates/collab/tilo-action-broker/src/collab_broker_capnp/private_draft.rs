//! Bounded, broker-private editor transfer. Tokens are correlation, not authority.
use super::{
    Arc, CapnpBrokerState, CapnpChannelHandle, InvocationOwner, PrivateWorkerRegistration,
};
use runtime_collab::private_draft::{Draft, Request, Target};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tilo_user_identity::DeviceCert;

type Origin = Arc<dyn Fn() -> Result<DeviceCert, String> + Send + Sync>;
pub(super) type Drafts = HashMap<uuid::Uuid, Entry>;
pub(super) struct Entry {
    pub(super) expires: Instant,
    certificate: DeviceCert,
    origin: Origin,
    draft: Draft,
}

/// The native Chat app this person has open in `workspace`: a worker on one
/// of Chat's routes there, under this very device certificate. A draft is
/// handed to the record its adopt action is delivered to, so any of them
/// may adopt it.
fn is_persons_chat(
    worker: &PrivateWorkerRegistration,
    workspace: &str,
    certificate: &DeviceCert,
) -> bool {
    worker.route.app_id == "so.tilo.chat"
        && worker.route.cluster_id == workspace
        && worker.certificate.user_public_key == certificate.user_public_key
        && worker.certificate.device_public_key == certificate.device_public_key
}

impl CapnpBrokerState {
    pub(super) fn mint_private_draft(
        &self,
        certificate: DeviceCert,
        origin: Origin,
        draft: Draft,
    ) -> Result<uuid::Uuid, String> {
        draft.editor.validate()?;
        match &draft.target {
            Target::Person { public_key }
                if public_key.len() == 64
                    && public_key
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => {}
            Target::Person { .. } => return Err("invalid private draft target".into()),
        }
        let chat_is_open = self
            .host_workers
            .lock()
            .map_err(|_| "native Chat registry unavailable")?
            .values()
            .filter_map(|worker| worker.worker_registration.as_ref())
            .any(|worker| is_persons_chat(worker, &draft.workspace, &certificate));
        if !chat_is_open {
            return Err("native Chat worker unavailable; open Chat and retry".into());
        }
        let mut drafts = self
            .private_drafts
            .lock()
            .map_err(|_| "private draft store unavailable")?;
        drafts.retain(|_, entry| entry.expires > Instant::now());
        if drafts.len() >= 16 {
            return Err("private draft handoff capacity reached; retry after a minute".into());
        }
        let expires = Instant::now()
            .checked_add(Duration::from_secs(60))
            .ok_or("private draft expiry exceeds clock range")?;
        let token = uuid::Uuid::new_v4();
        drafts.insert(
            token,
            Entry {
                expires,
                certificate,
                origin,
                draft,
            },
        );
        drop(drafts);
        self.local_turn.notify_one();
        Ok(token)
    }
}

impl CapnpChannelHandle {
    /// Prove the original token-only action, original Person and exact native worker.
    /// Called on fetch, acknowledgement and each pair open performed during adoption.
    pub(super) fn private_draft_authority(
        &self,
        token: uuid::Uuid,
        original: &[u8],
    ) -> Result<(DeviceCert, Draft), String> {
        let id = uuid::Uuid::from_slice(original)
            .map_err(|_| "private draft requires original invocation")?;
        let (peer, worker) = self.private_chat_worker()?;
        let data = {
            let pending = self
                .state
                .pending_invocations
                .lock()
                .map_err(|_| "private invocation unavailable")?;
            let mut matching = pending
                .values()
                .filter(|entry| entry.peer_id == peer && entry.update.invocation().0 == id);
            let entry = matching
                .next()
                .ok_or("original private draft invocation unavailable")?;
            if matching.next().is_some()
                || entry.update.is_terminal()
                || entry.cancellation_requested_at.is_some()
                || matches!(entry.owner, InvocationOwner::NativeTask { .. })
            {
                return Err("private draft invocation is no longer live".into());
            }
            let original = entry.original.clone();
            drop(pending);
            original.ok_or("private draft original person unavailable")?
        };
        let actor = data.private_draft_caller(&self.state, peer, &worker, token)?;
        // Copy the origin callback out before revalidation: it consults broker state.
        let (origin, certificate, draft, expires) = {
            let mut drafts = self
                .state
                .private_drafts
                .lock()
                .map_err(|_| "private draft store unavailable")?;
            drafts.retain(|_, entry| entry.expires > Instant::now());
            let entry = drafts
                .get(&token)
                .ok_or("private draft token expired or already adopted")?;
            let retained = (
                entry.origin.clone(),
                entry.certificate.clone(),
                entry.draft.clone(),
                entry.expires,
            );
            drop(drafts);
            retained
        };
        // The adopt action was delivered to this worker's own route (checked
        // with the original invocation above): that record is the draft's.
        if actor != certificate
            || !is_persons_chat(&worker, &draft.workspace, &certificate)
            || draft.workspace != self.route().cluster_id
            || origin()? != certificate
            || expires <= Instant::now()
        {
            return Err("private draft identity, workspace or worker changed".into());
        }
        Ok((actor, draft))
    }

    pub(super) fn private_draft_dispatch(
        &self,
        bytes: &[u8],
        original: &[u8],
    ) -> Result<Vec<u8>, String> {
        if bytes.len() > 256 {
            return Err("private draft request exceeds bound".into());
        }
        let request: Request =
            serde_json::from_slice(bytes).map_err(|_| "invalid private draft request")?;
        let (_, draft) = self.private_draft_authority(request.private_draft, original)?;
        if request.acknowledge {
            self.state
                .private_drafts
                .lock()
                .map_err(|_| "private draft store unavailable")?
                .remove(&request.private_draft)
                .ok_or("private draft already adopted")?;
            Ok(b"null".to_vec())
        } else {
            let reply = serde_json::to_vec(&draft).map_err(|_| "private draft encoding failed")?;
            if reply.len() > 128 * 1024 {
                return Err("private draft reply exceeds bound".into());
            }
            Ok(reply)
        }
    }
}
