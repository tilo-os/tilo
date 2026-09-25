//! LIVE_V1 WASM calls over the existing Peer key, shared HTTPS custody and
//! checked admission. JavaScript ferries bytes; it never supplies a digest to
//! sign, a grant binding, or state to install.
use super::*;
use crate::peer_delivery::admission::CutAdmission;
use crate::peer_delivery_auth::{OwnerView, fail, time};
use crate::peer_live::{self, GrantBinding, LiveCustody, VerifiedGrant};
use tilo_relay::delivery_v3::live_wire::{self, LiveDirections};

thread_local! {
    static LIVE: RefCell<BTreeMap<u32, LiveCustody>> = RefCell::new(BTreeMap::new());
}

fn owner(id: u32) -> Result<Rc<Peer>, JsValue> {
    let peer = PEERS.with(|p| p.borrow().get(&id).cloned());
    let Some(peer) = peer.filter(|peer| !peer.closed.get()) else {
        LIVE.with(|live| live.borrow_mut().remove(&id));
        AUTHOR_LISTENERS.with(|all| all.borrow_mut().remove(&id));
        return Err(error("closed Peer"));
    };
    Ok(peer)
}
fn authority(
    peer: &Peer,
) -> Result<Rc<RefCell<crate::peer_delivery_auth::DeliveryAuthority>>, JsValue> {
    peer.delivery
        .borrow()
        .authority
        .clone()
        .ok_or_else(|| error("shared HTTPS custody missing"))
}
fn with_custody<T>(id: u32, f: impl FnOnce(&mut LiveCustody) -> T) -> T {
    LIVE.with(|live| f(live.borrow_mut().entry(id).or_default()))
}

/// Directions the current room role permits (viewers: subscribe only).
fn room_directions(view: &OwnerView<'_>, asked: LiveDirections) -> Result<LiveDirections, String> {
    use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
    let device = view.device.device_public_key;
    let (role, caps, person) = view.peer.permissions(device, time(view.now_ns))?;
    let room = view.peer.room().to_owned();
    let actor = Actor::person(Some(role), &person).in_room(room.clone());
    let allowed = |verb| {
        decide_capability(
            &actor,
            &ResourceRef::Room(room.clone()),
            verb,
            &caps,
            Some(view.now_ns / 1_000_000_000),
        ) == CapDecision::Allow
    };
    let directions = LiveDirections {
        publish: asked.publish && allowed(Verb::Write),
        subscribe: asked.subscribe && allowed(Verb::Read),
    };
    if !directions.publish && !directions.subscribe {
        return Err("live room direction denied".into());
    }
    Ok(directions)
}

fn grant_json(value: &VerifiedGrant) -> String {
    let m = &value.grant.metadata;
    serde_json::json!({
        "kind": "grant",
        "authorization": value.bytes,
        "endpoint": value.endpoint,
        "namespace": m.namespace.components(),
        "directions": {"publish": m.request.directions.publish, "subscribe": m.request.directions.subscribe},
        "expiresInMs": value.expires_in_ns as f64 / 1_000_000.0,
        "renewInMs": value.renew_in_ns as f64 / 1_000_000.0,
        "membershipGeneration": m.membership_generation.to_string(),
    })
    .to_string()
}

/// Whether the verified descriptor advertises the installed live relay and
/// the shared HTTPS session is ready to prove requests.
#[wasm_bindgen]
pub fn peer_live_supported(id: u32) -> bool {
    let Ok(peer) = owner(id) else { return false };
    let Ok(authority) = authority(&peer) else {
        return false;
    };
    media_api::with_view(&peer, |view| {
        let authority = authority.borrow();
        Ok(authority.http_ready()
            && authority.current(view)
            && authority.descriptor.media_capabilities() & live_wire::MEDIA_LIVE_V1 != 0)
    })
    .unwrap_or(false)
}

