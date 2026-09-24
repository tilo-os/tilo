//! Closed HTTPS carrier over the existing Peer key, state, floor and storage lease.
use super::*;
use crate::peer_delivery::{
    ReplayCut,
    admission::{self, CutAdmission},
};
use crate::peer_delivery_auth::{self, DeliveryAuthority, HttpStep, PreparedRequest};
use crate::peer_delivery_cursor::StoredDeliveryCursor;
use crate::peer_journal::AcknowledgedDescriptorFloor;
use std::collections::VecDeque;
use tilo_relay::delivery_v3::{
    cursor::CursorToken,
    framing::{self, MessageType},
    private_wire::{self, PrivateOperation, PrivateScope},
    state::{
        self, AdmissionOutcome, CommitScope, CursorAck, FlowCredit, StateAdmission, StateItemChunk,
    },
};
use tilo_relay::peer_actions::{ActionMessage, DirectedAction};
#[path = "wasm_transfer.rs"]
mod transfers;

/// Retained request handles (each reserves its bounded response backing).
const MAX_PENDING_REQUESTS: usize = 8;
/// Handles a new inline state request leaves free: held poll, private poll
/// and one ACK or private action.
const STATE_RESERVED_HANDLES: usize = 3;
#[derive(Default)]
pub(super) struct DeliveryState {
    bootstrap: Option<peer_delivery_auth::Bootstrap>,
    descriptor: Option<peer_delivery_auth::VerifiedBootstrap>,
    pub(super) authority: Option<Rc<RefCell<DeliveryAuthority>>>,
    pending: BTreeMap<u32, Pending>,
    incoming: BTreeMap<u32, automerge::resource_limits::NativeReservation>,
    next: u32,
    actions: VecDeque<DirectedAction>,
    pub(super) broker: Option<uuid::Uuid>,
    upload: Option<Upload>,
    /// Inline multi-item state requests in flight (bounded, in outbox order).
    window: crate::peer_delivery::upload::UploadWindow,
    cursor: Option<StoredDeliveryCursor>,
    pending_cursor: Option<StoredDeliveryCursor>,
    /// A pipelined poll page that arrived while the previous cut's storage
    /// transaction was still pending. Installed only after that exact commit.
    deferred_poll: Option<(u64, Vec<u8>)>,
    transfer: Option<transfers::TransferState>,
    recovery: transfers::ResumeState,
    acked: Option<Vec<u8>>,
    cursor_recovery: crate::peer_delivery::cursor_recovery::CursorRecovery,
    needs_snapshot: bool,
    live: bool,
}
struct Upload {
    id: [u8; 16],
    digest: [u8; 32],
    bytes: Vec<u8>,
    progress: crate::peer_delivery::upload::UploadProgress,
}
enum Pending {
    Bootstrap,
    Http(u32),
    Poll(u64),
    PrivatePoll,
    Snapshot,
    SnapshotResume,
    SnapshotRenew(u32),
    StageList,
    SnapshotChunk(u32),
    StageRead,
    StageWrite,
    Private {
        operation: PrivateOperation,
        action: DirectedAction,
    },
    State {
        delivery: [u8; 16],
        digest: [u8; 32],
        end: usize,
    },
    /// One inline multi-item state request of the upload window.
    StateBatch(u64),
    Ack(Vec<u8>),
    Detach,
}
impl DeliveryState {
    /// LIVE_V1: an early forwarded signed change may install only between
    /// durable cuts (no snapshot transfer, staged cut or reset in progress).
    pub(super) fn live_admission_quiet(&self) -> bool {
        self.live
            && self.transfer.is_none()
            && self.pending_cursor.is_none()
            && !self.needs_snapshot
    }
    pub fn close(&mut self) {
        self.stop();
        if let Some(authority) = self.authority.take() {
            authority.borrow_mut().close();
        }
    }
    fn stop(&mut self) {
        for (_, pending) in std::mem::take(&mut self.pending) {
            if let Pending::Http(id) = pending {
                if let Some(authority) = &self.authority {
                    authority.borrow_mut().forget(id);
                }
            }
        }
        self.bootstrap = None;
        self.descriptor = None;
        self.actions.clear();
        self.broker = None;
        self.upload = None;
        self.window.clear();
        self.deferred_poll = None;
        self.live = false;
        self.transfer = None;
        self.recovery = Default::default();
    }
    fn retain(
        &mut self,
        pending: Pending,
        budget: &crate::peer_delivery::incoming::IncomingBudget,
    ) -> Result<u32, String> {
        if self.pending.len() >= MAX_PENDING_REQUESTS || self.incoming.len() >= MAX_PENDING_REQUESTS
        {
            return Err("delivery request capacity exceeded".into());
        }
        // Reserve before JS fetch allocation: response backing, returned JS copy,
        // WASM copy and one bounded wire DTO overlap. Retain through raw completion.
        let credit = budget.reserve(4 * 1048576 + 8192)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("delivery request handles exhausted")?;
        self.pending.insert(self.next, pending);
        self.incoming.insert(self.next, credit);
        Ok(self.next)
    }
    fn wrap(
        &mut self,
        request: PreparedRequest,
        pending: Pending,
        budget: &crate::peer_delivery::incoming::IncomingBudget,
    ) -> Result<String, String> {
        let id = self.retain(pending, budget)?;
        Ok(serde_json::json!({"kind":"request","id":id,"method":"POST","origin":request.origin,"target":request.target,"body":request.body,"proof":request.proof}).to_string())
    }
}
fn owner(id: u32) -> Result<Rc<Peer>, JsValue> {
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed Peer"))?;
    if peer.closed.get() {
        return Err(error("closed Peer"));
    }
    Ok(peer)
}
fn identity(peer: &Peer) -> CursorIdentity {
    media_api::identity(peer)
}
fn check(peer: &Peer) -> Result<Rc<RefCell<DeliveryAuthority>>, JsValue> {
    let authority = peer
        .delivery
        .borrow()
        .authority
        .clone()
        .ok_or_else(|| error("delivery custody not installed"))?;
    let current = media_api::with_view(peer, |view| {
        if authority.borrow().product_ready(view) {
            Ok(())
        } else {
            Err("current full HTTPS delivery capability unavailable".into())
        }
    });
    if current.is_err() {
        let changed = peer.media.borrow_mut().expire();
        if changed {
            let _ = peer.callback.call1(
                &JsValue::NULL,
                &JsValue::from_str("{\"mediaAuthorizationExpired\":true}"),
            );
        }
    }
    current?;
    Ok(authority)
}
fn committed_cursor(peer: &Peer) -> Result<Option<StoredDeliveryCursor>, String> {
    let room = peer.state.borrow().room().to_owned();
    let Some(storage) = crate::peer_journal::committed_storage(&peer.cache, &room) else {
        return Ok(None);
    };
    let cursor = storage?.delivery;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.identity != identity(peer))
    {
        return Err("committed delivery cursor identity changed".into());
    }
    Ok(cursor)
}
fn ready(peer: &Rc<Peer>) -> Result<String, JsValue> {
    check(peer)?;
    let cursor = committed_cursor(peer).map_err(error)?;
    let mut delivery = peer.delivery.borrow_mut();
    delivery.cursor = cursor;
    delivery.pending_cursor = None;
    delivery.deferred_poll = None;
    delivery.acked = None;
    delivery.cursor_recovery.reset().map_err(error)?;
    delivery.needs_snapshot = delivery.cursor.is_none();
    delivery.live = true;
    drop(delivery);
    peer.action_v2.set(true);
    peer.transport_connected.set(true);
    peer.last_relay_contact.set(Some(js_sys::Date::now()));
    report(peer, "admitting", None);
    Ok("{\"kind\":\"ready\"}".into())
}
#[wasm_bindgen]
pub fn peer_delivery_begin(id: u32) -> Result<String, JsValue> {
    let peer = owner(id)?;
    if check(&peer).is_ok() {
        return ready(&peer);
    }
    peer.media.borrow_mut().expire();
    let old = peer.delivery.borrow_mut().authority.take();
    if let Some(old) = old {
        old.borrow_mut().close();
    }
    media_api::with_view(&peer, |view| {
        let hint = peer.media_enrollment.hint.borrow();
        let hint = hint
            .as_ref()
            .ok_or("installed HTTPS descriptor unavailable")?;
        let (bootstrap, request) = peer_delivery_auth::Bootstrap::start(view, hint)?;
        let mut delivery = peer.delivery.borrow_mut();
        if !delivery.pending.is_empty() {
            return Err("delivery setup already pending".into());
        }
        delivery.bootstrap = Some(bootstrap);
        delivery.wrap(request, Pending::Bootstrap, &peer.incoming)
    })
}
#[wasm_bindgen]
pub fn peer_delivery_detach(id: u32) -> Result<String, JsValue> {
    let peer = owner(id)?;
    let authority = check(&peer)?;
    let session = authority
        .borrow()
        .session
        .as_ref()
        .ok_or_else(|| error("HTTP session missing"))?
        .proof
        .transcript
        .session_id;
    let token: String = session.iter().map(|byte| format!("{byte:02x}")).collect();
    request(
        &peer,
        "DELETE",
        format!("/peer/v3/sessions/{token}"),
        vec![],
        Pending::Detach,
    )
}
#[wasm_bindgen]
pub fn peer_delivery_resume(id: u32) -> Result<String, JsValue> {
    let peer = owner(id)?;
    if peer.delivery.borrow().descriptor.is_none() {
        check(&peer)?;
        let pending = peer.delivery.borrow().pending_cursor.clone();
        if let Some(pending) = pending {
            let actual = peer
                .batches
                .borrow()
                .committed_cursor(peer.transport_generation.get())
                .cloned();
            admission::confirm_commit(&pending, actual.as_ref()).map_err(error)?;
            let mut state = peer.delivery.borrow_mut();
            state.cursor = actual;
            state.pending_cursor = None;
            // A snapshot cut is complete only once its transaction consumed it.
            if state
                .transfer
                .as_ref()
                .is_some_and(transfers::waiting_commit)
            {
                state.transfer = None;
                state.needs_snapshot = false;
            }
            let deferred = state.deferred_poll.take();
            drop(state);
            peer.initial_admission_replay.borrow_mut().finish();
            report(&peer, "admitting", None);
            // The pipelined page was requested at exactly this cursor; it is
            // admitted only now that the cut it follows is durable.
            if let Some((epoch, bytes)) = deferred {
                return poll_page(&peer, epoch, bytes);
            }
        }
        return Ok("{\"kind\":\"idle\"}".into());
    }
    let floor = AcknowledgedDescriptorFloor::read(&peer.cache, &identity(&peer)).map_err(error)?;
    media_api::with_view(&peer, |view| {
        let descriptor = peer
            .delivery
            .borrow_mut()
            .descriptor
            .take()
            .ok_or("verified delivery bootstrap missing")?;
        let authority = Rc::new(RefCell::new(DeliveryAuthority::new(
            view.lease_epoch,
            descriptor.descriptor,
            floor,
        )?));
        let request = authority.borrow_mut().start_product_http(view)?;
        let inner = request.id;
        let mut delivery = peer.delivery.borrow_mut();
        delivery.authority = Some(authority);
        delivery.wrap(request, Pending::Http(inner), &peer.incoming)
    })
}
fn request(
    peer: &Peer,
    method: &str,
    target: String,
    body: Vec<u8>,
    pending: Pending,
) -> Result<String, JsValue> {
    let authority = check(peer)?;
    let proof = media_api::with_view(peer, |view| {
        authority.borrow().prove(view, method, &target, &body)
    })?;
    let origin = authority.borrow().descriptor.origin().to_owned();
    let id = peer
        .delivery
        .borrow_mut()
        .retain(pending, &peer.incoming)
        .map_err(error)?;
    Ok(serde_json::json!({"kind":"request","id":id,"method":method,"origin":origin,"target":target,"body":body,"proof":proof}).to_string())
}
fn framed(kind: MessageType, payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    framing::append_frame(&mut out, kind, payload, 1048576).map_err(|e| e.to_string())?;
    Ok(out)
}
pub(super) fn queue_action(peer: &Peer, action: DirectedAction) -> Result<(), String> {
    if !peer.transport_connected.get() || peer.closed.get() {
        return Err("HTTPS private carrier unavailable".into());
    }
    let mut delivery = peer.delivery.borrow_mut();
    if delivery.actions.len()
        + delivery
            .pending
            .values()
            .filter(|p| matches!(p, Pending::Private { .. }))
            .count()
        >= 8
    {
        return Err("private delivery capacity exceeded".into());
    }
    delivery.actions.push_back(action);
    drop(delivery);
    wake(peer);
    Ok(())
}
fn wake(peer: &Peer) {
    let _ = peer.callback.call1(
        &JsValue::NULL,
        &JsValue::from_str("{\"deliveryReady\":true}"),
    );
}
pub(super) fn queue_committed(peer: &Peer) -> Result<(), String> {
    if peer.closed.get() {
        return Ok(());
    }
    if peer.delivery.borrow().live {
        wake(peer);
    }
    Ok(())
}
pub(super) fn request_snapshot(peer: &Peer) -> Result<(), String> {
    peer.delivery.borrow_mut().needs_snapshot = true;
    wake(peer);
    Ok(())
}
pub(super) fn stop(peer: &Peer) {
    peer.delivery.borrow_mut().stop();
    // The stopped session's staging handles are gone; pending journal work and
    // its cursor stay in the cache and commit later without the stage.
    peer.batches.borrow_mut().abandon_transfer();
    peer.transport_connected.set(false);
    peer.action_v2.set(false);
    *peer.actions.borrow_mut() = Default::default();
    *peer.agent_reads.borrow_mut() = Default::default();
    if let Some(next) = peer.transport_generation.get().checked_add(1) {
        peer.transport_generation.set(next);
    } else {
        peer.closed.set(true);
    }
}
#[wasm_bindgen]
pub fn peer_delivery_stop(id: u32) {
    if let Some(peer) = PEERS.with(|peers| peers.borrow().get(&id).cloned()) {
        stop(&peer);
    }
}
/// Drop the HTTP session the host refused on its data stream. The next begin
/// bootstraps a fresh session, whose finish re-registers the durable consumer
/// under the host's current authorization view.
#[wasm_bindgen]
pub fn peer_delivery_discard_session(id: u32) {
    if let Some(peer) = PEERS.with(|peers| peers.borrow().get(&id).cloned()) {
        let old = peer.delivery.borrow_mut().authority.take();
        if let Some(old) = old {
            old.borrow_mut().close();
        }
    }
}
#[wasm_bindgen]
pub fn peer_delivery_forget(id: u32, request: u32) {
    if let Ok(peer) = owner(id) {
        peer.delivery.borrow_mut().incoming.remove(&request);
        let pending = peer.delivery.borrow_mut().pending.remove(&request);
        if let Some(Pending::Http(inner)) = pending {
            if let Some(authority) = &peer.delivery.borrow().authority {
                authority.borrow_mut().forget(inner);
            }
        }
        // An unanswered inline state request (lost reply, timeout, transport
        // retry) releases its items; a resend returns the durable receipt.
        if let Some(Pending::StateBatch(batch)) = pending {
            peer.delivery.borrow_mut().window.abandon(batch);
        }
    }
}
#[wasm_bindgen]
pub fn peer_delivery_next(id: u32) -> Result<String, JsValue> {
    let peer = owner(id)?;
    check(&peer)?;
    if !peer.delivery.borrow().live {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    // Every handle is in use: wait for one to settle instead of failing the
    // carrier in `retain` (the executor wakes again after each completion).
    if peer.delivery.borrow().pending.len() >= MAX_PENDING_REQUESTS {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    if peer.delivery.borrow().pending_cursor.is_some() {
        // Keep one poll outstanding while the previous cut commits: request the
        // next page at the pending cut's cursor (a signed boundary the host
        // issued). Its page is deferred until that commit is confirmed; the
        // ACK still follows the confirmed commit only.
        return pipelined_poll(&peer);
    }
    if peer.delivery.borrow().transfer.is_some() {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    if !peer.delivery.borrow().recovery.scanned() {
        return transfers::begin(&peer);
    }
    if peer.delivery.borrow().needs_snapshot {
        return transfers::begin(&peer);
    }
    // This cursor was read from acknowledged canonical journal bytes in ready,
    // or installed by resume only after exact atomic batch acknowledgement.
    let saved = {
        let state = peer.delivery.borrow();
        state
            .cursor_recovery
            .ack_allowed()
            .then(|| state.cursor.clone())
            .flatten()
    };
    if let Some(saved) = saved {
        let delivery = peer.delivery.borrow();
        let required = delivery.acked.as_ref() != Some(&saved.token)
            && !delivery
                .pending
                .values()
                .any(|p| matches!(p, Pending::Ack(_)));
        drop(delivery);
        if required {
            let body = framed(
                MessageType::CursorAck,
                &CursorAck::new(CursorToken::from_bytes(&saved.token).map_err(error)?)
                    .encode()
                    .map_err(error)?,
            )
            .map_err(error)?;
            return request(
                &peer,
                "POST",
                "/peer/v3/acks".into(),
                body,
                Pending::Ack(saved.token),
            );
        }
    }
    let action = peer.delivery.borrow_mut().actions.pop_front();
    if let Some(action) = action {
        let operation = match action.message {
            ActionMessage::CatalogueRefresh { .. } => PrivateOperation::Catalogue,
            ActionMessage::InvokeBound { .. } => PrivateOperation::Invoke,
            ActionMessage::Status { .. } => PrivateOperation::Status,
            ActionMessage::Cancel { .. } => PrivateOperation::Cancel,
            ActionMessage::AgentReadV1 { .. } => PrivateOperation::AgentRead,
            _ => return Err(error("unsupported private operation")),
        };
        let scope = PrivateScope {
            room: action.room,
            source: peer._key.verifying_key().to_bytes(),
            target: peer.state.borrow().authority().device_public_key,
        };
        let body = private_wire::encode_request(operation, scope, &action).map_err(error)?;
        return request(
            &peer,
            "POST",
            operation.path().into(),
            body,
            Pending::Private { operation, action },
        );
    }
    if !peer.cache.has_pending_writes() {
        if let Some(step) = next_inline_batch(&peer)? {
            return Ok(step);
        }
    }
    if !peer
        .delivery
        .borrow()
        .pending
        .values()
        .any(|p| matches!(p, Pending::State { .. }))
        && !peer.cache.has_pending_writes()
    {
        // The staged path starts only when no inline request is in flight and
        // the oldest unsubmitted item exceeds the inline bound.
        if peer.delivery.borrow().upload.is_none() && peer.delivery.borrow().window.is_empty() {
            let outbox = peer.outbox.borrow();
            for message in qos_peer_state::outbox::publication_batch(&outbox, SystemTime::now()) {
                let bytes = postcard::to_allocvec(message).map_err(error)?;
                let digest = state::canonical_item_digest(&bytes);
                if peer.published.borrow().contains(&digest) {
                    continue;
                }
                if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
                    return Err(error("canonical upload exceeds supported bound"));
                }
                if bytes.len() <= crate::peer_delivery::upload::INLINE_ITEM_BYTES {
                    // Inline items travel through the window, never staged.
                    break;
                }
                let mut id = [0; 16];
                id.copy_from_slice(&digest[..16]);
                if id == [0; 16] {
                    id[0] = 1;
                }
                peer.delivery.borrow_mut().upload = Some(Upload {
                    id,
                    digest,
                    progress: crate::peer_delivery::upload::UploadProgress::new(bytes.len())
                        .map_err(error)?,
                    bytes,
                });
                break;
            }
        }
        let mut delivery = peer.delivery.borrow_mut();
        if let Some(upload) = &mut delivery.upload {
            let range = upload.progress.next().map_err(error)?;
            let end = range.end;
            let chunk = StateItemChunk::new(
                upload.id,
                upload.digest,
                upload.bytes.len() as u64,
                range.start as u64,
                &upload.bytes[range],
            )
            .map_err(error)?;
            let body = framed(MessageType::StateItemChunk, &chunk.encode().map_err(error)?)
                .map_err(error)?;
            let pending = Pending::State {
                delivery: upload.id,
                digest: upload.digest,
                end,
            };
            drop(delivery);
            return request(&peer, "POST", "/peer/v3/state".into(), body, pending);
        }
    }
    let private_needed = {
        let delivery = peer.delivery.borrow();
        delivery.broker.is_some()
            && !delivery
                .pending
                .values()
                .any(|p| matches!(p, Pending::PrivatePoll))
    };
    if private_needed {
        return request(
            &peer,
            "GET",
            private_wire::PRIVATE_EVENTS_TARGET.into(),
            vec![],
            Pending::PrivatePoll,
        );
    }
    let delivery = peer.delivery.borrow();
    if delivery
        .pending
        .values()
        .any(|p| matches!(p, Pending::Poll(_)))
    {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    let target = match &delivery.cursor {
        Some(cursor) => format!(
            "/peer/v3/events?cursor={}&mode=poll",
            state::encode_cursor_url(&cursor.token).map_err(error)?
        ),
        None => "/peer/v3/events?mode=poll".into(),
    };
    let epoch = delivery.cursor_recovery.epoch();
    drop(delivery);
    request(&peer, "GET", target, vec![], Pending::Poll(epoch))
}
#[wasm_bindgen]
pub fn peer_delivery_advance(
    id: u32,
    request_id: u32,
    input: js_sys::Uint8Array,
) -> Result<String, JsValue> {
    if input.length() > 1048576 {
        return Err(error("delivery response exceeds bound"));
    }
    let peer = owner(id)?;
    let bytes = input.to_vec();
    let pending = peer
        .delivery
        .borrow_mut()
        .pending
        .remove(&request_id)
        .ok_or_else(|| error("unknown or consumed delivery operation"))?;
    match pending {
        Pending::Bootstrap => media_api::with_view(&peer, |view| {
            let bootstrap = peer
                .delivery
                .borrow_mut()
                .bootstrap
                .take()
                .ok_or("bootstrap already consumed")?;
            let verified = bootstrap.finish(view, &bytes)?;
            let mut next = peer.journal_storage.borrow().clone();
            next.advance_descriptor(verified.floor.clone())?;
            let previous = peer.journal_storage.replace(next);
            // The journal carries the committed delivery cursor, so it must
            // describe exactly the cached records, which hold the view: stage
            // the view and promote its outstanding forwarded changes (as a
            // durable cut settles them) instead of staging the durable basis.
            if let Err(error) = stage_journal(&peer, view.peer, &peer.outbox.borrow()) {
                peer.journal_storage.replace(previous);
                return Err(error);
            }
            super::live_api::promote_for_local_publication(&peer);
            peer.delivery.borrow_mut().descriptor = Some(verified);
            Ok("{\"kind\":\"persist\"}".into())
        }),
        Pending::Http(inner) => {
            let authority = peer
                .delivery
                .borrow()
                .authority
                .clone()
                .ok_or_else(|| error("HTTP custody missing"))?;
            let step = media_api::with_view(&peer, |view| {
                authority.borrow_mut().advance(view, inner, &bytes)
            })?;
            match step {
                HttpStep::Ready => ready(&peer),
                HttpStep::Request(request) => {
                    let inner = request.id;
                    peer.delivery
                        .borrow_mut()
                        .wrap(request, Pending::Http(inner), &peer.incoming)
                        .map_err(error)
                }
            }
        }
        Pending::Snapshot => {
            let (kind, payload, tail) = framing::decode_frame_prefix(&bytes).map_err(error)?;
            if kind == MessageType::SnapshotRequired && tail.is_empty() {
                // A baseline refused for a changed authorization view needs a
                // fresh session: only session finish re-registers the consumer.
                let reset = state::SnapshotRequired::decode(payload).map_err(error)?;
                if reset.reason() == state::ResetReason::AuthorizationChanged {
                    let old = peer.delivery.borrow_mut().authority.take();
                    if let Some(old) = old {
                        old.borrow_mut().close();
                    }
                    return Err(error(
                        "snapshot authorization changed; session renewal required",
                    ));
                }
            }
            if kind != MessageType::SnapshotManifest || !tail.is_empty() {
                return Err(error("expected one signed snapshot manifest"));
            }
            transfers::manifest(&peer, payload)
        }
        Pending::SnapshotResume => transfers::resumed(&peer, &bytes),
        Pending::SnapshotRenew(index) => transfers::renewed(&peer, index, &bytes),
        Pending::SnapshotChunk(index) => transfers::chunk(&peer, index, &bytes),
        Pending::StageList | Pending::StageRead | Pending::StageWrite => {
            Err(error("storage operation cannot accept network bytes"))
        }
        Pending::Ack(token) => {
            check(&peer)?;
            if !bytes.is_empty() {
                return Err(error("unexpected cursor ACK body"));
            }
            peer.delivery.borrow_mut().acked = Some(token);
            Ok("{\"kind\":\"idle\"}".into())
        }
        Pending::Detach => {
            check(&peer)?;
            if !bytes.is_empty() {
                return Err(error("unexpected detach response body"));
            }
            if let Some(authority) = &peer.delivery.borrow().authority {
                authority.borrow_mut().close();
            }
            Ok("{\"kind\":\"idle\"}".into())
        }
        Pending::Private { operation, action } => {
            check(&peer)?;
            let response =
                private_wire::decode_response(operation, &action, &bytes).map_err(error)?;
            let source_user = peer.state.borrow().authority().user_public_key;
            receive_private_action(
                &peer,
                tilo_relay::transport_v2::EnvelopeV2::Delivered {
                    action: response,
                    source_user,
                },
                peer.transport_generation.get(),
            );
            if !peer.cache.has_pending_writes() {
                Ok("{\"kind\":\"idle\"}".into())
            } else {
                Ok("{\"kind\":\"persist\"}".into())
            }
        }
        Pending::State {
            delivery,
            digest,
            end,
        } => {
            check(&peer)?;
            let (kind, payload, rest) = framing::decode_frame_prefix(&bytes).map_err(error)?;
            if !rest.is_empty() {
                return Err(error("unexpected upload response suffix"));
            }
            match kind {
                MessageType::FlowCredit => {
                    let credit = FlowCredit::decode(payload).map_err(error)?;
                    let mut state = peer.delivery.borrow_mut();
                    let upload = state
                        .upload
                        .as_mut()
                        .ok_or_else(|| error("upload custody missing"))?;
                    if credit.delivery() != delivery
                        || credit.input_digest() != digest
                        || credit.canonical_size() != upload.bytes.len() as u64
                        || upload.id != delivery
                        || upload.digest != digest
                    {
                        return Err(error("upload progress mismatch or stalled"));
                    }
                    upload
                        .progress
                        .credit(credit.next_offset())
                        .map_err(error)?;
                }
                MessageType::StateAdmission => {
                    let admission = StateAdmission::decode(payload).map_err(error)?;
                    if admission.delivery() != delivery || admission.input_digest() != digest {
                        return Err(error("upload admission identity mismatch"));
                    }
                    if !matches!(
                        admission.outcome(),
                        AdmissionOutcome::Accepted {
                            scope: CommitScope::NativeCommitted,
                            ..
                        } | AdmissionOutcome::Duplicate {
                            scope: CommitScope::NativeCommitted,
                            ..
                        }
                    ) {
                        return Err(error(format!(
                            "native upload not admitted: {:?}",
                            admission.outcome()
                        )));
                    }
                    let mut state = peer.delivery.borrow_mut();
                    let upload = state
                        .upload
                        .as_ref()
                        .ok_or_else(|| error("upload custody missing"))?;
                    if upload.id != delivery || upload.digest != digest || end > upload.bytes.len()
                    {
                        return Err(error("upload changed before admission"));
                    }
                    // Native admission marks this transport submission only. The durable
                    // outbox remains until original signed native coverage retires it.
                    peer.published.borrow_mut().insert(digest);
                    state.upload = None;
                }
                _ => return Err(error("unexpected upload response type")),
            }
            Ok("{\"kind\":\"idle\"}".into())
        }
        Pending::StateBatch(batch) => {
            check(&peer)?;
            settle_inline_batch(&peer, batch, &bytes)?;
            Ok("{\"kind\":\"idle\"}".into())
        }
        Pending::PrivatePoll => {
            check(&peer)?;
            if bytes.is_empty() {
                return Ok("{\"kind\":\"idle\"}".into());
            }
            let host = peer
                .delivery
                .borrow()
                .broker
                .ok_or_else(|| error("broker catalogue custody unavailable"))?;
            let scope = PrivateScope {
                room: uuid::Uuid::parse_str(peer.state.borrow().room()).map_err(error)?,
                source: peer.state.borrow().authority().device_public_key,
                target: peer._key.verifying_key().to_bytes(),
            };
            let response = private_wire::decode_event(scope, host, &bytes).map_err(error)?;
            let source_user = peer.state.borrow().authority().user_public_key;
            receive_private_action(
                &peer,
                tilo_relay::transport_v2::EnvelopeV2::Delivered {
                    action: response,
                    source_user,
                },
                peer.transport_generation.get(),
            );
            Ok("{\"kind\":\"idle\"}".into())
        }
        Pending::Poll(epoch) => {
            check(&peer)?;
            peer.last_relay_contact.set(Some(js_sys::Date::now()));
            if !bytes.is_empty() && peer.delivery.borrow().pending_cursor.is_some() {
                let mut delivery = peer.delivery.borrow_mut();
                if delivery.deferred_poll.is_some() {
                    return Err(error("second deferred replay page"));
                }
                delivery.deferred_poll = Some((epoch, bytes));
                return Ok("{\"kind\":\"idle\"}".into());
            }
            poll_page(&peer, epoch, bytes)
        }
    }
}
fn pipelined_poll(peer: &Rc<Peer>) -> Result<String, JsValue> {
    let delivery = peer.delivery.borrow();
    let Some(cursor) = delivery.pending_cursor.as_ref() else {
        return Ok("{\"kind\":\"idle\"}".into());
    };
    if !delivery.live
        || delivery.transfer.is_some()
        || delivery.needs_snapshot
        || delivery.deferred_poll.is_some()
        || !delivery.recovery.scanned()
        || delivery
            .pending
            .values()
            .any(|p| matches!(p, Pending::Poll(_)))
    {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    let target = format!(
        "/peer/v3/events?cursor={}&mode=poll",
        state::encode_cursor_url(&cursor.token).map_err(error)?
    );
    let epoch = delivery.cursor_recovery.epoch();
    drop(delivery);
    request(peer, "GET", target, vec![], Pending::Poll(epoch))
}
/// Admit one replay page for poll `epoch`: one complete group, or with
/// MULTI_GROUP_REPLAY_V1 several consecutive groups installed together.
fn poll_page(peer: &Rc<Peer>, epoch: u64, bytes: Vec<u8>) -> Result<String, JsValue> {
    let peer = peer.clone();
    if bytes.is_empty() {
        peer.delivery
            .borrow_mut()
            .cursor_recovery
            .accept_poll(epoch)
            .map_err(error)?;
        return Ok("{\"kind\":\"idle\"}".into());
    }
    let negotiated = peer
        .delivery
        .borrow()
        .authority
        .as_ref()
        .is_some_and(|authority| authority.borrow().multi_group_replay());
    let bounds = tilo_relay::delivery_v3::capabilities::page_bounds(if negotiated {
        tilo_relay::delivery_v3::capabilities::MULTI_GROUP_REPLAY_V1
    } else {
        0
    });
    let contents = crate::peer_delivery::collect_page(
        &identity(&peer),
        peer.transport_generation.get(),
        &peer.incoming,
        &bytes,
        bounds,
    )
    .map_err(error)?;
    match contents {
        crate::peer_delivery::PageContents::Cuts(cuts) => {
            peer.delivery
                .borrow_mut()
                .cursor_recovery
                .accept_poll(epoch)
                .map_err(error)?;
            install_cuts(&peer, cuts)
        }
        crate::peer_delivery::PageContents::Manifest(payload) => {
            let result = transfers::manifest(&peer, payload)?;
            peer.delivery
                .borrow_mut()
                .cursor_recovery
                .accept_poll(epoch)
                .map_err(error)?;
            Ok(result)
        }
        crate::peer_delivery::PageContents::Reset => {
            let mut delivery = peer.delivery.borrow_mut();
            delivery.cursor_recovery.reset().map_err(error)?;
            delivery.needs_snapshot = true;
            Ok("{\"kind\":\"idle\"}".into())
        }
    }
}
/// Plan the next inline multi-item state request: the first local change goes
/// at once, later changes coalesce (up to the host's 64-item / 1 MiB bound)
/// into the next free slot of a small bounded window. Order within a request
/// is outbox order; the host admits items sequentially.
fn next_inline_batch(peer: &Rc<Peer>) -> Result<Option<String>, JsValue> {
    use crate::peer_delivery::upload::{ItemKey, MAX_BATCH_ITEMS};
    {
        let delivery = peer.delivery.borrow();
        let staged = delivery.upload.is_some()
            || delivery
                .pending
                .values()
                .any(|p| matches!(p, Pending::State { .. }));
        // Keep request handles for the held poll, the private poll and an
        // ACK: a full retain table would fail the whole carrier.
        if staged
            || !delivery.window.can_send()
            || delivery.pending.len() + STATE_RESERVED_HANDLES >= MAX_PENDING_REQUESTS
        {
            return Ok(None);
        }
    }
    let mut candidates: Vec<(ItemKey, Vec<u8>)> = Vec::new();
    {
        let outbox = peer.outbox.borrow();
        let published = peer.published.borrow();
        for message in qos_peer_state::outbox::publication_batch(&outbox, SystemTime::now()) {
            let bytes = postcard::to_allocvec(message).map_err(error)?;
            let digest = state::canonical_item_digest(&bytes);
            // In-flight items stay candidates: the window may repeat them as
            // a resend prefix so a reordered request is self-contained.
            if published.contains(&digest) {
                continue;
            }
            if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
                return Err(error("canonical upload exceeds supported bound"));
            }
            let mut id = [0; 16];
            id.copy_from_slice(&digest[..16]);
            if id == [0; 16] {
                id[0] = 1;
            }
            let large = bytes.len() > crate::peer_delivery::upload::INLINE_ITEM_BYTES;
            candidates.push((
                ItemKey {
                    delivery: id,
                    digest,
                },
                bytes,
            ));
            if large || candidates.len() > MAX_BATCH_ITEMS {
                break;
            }
        }
    }
    let planned = peer
        .delivery
        .borrow_mut()
        .window
        .plan(candidates.iter().map(|(key, bytes)| (*key, bytes.len())));
    let Some((batch, items)) = planned else {
        return Ok(None);
    };
    let body = (|| -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        // The plan may skip in-flight candidates, so select by exact identity.
        for item in &items {
            let (key, bytes) = candidates
                .iter()
                .find(|(key, _)| key == item)
                .ok_or("planned upload item missing")?;
            let chunk = StateItemChunk::new(key.delivery, key.digest, bytes.len() as u64, 0, bytes)
                .map_err(|e| e.to_string())?;
            framing::append_frame(
                &mut body,
                MessageType::StateItemChunk,
                &chunk.encode().map_err(|e| e.to_string())?,
                1048576,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(body)
    })();
    let result = body.map_err(error).and_then(|body| {
        request(
            peer,
            "POST",
            "/peer/v3/state".into(),
            body,
            Pending::StateBatch(batch),
        )
    });
    if result.is_err() {
        peer.delivery.borrow_mut().window.abandon(batch);
    }
    result.map(Some)
}
/// Apply one inline state response: exactly one StateAdmission per request
/// item, in request order. Admitted items are marked submitted (the durable
/// outbox still retires only on signed native coverage); reordered items are
/// re-sent after the window drains.
fn settle_inline_batch(peer: &Rc<Peer>, batch: u64, bytes: &[u8]) -> Result<(), JsValue> {
    use crate::peer_delivery::upload::{ItemKey, ItemOutcome, MAX_BATCH_ITEMS};
    let mut outcomes = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        if outcomes.len() == MAX_BATCH_ITEMS {
            return Err(error("upload admission count exceeded"));
        }
        let (kind, payload, tail) = framing::decode_frame_prefix(rest).map_err(error)?;
        rest = tail;
        if kind != MessageType::StateAdmission {
            return Err(error("unexpected upload response type"));
        }
        let admission = StateAdmission::decode(payload).map_err(error)?;
        let outcome = match admission.outcome() {
            AdmissionOutcome::Accepted {
                scope: CommitScope::NativeCommitted,
                ..
            }
            | AdmissionOutcome::Duplicate {
                scope: CommitScope::NativeCommitted,
                ..
            } => ItemOutcome::Admitted,
            AdmissionOutcome::RetryLater { .. } => ItemOutcome::RetryLater,
            other => ItemOutcome::Refused(format!("{other:?}")),
        };
        outcomes.push((
            ItemKey {
                delivery: admission.delivery(),
                digest: admission.input_digest(),
            },
            outcome,
        ));
    }
    let settled = peer
        .delivery
        .borrow_mut()
        .window
        .settle(batch, outcomes)
        .map_err(error)?;
    peer.published.borrow_mut().extend(settled.admitted);
    Ok(())
}
/// Install the complete groups of one page as ONE staged storage batch: the
/// candidate after the admitted prefix and the cursor of its last group. Only
/// that last cursor is ACKed, after its batch commits.
fn install_cuts(peer: &Rc<Peer>, cuts: Vec<ReplayCut>) -> Result<String, JsValue> {
    check(peer)?;
    let prior = peer.delivery.borrow().cursor.clone();
    admission::page_cursor_steps(prior.as_ref(), &cuts).map_err(error)?;
    // Flush local edits before cloning admission, as for committed desktops.
    crate::browser::prepare_peer_remote(peer.id).map_err(error)?;
    let now = SystemTime::now();
    // LIVE_V1 lane C: a durable cut builds on durable state, never on the
    // view that includes speculative forwarded changes.
    let basis = super::live_api::take_durable_basis(peer);
    let candidate = (|| {
        let current = peer.state.borrow();
        admission::admit_cuts(
            basis.as_ref().unwrap_or(&*current),
            cuts,
            &peer.incoming,
            now,
        )
    })();
    let result = candidate.map_err(error).and_then(|(candidate, cursor, _)| {
        let Some(durable) = basis.as_ref() else {
            return install_admitted(peer, candidate, cursor, None);
        };
        // The cursor journal staged with this cut must describe the records
        // the view already put in the cache: settle outstanding changes into
        // the cut instead of keeping them speculative across it.
        let (state, previous, withdrawn) = super::live_api::settle_durable_cut(peer, candidate);
        match install_admitted(peer, state, cursor, None) {
            Ok(value) => {
                super::live_api::finish_settled_cut(peer, withdrawn);
                Ok(value)
            }
            Err(failure) => {
                super::live_api::restore_settled_cut(peer, durable.clone(), previous);
                Err(failure)
            }
        }
    });
    if result.is_err() {
        if let Some(basis) = basis {
            super::live_api::restore_durable_basis(peer, basis);
        }
    }
    result
}
/// Stage the admitted candidate, its pending journal work and the complete
/// cut cursor as one captured storage batch (the cursor lives inside the one
/// Admission journal value). The cursor is ACKed only after `resume` observes
/// that exact batch's confirmed commit; failure restores the prior journal.
fn install_admitted(
    peer: &Rc<Peer>,
    candidate: StatePeer,
    cursor: StoredDeliveryCursor,
    transfer: Option<&str>,
) -> Result<String, JsValue> {
    let storage =
        admission::advance_cursor(&peer.journal_storage.borrow(), cursor.clone()).map_err(error)?;
    if let Some(id) = transfer {
        peer.batches
            .borrow_mut()
            .bind_transfer(id, cursor.clone(), peer.transport_generation.get())
            .map_err(error)?;
    }
    let previous = peer.journal_storage.replace(storage);
    if let Err(failure) = install_candidate(peer, candidate) {
        peer.journal_storage.replace(previous);
        if let Some(id) = transfer {
            let _ = peer.batches.borrow_mut().cancel_transfer(id);
        }
        return Err(error(failure));
    }
    // A duplicate cut whose exact journal is already committed stages no
    // write; its cursor is durable, so there is no transaction to await.
    let journal = StorageKey::Admission {
        workspace: peer.state.borrow().room().into(),
    };
    if peer
        .cache
        .status_meta(&journal)
        .is_some_and(|status| status.durable)
        && committed_cursor(peer).map_err(error)?.as_ref() == Some(&cursor)
    {
        let mut delivery = peer.delivery.borrow_mut();
        delivery.cursor = Some(cursor);
        if let Some(id) = transfer {
            peer.batches
                .borrow_mut()
                .cancel_transfer(id)
                .map_err(error)?;
            return transfers::discard_committed(&mut delivery);
        }
        return Ok("{\"kind\":\"idle\"}".into());
    }
    peer.delivery.borrow_mut().pending_cursor = Some(cursor);
    Ok("{\"kind\":\"persist\"}".into())
}