/// Prepare a proven `POST /peer/v3/live/grant`. `fresh` starts a new relay
/// participation (connect); otherwise the current one is renewed in place.
#[wasm_bindgen]
pub fn peer_live_grant(
    id: u32,
    publish: bool,
    subscribe: bool,
    fresh: bool,
) -> Result<String, JsValue> {
    let peer = owner(id)?;
    let authority = authority(&peer)?;
    media_api::with_view(&peer, |view| {
        let authority = authority.borrow();
        if !authority.http_ready()
            || authority.descriptor.media_capabilities() & live_wire::MEDIA_LIVE_V1 == 0
        {
            return Err("live relay unavailable".into());
        }
        let directions = room_directions(view, LiveDirections { publish, subscribe })?;
        let origin = authority.descriptor.origin().to_owned();
        let (request, binding) = with_custody(id, |custody| {
            custody.begin(
                |participation| {
                    Ok(GrantBinding {
                        origin,
                        root: view.device.user_public_key,
                        host: view.host.device_public_key,
                        request: peer_live::grant_request(
                            view.device.device_public_key,
                            view.peer.room(),
                            participation,
                            directions,
                        )?,
                    })
                },
                fresh,
                || tilo_relay::delivery_v3::media_wire::fresh_nonce().map_err(fail),
            )
        })?;
        let prepared = (|| {
            let body = binding.request.encode().map_err(fail)?;
            let proof = authority.prove(view, "POST", live_wire::LIVE_GRANT_TARGET, &body)?;
            Ok::<_, String>(serde_json::json!({
                "kind": "request", "id": request, "origin": binding.origin,
                "target": live_wire::LIVE_GRANT_TARGET, "body": body, "proof": proof,
            }))
        })();
        match prepared {
            Ok(value) => Ok(value.to_string()),
            Err(failure) => {
                with_custody(id, |custody| custody.forget(request));
                Err(failure)
            }
        }
    })
}

/// Verify the host's `SignedLiveGrant` response for one prepared request.
#[wasm_bindgen]
pub fn peer_live_grant_complete(id: u32, request: u32, bytes: Vec<u8>) -> Result<String, JsValue> {
    owner(id)?;
    if bytes.len() > live_wire::MAX_GRANT_BYTES {
        with_custody(id, |custody| custody.forget(request));
        return Err(error("oversized live grant"));
    }
    let now = tilo_relay::clock::now_ns();
    with_custody(id, |custody| custody.complete(request, &bytes, now))
        .map(|value| grant_json(&value))
        .map_err(error)
}

#[wasm_bindgen]
pub fn peer_live_forget(id: u32, request: u32) {
    with_custody(id, |custody| custody.forget(request));
}

/// Drop live authority (revocation, expiry, disposal). A closed Peer's
/// custody is removed entirely by `owner`.
#[wasm_bindgen]
pub fn peer_live_reset(id: u32) {
    if owner(id).is_ok() {
        // Keep the de-duplication window; drop participation and grant.
        LIVE.with(|live| live.borrow_mut().get_mut(&id).map(LiveCustody::revoke));
    }
}

/// Checked admission of one forwarded canonical item on top of `state`: the
/// same bounded native-cut path as durable replay.
fn admit_forwarded(
    peer: &Peer,
    state: &StatePeer,
    canonical: &[u8],
    now: SystemTime,
) -> Result<StatePeer, String> {
    let mut admitted = CutAdmission::begin(state, peer.incoming.clone(), now)?;
    admitted.push_retained(canonical, now)?;
    admitted.finish_complete(now)
}

/// Install an in-memory view (durable basis plus outstanding speculative
/// changes): records for rendering, the Peer state and mounts. The admission
/// journal, cursor and outbox are not staged: they stay durable-only.
fn install_view(peer: &Peer, view: StatePeer) -> Result<(), String> {
    let next = peer
        .state_revision
        .get()
        .checked_add(1)
        .ok_or("canonical Peer revision exhausted")?;
    if view.incomplete_reason().is_none() {
        install_records(peer, &view)?;
    }
    *peer.state.borrow_mut() = view;
    peer.state_revision.set(next);
    crate::browser::refresh_peer_mounts(peer.id);
    crate::browser::expedite_peer_semantics(peer.id);
    report_light(peer);
    Ok(())
}

/// Replace each withdrawn change's record projection with the document of the
/// installed state (durable plus still-outstanding changes). A merge cannot
/// remove CRDT changes; replacing the stored record bytes does. A record whose
/// owner is mounted keeps its in-memory document until it is reopened.
fn withdraw_records(peer: &Peer, withdrawn: &[crate::peer_live::speculation::Speculative]) {
    let state = peer.state.borrow();
    for item in withdrawn {
        let Some((app, record)) = item.route() else {
            continue;
        };
        let key = storage_key(state.room(), &app, &record);
        let (Some(bytes), Some(current)) = (state.document(&app, &record), peer.cache.get(&key))
        else {
            continue;
        };
        if let Err(failure) = peer.cache.put(key, bytes, current.actor) {
            web_sys::console::debug_1(&JsValue::from_str(&format!(
                "live withdrawal record restore: {failure}"
            )));
        }
    }
}

fn apply_rebased(
    peer: &Peer,
    rebased: crate::peer_live::speculation::Rebased,
) -> Result<(), String> {
    if let Some(state) = rebased.state {
        install_view(peer, state)?;
    }
    if !rebased.withdrawn.is_empty() {
        withdraw_records(peer, &rebased.withdrawn);
        report(
            peer,
            "admitting",
            Some(format!(
                "{} forwarded live change(s) were not confirmed by the host and were withdrawn",
                rebased.withdrawn.len()
            )),
        );
    }
    Ok(())
}

/// Durable basis for a durable cut (lane C): the cut builds on it instead of
/// the speculative view. `None` when nothing is outstanding.
pub(super) fn take_durable_basis(peer: &Peer) -> Option<StatePeer> {
    LIVE.with(|live| {
        live.borrow_mut()
            .get_mut(&peer.id)
            .and_then(|c| c.speculation.take_basis())
    })
}
/// A durable cut built on the basis is persisted together with its delivery
/// cursor, and the record cache already holds the view: settle every
/// outstanding forwarded change into the cut (persisted with it, or
/// withdrawn) instead of keeping it speculative across the cut, so the cursor
/// journal describes exactly the persisted records (`Speculation::settle`).
/// Returns the state to install, the prior overlay (for
/// `restore_settled_cut`) and the withdrawn changes (for
/// `finish_settled_cut`).
pub(super) fn settle_durable_cut(
    peer: &Peer,
    candidate: StatePeer,
) -> (
    StatePeer,
    Vec<crate::peer_live::speculation::Speculative>,
    Vec<crate::peer_live::speculation::Speculative>,
) {
    let now_ns = tilo_relay::clock::now_ns();
    let now = SystemTime::now();
    let (previous, settled) = with_custody(peer.id, |custody| {
        let previous = custody.speculation.outstanding().to_vec();
        let settled = custody
            .speculation
            .settle(candidate, now_ns, |state, bytes| {
                admit_forwarded(peer, state, bytes, now)
            });
        (previous, settled)
    });
    (settled.state, previous, settled.withdrawn)
}
/// The settled cut failed to install: the view is unchanged, restore the overlay.
pub(super) fn restore_settled_cut(
    peer: &Peer,
    basis: StatePeer,
    previous: Vec<crate::peer_live::speculation::Speculative>,
) {
    with_custody(peer.id, |custody| {
        custody.speculation.restore(basis, previous)
    });
}
/// The settled cut installed: restore withdrawn changes' record bytes before
/// the storage batch is captured.
pub(super) fn finish_settled_cut(
    peer: &Peer,
    withdrawn: Vec<crate::peer_live::speculation::Speculative>,
) {
    if withdrawn.is_empty() {
        return;
    }
    withdraw_records(peer, &withdrawn);
    report(
        peer,
        "admitting",
        Some(format!(
            "{} forwarded live change(s) were not confirmed by the host and were withdrawn",
            withdrawn.len()
        )),
    );
}
/// The cut failed before installation: the view and basis are unchanged.
pub(super) fn restore_durable_basis(peer: &Peer, basis: StatePeer) {
    with_custody(peer.id, |custody| custody.speculation.restore_basis(basis));
}

/// Called after every durable install (`install_candidate`). A durable cut
/// took the basis first and built on it: outstanding changes are rebased
/// (promoted when covered, re-applied, or withdrawn). Any other durable path
/// built on the view, so its persisted state already contains the changes:
/// they are promoted.
pub(super) fn after_durable_install(peer: &Peer) {
    let now_ns = tilo_relay::clock::now_ns();
    let rebased = LIVE.with(|live| {
        let mut live = live.borrow_mut();
        let custody = live.get_mut(&peer.id)?;
        if custody.speculation.outstanding().is_empty() {
            return None;
        }
        if custody.speculation.is_active() {
            custody.speculation.promote_all();
            return None;
        }
        let durable = peer.state.borrow();
        let now = SystemTime::now();
        Some(
            custody
                .speculation
                .rebase(&durable, now_ns, |state, bytes| {
                    admit_forwarded(peer, state, bytes, now)
                }),
        )
    });
    if let Some(rebased) = rebased {
        if let Err(failure) = apply_rebased(peer, rebased) {
            mark_failure(peer.id, failure);
        }
    }
}

/// A local publication builds on the view: outstanding changes become causal
/// dependencies of durable local state and are kept (promoted). A delivery
/// bootstrap journal is staged from the view for the same reason: the cached
/// records it must match already hold the view.
pub(super) fn promote_for_local_publication(peer: &Peer) {
    LIVE.with(|live| {
        if let Some(custody) = live.borrow_mut().get_mut(&peer.id) {
            custody.speculation.promote_all();
        }
    });
}

thread_local! {
    /// Per-Peer JavaScript listener for this Peer's own publications
    /// (`peer_live_author_listen`): `(frame: Uint8Array, app, record)`.
    static AUTHOR_LISTENERS: RefCell<BTreeMap<u32, js_sys::Function>> = RefCell::new(BTreeMap::new());
}

/// Register (or with `null` remove) the live client's listener for this
/// Peer's own signed document publications. The listener receives each
/// publication's `OpsFrame::SignedChange` bytes synchronously, right after
/// the change entered the outbox and before its IndexedDB commit and upload;
/// it only queues the frame (ordered behind the previews already queued for
/// that record) and must not call back into the Peer.
#[wasm_bindgen]
pub fn peer_live_author_listen(id: u32, listener: Option<js_sys::Function>) -> Result<(), JsValue> {
    owner(id)?;
    AUTHOR_LISTENERS.with(|all| {
        let mut all = all.borrow_mut();
        match listener {
            Some(listener) => all.insert(id, listener),
            None => all.remove(&id),
        }
    });
    Ok(())
}

/// Canonical bytes of a local publication, when a live listener wants them
/// and the Peer holds a current publishing grant (otherwise `None`: the
/// change reaches members through the host forward and durable replay).
pub(super) fn author_canonical(
    peer: &Peer,
    message: &qos_peer_state::NeighbourMsg,
) -> Option<Vec<u8>> {
    if !matches!(message, qos_peer_state::NeighbourMsg::AppDocument { .. })
        || !AUTHOR_LISTENERS.with(|all| all.borrow().contains_key(&peer.id))
    {
        return None;
    }
    let now_ns = tilo_relay::clock::now_ns();
    let publishing = LIVE.with(|live| {
        live.borrow()
            .get(&peer.id)
            .and_then(|custody| custody.current(now_ns))
            .is_some_and(|grant| grant.metadata.request.directions.publish)
    });
    if !publishing {
        return None;
    }
    postcard::to_allocvec(message).ok()
}

/// Hand a local publication (from [`author_canonical`]) to the live client.
/// Called after the Peer installed the new state and outbox, with no borrow
/// held; failures only lose the early forward.
pub(super) fn author_published(peer: &Peer, canonical: Option<Vec<u8>>) {
    let Some(canonical) = canonical else { return };
    let Some((_, record, frame)) = peer_live::author_signed_change(&canonical) else {
        return;
    };
    let Some(listener) = AUTHOR_LISTENERS.with(|all| all.borrow().get(&peer.id).cloned()) else {
        return;
    };
    let bytes = js_sys::Uint8Array::from(frame.as_slice());
    let _ = listener.call3(
        &JsValue::NULL,
        &bytes,
        &JsValue::from_str(&record.app),
        &JsValue::from_str(&record.record),
    );
}

/// Admit a forwarded `OpsFrame::SignedChange` through the same checked
/// native publication path as replay, as a speculative change: it is shown
/// (Peer state, records, mounts) but never enters a durable cut, the
/// admission journal or the cursor until durable state covers it; unconfirmed
/// changes are withdrawn at their deadline (`peer_live_reconcile`).
/// Returns `{"kind": "admitted"|"duplicate"|"deferred", "digest": hex,
/// "speculative": bool, "confirmByMs": wall ms}`.
#[wasm_bindgen]
pub fn peer_live_admit(id: u32, frame: Vec<u8>) -> Result<String, JsValue> {
    let peer = owner(id)?;
    let (digest, _record, canonical) = peer_live::signed_change(&frame).map_err(error)?;
    let result = |kind: &str| {
        Ok(serde_json::json!({"kind": kind, "digest": peer_live::hex(&digest), "speculative": false}).to_string())
    };
    let now_ns = tilo_relay::clock::now_ns();
    let live_authorized = with_custody(id, |custody| custody.current(now_ns).is_some());
    if !live_authorized {
        return Err(error("live authorization absent"));
    }
    if with_custody(id, |custody| {
        custody.admitted.contains(&digest) || custody.speculation.contains(&digest)
    }) || peer.published.borrow().contains(&digest)
    {
        return result("duplicate");
    }
    if !peer.delivery.borrow().live_admission_quiet() {
        return result("deferred");
    }
    let held = qos_peer_state::resources::BoundedPeerMessage::decode(
        &canonical,
        qos_peer_state::resources::OuterLimits {
            allocation_bytes: peer
                .incoming
                .available()
                .min(live_wire::MAX_STREAM_FRAME_BYTES * 64),
            collection_entries: 65536,
        },
        peer.incoming.allowance(),
    )
    .map_err(|e| error(format!("bounded live change decode: {e:?}")))?;
    if peer.state.borrow().covers(held.message()) {
        with_custody(id, |custody| custody.admitted.insert(digest));
        return result("duplicate");
    }
    drop(held);
    crate::browser::prepare_peer_remote(peer.id).map_err(error)?;
    let now = SystemTime::now();
    let item = crate::peer_live::speculation::Speculative {
        digest,
        canonical,
        admitted_at_ns: now_ns,
    };
    let admitted = {
        let view = peer.state.borrow();
        with_custody(id, |custody| {
            custody.speculation.admit(&view, item, |state, bytes| {
                admit_forwarded(&peer, state, bytes, now)
            })
        })
    };
    // Not admissible yet (missing prerequisite, foreign family, capacity): the
    // durable path remains authoritative and will deliver it in order.
    let Ok(view) = admitted else {
        return result("deferred");
    };
    if let Err(failure) = install_view(&peer, view) {
        with_custody(id, |custody| custody.speculation.forget(&digest));
        return Err(error(failure));
    }
    with_custody(id, |custody| custody.admitted.insert(digest));
    let confirm_by_ms =
        (now_ns + crate::peer_live::speculation::CONFIRM_DEADLINE_NS) as f64 / 1_000_000.0;
    Ok(serde_json::json!({"kind": "admitted", "digest": peer_live::hex(&digest), "speculative": true, "confirmByMs": confirm_by_ms}).to_string())
}

/// Withdraw forwarded changes that durable state has not confirmed by their
/// deadline. Returns `{"outstanding": n, "withdrawn": n, "nextDeadlineMs": wall ms|null}`.
#[wasm_bindgen]
pub fn peer_live_reconcile(id: u32) -> Result<String, JsValue> {
    let peer = owner(id)?;
    let now_ns = tilo_relay::clock::now_ns();
    let now = SystemTime::now();
    let expired = with_custody(id, |custody| {
        custody.speculation.expire(now_ns, |state, bytes| {
            admit_forwarded(&peer, state, bytes, now)
        })
    });
    let withdrawn = expired
        .as_ref()
        .map_or(0, |rebased| rebased.withdrawn.len());
    if let Some(rebased) = expired {
        apply_rebased(&peer, rebased).map_err(error)?;
    }
    let (outstanding, next) = with_custody(id, |custody| {
        (
            custody.speculation.outstanding().len(),
            custody.speculation.next_deadline_ns(),
        )
    });
    Ok(serde_json::json!({
        "outstanding": outstanding,
        "withdrawn": withdrawn,
        "nextDeadlineMs": next.map(|ns| ns as f64 / 1_000_000.0),
    })
    .to_string())
}

// ---- Inert codecs (no Peer) ----------------------------------------------

#[wasm_bindgen]
pub fn live_encode_presence(json: &str) -> Result<Vec<u8>, JsValue> {
    peer_live::encode_presence(json).map_err(error)
}
#[wasm_bindgen]
pub fn live_decode_presence(bytes: &[u8]) -> Result<String, JsValue> {
    peer_live::decode_presence(bytes).map_err(error)
}
#[wasm_bindgen]
pub fn live_encode_preview(json: &str) -> Result<Vec<u8>, JsValue> {
    peer_live::encode_preview(json).map_err(error)
}
#[wasm_bindgen]
pub fn live_decode_ops(bytes: &[u8]) -> Result<String, JsValue> {
    peer_live::decode_ops(bytes).map_err(error)
}
#[wasm_bindgen]
pub fn live_ops_track(app: &str, record: &str) -> Result<String, JsValue> {
    peer_live::ops_track(app, record).map_err(error)
}
