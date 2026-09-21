//! Browser transport uses the same enrolled relay client and protocol as native.
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use serde::Deserialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use tilo_relay::{
    RelayClient,
    coedit::{Inbound, RelayCoeditSession},
};
use tilo_user_identity::{
    DeviceCert, DirectoryUpdate, RevocationEntry, RevocationList, WorkspaceDirectory,
};
use ui::portable_storage::{PendingWrite, PortableStorage, StorageKey};
use wasm_bindgen::prelude::*;
use web_time::SystemTime;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    version: u32,
    certificate: DeviceCert,
    root_public_key: [u8; 32],
    host_certificate: DeviceCert,
    relay_address: iroh_base::EndpointAddr,
    packet_relay: iroh_base::RelayUrl,
    revocations: Vec<RevocationEntry>,
    bootstrap: Bootstrap,
}
#[derive(Deserialize)]
struct Bootstrap {
    room: String,
    workspace_id: String,
    directory_update: DirectoryUpdate,
    directory_author: DeviceCert,
    certified_roster: Vec<DeviceCert>,
}
use crate::peer_journal::Journal;
struct Peer {
    actions: RefCell<crate::action_client::PendingActions>,
    health_projection: RefCell<crate::peer_journal::HealthProjection>,
    action_v2: std::cell::Cell<bool>,
    admission_failure: RefCell<Option<String>>,
    was_ready: std::cell::Cell<bool>,
    published: RefCell<BTreeSet<Vec<u8>>>,
    id: u32,
    cache: PortableStorage,
    outbox: RefCell<Vec<NeighbourMsg>>,
    endpoint: RefCell<Option<Endpoint>>,
    session: RefCell<Option<Rc<RelayCoeditSession>>>,
    transport_generation: std::cell::Cell<u64>,
    transport_connected: std::cell::Cell<bool>,
    admission_progress: RefCell<crate::admission_health::AdmissionProgress>,
    last_relay_contact: std::cell::Cell<Option<f64>>,
    last_peer_contact: std::cell::Cell<Option<f64>>,
    batches: RefCell<BTreeMap<String, (Vec<PendingWrite>, Option<u64>)>>,
    next_batch: std::cell::Cell<u64>,
    storage_epoch: RefCell<Option<String>>,
    committed_at: std::cell::Cell<Option<f64>>,
    state: RefCell<StatePeer>,
    _key: SigningKey,
    callback: js_sys::Function,
    closed: std::cell::Cell<bool>,
    /// Whether the last full report was `ready`; the light report between
    /// commit boundaries then needs no recomputed health (see `report_light`).
    last_report_ready: std::cell::Cell<bool>,
    /// A report whose text the host already holds is not delivered again.
    report_dedup: RefCell<crate::report_text::ReportDedup>,
}
thread_local! {static PEERS:RefCell<BTreeMap<u32,Rc<Peer>>>=const{RefCell::new(BTreeMap::new())};static NEXT:std::cell::Cell<u32>=const{std::cell::Cell::new(1)};}
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn report(peer: &Peer, phase: &str, error: Option<String>) {
    let source = if !peer.transport_connected.get() {
        crate::admission_health::ReportSource::Transport
    } else {
        crate::admission_health::ReportSource::Runtime
    };
    report_from(peer, phase, error, source);
}
fn report_from(
    peer: &Peer,
    phase: &str,
    error: Option<String>,
    source: crate::admission_health::ReportSource,
) {
    let state = peer.state.borrow();
    let complete = crate::desktop::admission(&state).is_ok_and(|admission| admission.complete());
    let outgoing = crate::admission_health::outgoing_projection(
        &state,
        &peer.outbox.borrow(),
        SystemTime::now(),
    );
    let incomplete = required_read_failure(peer, &state)
        .or_else(|| state.incomplete_reason())
        .or_else(|| peer.admission_failure.borrow().clone());
    // One pass over the pending writes for both the phase and the trigger; the
    // health projection below counts them per record.
    let persistence_needed = !peer.cache.pending_writes().is_empty();
    let phase = crate::admission_health::runtime_phase(
        phase,
        complete,
        persistence_needed,
        peer.was_ready.get(),
        incomplete.is_some(),
    );
    if phase == "ready" {
        peer.was_ready.set(true);
    }
    let error = error.or(incomplete);
    let health = health(
        peer,
        &state,
        phase == "ready",
        error.as_deref(),
        source,
        outgoing.blocked_expired,
    );
    let synchronized = health["records"].as_array().is_some_and(|records| {
        !records.is_empty()
            && records
                .iter()
                .all(|record| record["acknowledgement"]["coversCurrentBasis"] == true)
    });
    let value = serde_json::json!({"phase":phase,"desktop":{"version":1,"mode":"session"},"room":state.room(),"deviceId":peer._key.verifying_key().to_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),"records":state.rows(),"pendingPrerequisites":state.pending_count(),"persistenceNeeded":persistence_needed,"outgoingPending":peer.outbox.borrow().len(),"outgoingBlocked":outgoing.blocked_expired,"outgoingExpiryAtMs":outgoing.expiry_at_ms,"synchronization":if outgoing.blocked_expired>0{"blocked"}else if synchronized{"acknowledged"}else{"pending"},"error":error,"health":health});
    let text = value.to_string();
    peer.last_report_ready.set(phase == "ready");
    // Health was recomputed above; only its delivery is skipped when the host
    // already holds this exact text (a forced publication that committed
    // nothing, an inbound frame that changed nothing).
    if peer
        .report_dedup
        .borrow_mut()
        .should_emit(&text, persistence_needed)
    {
        let _ = peer
            .callback
            .call1(&JsValue::NULL, &JsValue::from_str(&text));
    }
}
/// Report between commit boundaries without recomputing health. A typed key
/// otherwise computed the full health report three times (publication,
/// acknowledgement, committed). The light report carries only the retained
/// phase and the persistence trigger; the host merges statuses field by field,
/// so rows, outbox counts and health keep the last full report's values until
/// `peer_committed` or a transport or failure path refreshes them, at most one
/// commit cycle later. Before the peer has ever been ready this is a full
/// report.
fn report_light(peer: &Peer) {
    if !(peer.was_ready.get() && peer.last_report_ready.get()) {
        return report(peer, "admitting", None);
    }
    let persistence_needed = !peer.cache.pending_writes().is_empty();
    let text = crate::report_text::light_report("ready", persistence_needed);
    if peer
        .report_dedup
        .borrow_mut()
        .should_emit(&text, persistence_needed)
    {
        let _ = peer
            .callback
            .call1(&JsValue::NULL, &JsValue::from_str(&text));
    }
}
#[wasm_bindgen]
pub async fn connect_peer(
    raw: String,
    identity: Vec<u8>,
    hydrated: JsValue,
    callback: js_sys::Function,
) -> Result<u32, JsValue> {
    connect_owner(None, raw, identity, hydrated, Some(callback)).await
}
#[wasm_bindgen]
pub async fn reconnect_peer(id: u32, raw: String, identity: Vec<u8>) -> Result<(), JsValue> {
    connect_owner(Some(id), raw, identity, JsValue::NULL, None)
        .await
        .map(|_| ())
}
async fn connect_owner(
    existing: Option<u32>,
    raw: String,
    identity: Vec<u8>,
    hydrated: JsValue,
    callback: Option<js_sys::Function>,
) -> Result<u32, JsValue> {
    let config: Enrollment = serde_json::from_str(&raw).map_err(error)?;
    if config.version != 2 {
        return Err(error("unsupported enrollment version"));
    }
    let key = crate::identity::key(&identity, &raw).map_err(error)?;
    let device = key.verifying_key().to_bytes();
    if device != config.certificate.device_public_key
        || config.certificate.user_public_key != config.root_public_key
        || config.host_certificate.user_public_key != config.root_public_key
    {
        return Err(error("enrollment key/root binding mismatch"));
    }
    let now = tilo_relay::clock::now_ns();
    let mut revoked = RevocationList::new();
    for entry in config.revocations {
        if entry.user_public_key != config.root_public_key {
            return Err(error("revocation root mismatch"));
        }
        revoked.insert(entry).map_err(error)?;
    }
    tilo_user_identity::verify_device_cert_at(&config.certificate, now).map_err(error)?;
    tilo_user_identity::verify_device_cert_at(&config.host_certificate, now).map_err(error)?;
    if revoked.is_revoked(&device) || revoked.is_revoked(&config.host_certificate.device_public_key)
    {
        return Err(error("enrolled device revoked"));
    }
    let workspace = config
        .bootstrap
        .workspace_id
        .parse::<u64>()
        .map_err(error)?;
    if !tilo_relay::room_workspace_id_matches(&config.bootstrap.room, workspace) {
        return Err(error("workspace room binding mismatch"));
    }
    let mut directory = WorkspaceDirectory::new(config.root_public_key, device);
    directory
        .apply_incremental(
            &config.bootstrap.directory_update,
            &config.bootstrap.directory_author,
            &revoked,
            now,
        )
        .map_err(error)?;
    let rows = directory.as_map();
    let row = rows
        .get(&workspace)
        .ok_or_else(|| error("signed directory missing workspace"))?;
    if !row.member_devices.contains(&device)
        || !row
            .host_devices
            .contains(&config.host_certificate.device_public_key)
    {
        return Err(error("signed directory membership/host mismatch"));
    }
    let initial = StatePeer::new(
        &config.bootstrap.room,
        config.bootstrap.directory_author,
        config.bootstrap.certified_roster.clone(),
        revoked.clone(),
        SystemTime::now(),
    )
    .map_err(error)?;
    let profile = format!("certified:{}", config.bootstrap.room);
    let device_hex = device
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let owner = existing
        .map(|id| {
            PEERS
                .with(|peers| peers.borrow().get(&id).cloned())
                .ok_or_else(|| error("closed peer"))
        })
        .transpose()?;
    if let Some(peer) = &owner {
        if peer.state.borrow().room() != initial.room()
            || peer.state.borrow().authority() != initial.authority()
            || peer._key.verifying_key() != key.verifying_key()
        {
            return Err(error("replacement transport owner binding mismatch"));
        }
    }
    let cache = if let Some(peer) = &owner {
        peer.cache.clone()
    } else {
        crate::browser::load_cache(&profile, &device_hex, &hydrated)?
    };
    let (state, outbox) = if let Some(peer) = &owner {
        (peer.state.borrow().clone(), peer.outbox.borrow().clone())
    } else if let Some(saved) = cache.get(&StorageKey::Admission {
        workspace: config.bootstrap.room.clone(),
    }) {
        let journal = Journal::decode(&saved.bytes).map_err(error)?;
        let restored = StatePeer::restore(
            &journal.state,
            &config.bootstrap.room,
            initial.authority().clone(),
            config.bootstrap.certified_roster.clone(),
            revoked.clone(),
            SystemTime::now(),
        )
        .map_err(error)?;
        let outbox = journal.into_unretired_outbox(&restored);
        (restored, outbox)
    } else {
        (initial, vec![])
    };
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Custom(RelayMap::from(config.packet_relay)))
        .bind()
        .await
        .map_err(error)?;
    let connected = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint.clone(),
        config.relay_address.clone(),
        config.root_public_key,
        config.host_certificate.device_public_key,
        config.certificate.clone(),
        key.clone(),
        revoked.clone(),
    )
    .await;
    // Only the typed TLS ALPN refusal permits v1. Auth and policy failures do not.
    let (connected, action_v2) = match connected {
        Err(tilo_relay::client::ClientError::UnsupportedProtocol) => (
            RelayClient::connect_to_enrolled_home_peer(
                endpoint.clone(),
                config.relay_address,
                config.root_public_key,
                config.host_certificate.device_public_key,
                config.certificate.clone(),
                key.clone(),
                revoked,
            )
            .await,
            false,
        ),
        result => (result, true),
    };
    let client = match connected {
        Ok(client) => client,
        Err(failure) => {
            endpoint.close().await;
            return Err(error(failure));
        }
    };
    let session = match RelayCoeditSession::join(client, workspace, device) {
        Ok(session) => Rc::new(session),
        Err(failure) => {
            endpoint.close().await;
            return Err(error(failure));
        }
    };
    let id = existing.unwrap_or_else(|| {
        NEXT.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        })
    });
    let peer = if let Some(peer) = owner {
        peer.action_v2.set(action_v2);
        *peer.actions.borrow_mut() = Default::default();
        peer.transport_generation
            .set(peer.transport_generation.get() + 1);
        let prior = peer.endpoint.borrow_mut().replace(endpoint.clone());
        *peer.session.borrow_mut() = Some(session.clone());
        peer.transport_connected.set(true);
        peer.published.borrow_mut().clear();
        if let Some(prior) = prior {
            prior.close().await;
        }
        peer
    } else {
        Rc::new(Peer {
            health_projection: RefCell::default(),
            actions: RefCell::default(),
            action_v2: std::cell::Cell::new(action_v2),
            admission_failure: RefCell::new(None),
            was_ready: std::cell::Cell::new(false),
            published: RefCell::new(BTreeSet::new()),
            id,
            cache,
            outbox: RefCell::new(outbox),
            endpoint: RefCell::new(Some(endpoint.clone())),
            session: RefCell::new(Some(session.clone())),
            state: RefCell::new(state),
            _key: key,
            callback: callback.ok_or_else(|| error("missing status callback"))?,
            closed: std::cell::Cell::new(false),
            last_report_ready: std::cell::Cell::new(false),
            report_dedup: RefCell::default(),
            transport_generation: std::cell::Cell::new(1),
            transport_connected: std::cell::Cell::new(true),
            admission_progress: RefCell::default(),
            last_relay_contact: std::cell::Cell::new(None),
            last_peer_contact: std::cell::Cell::new(None),
            batches: RefCell::new(BTreeMap::new()),
            next_batch: std::cell::Cell::new(0),
            storage_epoch: RefCell::new(None),
            committed_at: std::cell::Cell::new(None),
        })
    };
    let transport_generation = peer.transport_generation.get();
    let hello = NeighbourMsg::IdentityHello {
        cert: Box::new(config.certificate),
        revocations: vec![],
    };
    let room = config.bootstrap.room;
    let setup = (|| -> Result<(), JsValue> {
        if action_v2 {
            let room = peer.state.borrow().room().parse().map_err(error)?;
            session.enable_actions(room).map_err(error)?;
            session.enable_broker_client(room).map_err(error)?;
        }
        // Rewrite legacy journal framing only through the same atomic cache transaction.
        stage_journal(&peer, &peer.state.borrow(), &peer.outbox.borrow()).map_err(error)?;
        session
            .publish_presence(postcard::to_allocvec(&hello).map_err(error)?)
            .map_err(error)?;
        Ok(())
    })();
    if let Err(error) = setup {
        peer.transport_connected.set(false);
        endpoint.close().await;
        return Err(error);
    }
    PEERS.with(|peers| peers.borrow_mut().insert(id, peer.clone()));
    report(&peer, "admitting", None);
    if action_v2 {
        let peer = peer.clone();
        let session = session.clone();
        wasm_bindgen_futures::spawn_local(async move {
            while let Some(event) = session.recv_action().await {
                if peer.closed.get() || peer.transport_generation.get() != transport_generation {
                    return;
                }
                // Delivered is attested by the same mutually authenticated relay.
                // Current membership/certificate validity is rechecked for every response.
                if action_context(&peer).is_ok() {
                    if matches!(&event, tilo_relay::transport_v2::EnvelopeV2::Delivered { action, .. }
                        if matches!(action.message, tilo_relay::peer_actions::ActionMessage::CommittedDesktop { .. }))
                    {
                        // Flush local edits before cloning admission, just as on
                        // the retained channel. A complete batch installs once.
                        if let Err(error) = crate::browser::prepare_peer_remote(peer.id) {
                            mark_failure(peer.id, error);
                            continue;
                        }
                        let admission = crate::committed_desktop::admit(
                            &peer.state.borrow(),
                            &event,
                            peer._key.verifying_key().to_bytes(),
                            SystemTime::now(),
                        );
                        let emit_notice =
                            |notice: &crate::committed_desktop::CommittedDesktopNotice| {
                                let value = serde_json::json!({"committedDesktop":{"invocation":notice.invocation,"room":notice.room,"opened":notice.opened}}).to_string();
                                let _ = peer
                                    .callback
                                    .call1(&JsValue::NULL, &JsValue::from_str(&value));
                            };
                        match admission {
                            Ok(crate::committed_desktop::Admission::Ready {
                                candidate,
                                notice,
                            }) => {
                                peer.last_peer_contact.set(Some(js_sys::Date::now()));
                                if let Err(e) = install_candidate(&peer, candidate) {
                                    *peer.admission_failure.borrow_mut() = Some(e.clone());
                                    report(&peer, "incomplete", Some(e));
                                } else if let Some(notice) = notice {
                                    emit_notice(&notice);
                                }
                            }
                            Ok(crate::committed_desktop::Admission::AwaitingRetainedState(
                                notice,
                            )) => {
                                peer.last_peer_contact.set(Some(js_sys::Date::now()));
                                emit_notice(&notice);
                            }
                            Ok(crate::committed_desktop::Admission::Ignored) => (),
                            Err(error) => web_sys::console::debug_1(&JsValue::from_str(&format!(
                                "Committed desktop direct admission rejected: {error}"
                            ))),
                        }
                        continue;
                    }
                    let root = peer.state.borrow().authority().user_public_key;
                    peer.actions.borrow_mut().receive(event, root);
                    // Wake command waiters as soon as the authenticated response
                    // is installed. The JS host keeps only a deadline timer.
                    let _ = peer.callback.call1(
                        &JsValue::NULL,
                        &JsValue::from_str("{\"actionResponseReady\":true}"),
                    );
                }
            }
        });
    }
    wasm_bindgen_futures::spawn_local(async move {
        let mut replaying = true;
        let mut post_ready_repair = crate::post_ready_repair::PostReadyRepair::default();
        while let Some(inbound) = session.recv().await {
            if peer.closed.get() || peer.transport_generation.get() != transport_generation {
                return;
            }
            let (author, data, authenticated) = match inbound {
                Inbound::Delta { author, data } => (author, data, true),
                Inbound::Presence { author, payload } => (author, payload, false),
                Inbound::Ready { .. } => {
                    replaying = false;
                    peer.last_relay_contact.set(Some(js_sys::Date::now()));
                    let request_repair = post_ready_repair.on_ready(
                        peer.transport_generation.get() == transport_generation,
                        &peer.state.borrow(),
                        peer._key.verifying_key().to_bytes(),
                        SystemTime::now(),
                    );
                    if request_repair {
                        let request = NeighbourMsg::SyncReq {
                            since: vec![],
                            room: room.clone(),
                            invite_id: None,
                        };
                        if let Err(failure) = postcard::to_allocvec(&request)
                            .map_err(|failure| failure.to_string())
                            .and_then(|request| {
                                session
                                    .publish_presence(request)
                                    .map_err(|failure| failure.to_string())
                            })
                        {
                            peer.transport_connected.set(false);
                            report(
                                &peer,
                                "failed",
                                Some(format!("checkpoint repair request failed: {failure}")),
                            );
                            return;
                        }
                    }
                    report(&peer, "admitting", None);
                    continue;
                }
            };
            let message = match postcard::from_bytes::<NeighbourMsg>(&data) {
                Ok(message) => message,
                Err(e) => {
                    report(&peer, "incomplete", Some(format!("protocol decode: {e}")));
                    continue;
                }
            };
            if !authenticated
                && matches!(
                    message,
                    NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
                )
            {
                report(
                    &peer,
                    "incomplete",
                    Some("state message requires relay-authenticated Delta carrier".into()),
                );
                continue;
            }
            if authenticated
                && matches!(
                    message,
                    NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
                )
            {
                if let Err(error) = crate::browser::prepare_peer_remote(peer.id) {
                    mark_failure(peer.id, error);
                    continue;
                }
            }
            let mut candidate = peer.state.borrow().clone();
            let pending_before = candidate.pending_count();
            let result = candidate.receive_from(
                if authenticated {
                    if replaying {
                        TransportOrigin::RetainedRelayDelta(author)
                    } else {
                        TransportOrigin::RelayDelta(author)
                    }
                } else {
                    TransportOrigin::UntrustedPresence(author)
                },
                message,
                SystemTime::now(),
            );
            match result {
                Ok(outcome) => {
                    if authenticated && author == peer.state.borrow().authority().device_public_key
                    {
                        peer.last_peer_contact.set(Some(js_sys::Date::now()));
                    }
                    if outcome.changed || outcome.metadata_changed {
                        if let Err(e) = install_candidate(&peer, candidate) {
                            *peer.admission_failure.borrow_mut() = Some(e.clone());
                            report(&peer, "incomplete", Some(e));
                        }
                    } else {
                        if candidate.pending_count() < pending_before {
                            peer.admission_progress.borrow_mut().accepted(None);
                        }
                        *peer.state.borrow_mut() = candidate;
                        report(
                            &peer,
                            if outcome.deferred {
                                "admitting"
                            } else {
                                "admitting"
                            },
                            None,
                        );
                    }
                }
                // A rejected packet did not change the transactional StatePeer.
                // Keep awaiting a fresh checkpoint (or keep an already durable
                // peer ready), while exposing the rejection. Authenticated missing
                // crypto and failed record installation remain sticky incomplete.
                Err(e) => {
                    let source = crate::admission_health::receive_source(&e);
                    report_from(&peer, "admitting", Some(e), source);
                }
            }
        }
        if !peer.closed.get() && peer.transport_generation.get() == transport_generation {
            peer.transport_connected.set(false);
            report(
                &peer,
                "failed",
                Some("certified relay stream disconnected".into()),
            );
        }
    });
    Ok(id)
}
/// Request bootstrap on the retained authenticated transport without replacing
/// the owner or requiring the very document permissions bootstrap may supply.
#[wasm_bindgen]
pub fn peer_request_sync(id: u32, expected_transport_generation: String) -> Result<(), JsValue> {
    let expected = expected_transport_generation
        .parse::<u64>()
        .map_err(error)?;
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if peer.closed.get()
            || !peer.transport_connected.get()
            || peer.transport_generation.get() != expected
        {
            return Err(error("sync transport generation is unavailable"));
        }
        let session = peer
            .session
            .borrow()
            .clone()
            .ok_or_else(|| error("sync transport disconnected"))?;
        let request = NeighbourMsg::SyncReq {
            since: vec![],
            room: peer.state.borrow().room().into(),
            invite_id: None,
        };
        session
            .publish_presence(postcard::to_allocvec(&request).map_err(error)?)
            .map_err(error)
    })
}
/// Disconnect transport without destroying durable cache/outbox ownership.
#[wasm_bindgen]
pub async fn disconnect_peer(id: u32) {
    let peer = PEERS.with(|peers| peers.borrow().get(&id).cloned());
    if let Some(peer) = peer {
        peer.transport_generation
            .set(peer.transport_generation.get() + 1);
        peer.transport_connected.set(false);
        peer.session.borrow_mut().take();
        let endpoint = peer.endpoint.borrow_mut().take();
        if let Some(endpoint) = endpoint {
            endpoint.close().await;
        }
    }
}
#[wasm_bindgen]
pub async fn close_peer(id: u32) {
    let peer = PEERS.with(|peers| peers.borrow_mut().remove(&id));
    if let Some(peer) = peer {
        peer.closed.set(true);
        peer.transport_generation
            .set(peer.transport_generation.get() + 1);
        let endpoint = peer.endpoint.borrow_mut().take();
        if let Some(endpoint) = endpoint {
            endpoint.close().await;
        }
        report(&peer, "closed", None);
    }
}

#[wasm_bindgen]
pub fn create_identity() -> Vec<u8> {
    crate::identity::create()
}
#[wasm_bindgen]
pub fn identity_request(material: &[u8]) -> Result<String, JsValue> {
    crate::identity::request(material).map_err(error)
}
#[wasm_bindgen]
pub fn identity_complete(material: &[u8], challenge: &str) -> Result<String, JsValue> {
    crate::identity::complete(material, challenge).map_err(error)
}
#[wasm_bindgen]
pub fn bind_identity(material: &[u8], challenge: &str, raw: &str) -> Result<Vec<u8>, JsValue> {
    crate::identity::bind(material, challenge, raw).map_err(error)
}
#[wasm_bindgen]
pub fn peer_enrollment(raw: &str) -> Result<String, JsValue> {
    let config: Enrollment = serde_json::from_str(raw).map_err(error)?;
    let device = config
        .certificate
        .device_public_key
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(serde_json::json!({"profile":format!("certified:{}",config.bootstrap.room),"device":device,"room":config.bootstrap.room}).to_string())
}
fn storage_key(room: &str, app: &str, record: &str) -> StorageKey {
    if record.is_empty() {
        StorageKey::Primary {
            package: app.into(),
            workspace: room.into(),
        }
    } else {
        StorageKey::Record {
            package: app.into(),
            workspace: room.into(),
            record: record.into(),
        }
    }
}
fn install_records(peer: &Peer, state: &StatePeer) -> Result<(), String> {
    crate::admitted_records::install(&peer.cache, state, peer._key.verifying_key().to_bytes())
}

fn stage_journal(peer: &Peer, state: &StatePeer, outbox: &[NeighbourMsg]) -> Result<(), String> {
    let bytes = Journal::from_state(state, outbox.to_vec())
        .and_then(|journal| journal.encode())
        .map_err(|e| e.to_string())?;
    peer.cache.put(
        StorageKey::Admission {
            workspace: state.room().into(),
        },
        bytes,
        None,
    )
}
/// The canonical record owner has already committed this document. Avoid a
/// full save/load round trip merely to recover its existing CRDT graph.
pub(crate) fn local_owned_document(
    id: u32,
    app: &str,
    record: &str,
    mut current: automerge::AutoCommit,
) -> Result<(), String> {
    if current.pending_ops() != 0 {
        return Err("record mutation is still unfinished".into());
    }
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or("certified peer is closed")?;
        let before = peer
            .state
            .borrow()
            .document_heads(app, record)
            .ok_or("canonical native record not admitted")?;
        if current.base_heads() == before {
            return Ok(());
        }
        let delta = current.save_after(&before);
        let bytes = current.save();
        let mut candidate = peer.state.borrow().clone();
        let message =
            candidate.publish_incremental(app, record, delta, &peer._key, SystemTime::now())?;
        let mut outbox = peer.outbox.borrow().clone();
        outbox.push(message);
        let key = storage_key(candidate.room(), app, record);
        let actor = peer
            .cache
            .get(&key)
            .ok_or("record actor custody missing")?
            .actor;
        peer.cache.put(key, bytes, actor)?;
        stage_journal(&peer, &candidate, &outbox)?;
        *peer.state.borrow_mut() = candidate;
        *peer.outbox.borrow_mut() = outbox;
        report_light(&peer);
        Ok(())
    })
}
#[wasm_bindgen]
pub fn peer_pending_writes(id: u32) -> Result<String, JsValue> {
    PEERS.with(|peers| {
        let peers = peers.borrow();
        let peer = peers.get(&id).ok_or_else(|| error("closed peer"))?;
        serde_json::to_string(
            &peer
                .cache
                .pending_writes()
                .iter()
                .map(crate::browser::write_json)
                .collect::<Vec<_>>(),
        )
        .map_err(error)
    })
}
/// Recompute the read-only projection when a signed outbox envelope reaches
/// its freshness deadline. This never publishes or changes durable custody.
#[wasm_bindgen]
pub fn peer_refresh_status(id: u32) -> Result<(), JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if !peer.closed.get() {
            report(&peer, "admitting", None);
        }
        Ok(())
    })
}
#[wasm_bindgen]
pub fn peer_committed(id: u32) -> Result<(), JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if !peer.cache.pending_writes().is_empty() {
            return Err(error("admission/record transaction not committed"));
        }
        if peer.closed.get() {
            return Ok(());
        }
        // Iterate the retained outbox in place: it holds every unsent and
        // uncovered document message, and cloning it per commit grew with it.
        let outbox = peer.outbox.borrow();
        for message in qos_peer_state::outbox::publication_batch(&outbox, SystemTime::now()) {
            let Some((channel, qos_peer_state::transport::Retention::Delta)) =
                qos_peer_state::transport::retention(message, false)
            else {
                return Err(error("outbox must contain incremental document state"));
            };
            let bytes = postcard::to_allocvec(message).map_err(error)?;
            if !peer.published.borrow().contains(&bytes) {
                peer.session
                    .borrow()
                    .as_ref()
                    .ok_or_else(|| error("transport unavailable"))?
                    .publish_retained_delta(channel, bytes.clone())
                    .map_err(error)?;
                peer.published.borrow_mut().insert(bytes);
            }
        }
        drop(outbox);
        // Keep durable outgoing bytes until a validated governor checkpoint covers them.
        // A queued transport send alone is not delivery acknowledgement.
        let complete = crate::desktop::admission(&peer.state.borrow())
            .is_ok_and(|admission| admission.complete());
        report(&peer, if complete { "ready" } else { "admitting" }, None);
        Ok(())
    })
}
pub(crate) fn permissions(id: u32) -> Result<ui::permissions::PermissionContext, String> {
    PEERS.with(|peers| {
        let peer = peers.borrow().get(&id).cloned().ok_or("closed peer")?;
        let (role, caps, person) = peer
            .state
            .borrow()
            .permissions(peer._key.verifying_key().to_bytes(), SystemTime::now())?;
        Ok(ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(role), &person),
            caps,
        ))
    })
}

pub(crate) fn mark_failure(id: u32, failure: String) {
    PEERS.with(|peers| {
        if let Some(peer) = peers.borrow().get(&id) {
            *peer.admission_failure.borrow_mut() = Some(failure.clone());
            report(peer, "incomplete", Some(failure));
        }
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchResult {
    batch_id: String,
    epoch: String,
    observed_at_ms: f64,
    result: BatchOutcome,
}
#[derive(Deserialize)]
struct BatchOutcome {
    ok: bool,
    code: Option<String>,
}
fn encode_pending_batch<T>(
    id: u32,
    encode: impl FnOnce(&str, &[PendingWrite], &[qos_peer_state::DocumentRow]) -> Result<T, JsValue>,
) -> Result<T, JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        let writes = peer.cache.pending_writes();
        if writes.is_empty() {
            return encode("0", &[], &[]);
        }
        let batch = peer
            .next_batch
            .get()
            .checked_add(1)
            .ok_or_else(|| error("batch sequence exhausted"))?;
        peer.next_batch.set(batch);
        let id = format!("{}:{batch}", peer.id);
        let result = encode(&id, &writes, &peer.state.borrow().rows())?;
        let journal_revision = writes
            .iter()
            .find(|write| matches!(write.key, StorageKey::Admission { .. }))
            .map(|write| write.revision);
        let basis = peer
            .admission_progress
            .borrow()
            .batch_basis(journal_revision);
        peer.batches.borrow_mut().insert(id, (writes, basis));
        Ok(result)
    })
}
#[wasm_bindgen]
pub fn peer_pending_batch(id: u32) -> Result<String, JsValue> {
    encode_pending_batch(id, |id, writes, records| {
        let values = writes
            .iter()
            .map(crate::browser::write_json)
            .collect::<Vec<_>>();
        Ok(serde_json::json!({"id":id,"writes":values,"records":records}).to_string())
    })
}
/// JS-owned typed buffers avoid expanding every binary byte into JSON digits.
/// The Rust-owned batch remains the sole authority for acknowledgement.
#[wasm_bindgen]
pub fn peer_pending_batch_bytes(id: u32) -> Result<JsValue, JsValue> {
    encode_pending_batch(id, |id, writes, records| {
        let result = js_sys::Object::new();
        let values = js_sys::Array::new();
        for write in writes {
            values.push(&crate::browser::write_bytes(write)?);
        }
        js_sys::Reflect::set(&result, &"id".into(), &id.into())?;
        js_sys::Reflect::set(&result, &"writes".into(), &values)?;
        let records = js_sys::JSON::parse(&serde_json::to_string(records).map_err(error)?)?;
        js_sys::Reflect::set(&result, &"records".into(), &records)?;
        Ok(result.into())
    })
}
#[wasm_bindgen]
pub fn acknowledge_peer_storage_batch(id: u32, raw: String) -> Result<(), JsValue> {
    let result: BatchResult = serde_json::from_str(&raw).map_err(error)?;
    if result.epoch.is_empty()
        || result.epoch.len() > 128
        || !result.observed_at_ms.is_finite()
        || result.observed_at_ms < 0.0
    {
        return Err(error("invalid storage batch observation"));
    }
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if peer
            .storage_epoch
            .borrow()
            .as_ref()
            .is_some_and(|epoch| epoch != &result.epoch)
        {
            return Err(error("foreign storage epoch"));
        }
        let (writes, basis) = peer
            .batches
            .borrow_mut()
            .remove(&result.batch_id)
            .ok_or_else(|| error("unknown or settled storage batch"))?;
        *peer.storage_epoch.borrow_mut() = Some(result.epoch);
        for write in writes {
            peer.cache
                .acknowledge(
                    &write,
                    if result.result.ok {
                        Ok(())
                    } else {
                        Err(result
                            .result
                            .code
                            .clone()
                            .unwrap_or_else(|| "storage-transaction-failed".into()))
                    },
                )
                .map_err(error)?;
        }
        if result.result.ok {
            peer.committed_at.set(Some(result.observed_at_ms));
            peer.admission_progress.borrow_mut().committed(basis);
        }
        report_light(&peer);
        Ok(())
    })
}
fn required_read_failure(peer: &Peer, state: &StatePeer) -> Option<String> {
    crate::desktop::admission(state)
        .ok()?
        .required
        .iter()
        .find_map(|record| {
            state
                .check_document_read(
                    peer._key.verifying_key().to_bytes(),
                    &record.app_id,
                    &record.record_id,
                    SystemTime::now(),
                )
                .err()
        })
}
fn health(
    peer: &Peer,
    state: &StatePeer,
    ready: bool,
    reported_error: Option<&str>,
    source: crate::admission_health::ReportSource,
    outgoing_blocked: usize,
) -> serde_json::Value {
    let journal_status = peer.cache.status(&StorageKey::Admission {
        workspace: state.room().into(),
    });
    let journal = journal_status
        .as_ref()
        .and_then(|status| status.committed.as_ref())
        .and_then(|(_, bytes)| peer.health_projection.borrow_mut().journal(&bytes.bytes));
    let durable_failure = crate::admission_health::durable_admission_failure(
        state,
        peer._key.verifying_key().to_bytes(),
        SystemTime::now(),
        peer.admission_failure.borrow().clone(),
    );
    let required_failure = required_read_failure(peer, state);
    let durable_failure = durable_failure.or(required_failure);
    let failure = crate::admission_health::reported_failure(
        durable_failure.as_deref(),
        reported_error,
        source,
        ready && state.pending_count() == 0,
    );
    let mut failures=failure.map(|text|{
        let (code,message,recovery)=crate::admission_health::failure_policy(text,source,durable_failure.is_some(),peer.transport_connected.get());
        serde_json::json!({"id":"runtime:admission","code":code,"scope":{"kind":if !peer.transport_connected.get(){"connection"}else{"admission"}},"severity":if durable_failure.is_some()||matches!(source,crate::admission_health::ReportSource::ReceiveRejection){"error"}else{"notice"},"message":message,"recovery":[if recovery=="review-permissions"{serde_json::json!({"kind":recovery,"failureId":"runtime:admission"})}else{serde_json::json!({"kind":recovery})}],"occurredAtMs":js_sys::Date::now()})
    }).into_iter().collect::<Vec<_>>();
    if outgoing_blocked > 0 {
        failures.push(serde_json::json!({
            "id":"runtime:outgoing",
            "code":"checkpoint-expired",
            "scope":{"kind":"admission"},
            "severity":"notice",
            "message":crate::admission_health::OUTGOING_CHECKPOINT_MESSAGE,
            "recovery":[{"kind":"refresh-checkpoint"}],
            "occurredAtMs":js_sys::Date::now()
        }));
    }
    if let Ok(admission) = crate::desktop::admission(state) {
        for app in admission.unsupported {
            failures.push(serde_json::json!({"id":format!("runtime:application:{app}"),"code":"runtime-unavailable","scope":{"kind":"admission"},"severity":"notice","message":format!("{app}: application integration or required service unavailable"),"recovery":[],"occurredAtMs":js_sys::Date::now()}));
        }
    }
    let epoch = peer.storage_epoch.borrow().clone();
    let revision = |revision: u64| {
        epoch
            .as_ref()
            .map(|epoch| serde_json::json!({"epoch":epoch,"revision":revision.to_string()}))
    };
    let pending = peer.cache.pending_writes();
    let records=state.rows().iter().map(|row|{
        let key=storage_key(state.room(),&row.app_id,&row.record_id);
        let status=peer.cache.status(&key);
        let durable_heads=journal.as_ref().and_then(|journal| {
            let heads=peer.cache.committed_document_heads(&key)?.ok()?.iter().map(ToString::to_string).collect::<Vec<_>>();
            journal.match_document_heads(&row.app_id,&row.record_id,&heads)
        });
        let durable_basis=durable_heads.as_ref().map(|heads|serde_json::json!({"heads":heads}));
        let is_durable=durable_basis.is_some()&&durable_heads.as_ref()==Some(&row.heads);
        let failed=status.as_ref().is_some_and(|status|status.error.is_some())||journal_status.as_ref().is_some_and(|status|status.error.is_some());
        let coverage=state.durable_coverage_with_current(&row.app_id,&row.record_id).ok().flatten().filter(|(receipt,_)|journal.as_ref().is_some_and(|journal|journal.receipts.get(&qos_peer_state::coverage::route_key(&row.app_id,&row.record_id))==Some(&receipt.receipt_id)));
        let covered=is_durable&&coverage.as_ref().is_some_and(|(_,current)|*current);
        let receipt=coverage.map(|(receipt,_)|receipt);
        let outgoing=peer.outbox.borrow().iter().filter(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id==&row.app_id&&record_id==&row.record_id)).count();
        let durable_outgoing=journal.as_ref().map(|journal|journal.outbox.iter().filter(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id==&row.app_id&&record_id==&row.record_id)).count()).unwrap_or(0);
        serde_json::json!({"record":{"roomId":state.room(),"appId":row.app_id,"recordId":row.record_id},"admission":if state.check_document_read(peer._key.verifying_key().to_bytes(),&row.app_id,&row.record_id,SystemTime::now()).is_ok(){"admitted"}else{"unavailable"},"current":{"heads":row.heads},"editing":{"mode":if durable_failure.is_some(){"blocked"}else if peer.transport_connected.get(){"online"}else{"offline-queued"}},"local":{"state":if failed{"failed"}else if is_durable{"durable"}else{"pending"},"durableBasis":durable_basis,"recordRevision":status.as_ref().and_then(|status|status.committed.as_ref()).and_then(|(r,_)|revision(*r)),"journalRevision":journal_status.as_ref().and_then(|status|status.committed.as_ref()).and_then(|(r,_)|revision(*r)),"pendingWrites":pending.iter().filter(|write|write.key==key||matches!(write.key,StorageKey::Admission{..})).count(),"pendingBytes":pending.iter().filter(|write|write.key==key||matches!(write.key,StorageKey::Admission{..})).map(|write|write.value.bytes.len()).sum::<usize>(),"committedAtMs":peer.committed_at.get()},"outgoing":{"pendingOperations":outgoing,"durableOperations":durable_outgoing},"acknowledgement":{"state":if covered{"covered"}else if receipt.is_some(){"pending"}else{"unknown"},"targetPeerId":receipt.as_ref().map(|receipt|receipt.coverage.author.iter().map(|byte|format!("{byte:02x}")).collect::<String>()),"coveredBasis":receipt.as_ref().map(|receipt|serde_json::json!({"heads":receipt.coverage.heads})),"coversCurrentBasis":covered,"receiptId":receipt.map(|receipt|receipt.receipt_id),"observedAtMs":if covered{peer.committed_at.get()}else{None}},"failureIds":[]})
    }).collect::<Vec<_>>();
    let admission_phase =
        crate::admission_health::admission_phase(state, ready, durable_failure.as_deref());
    serde_json::json!({"version":1,"transport":{"phase":if peer.closed.get(){"closed"}else if peer.transport_connected.get(){"connected"}else{"offline"},"authenticated":peer.transport_connected.get(),"generation":peer.transport_generation.get().to_string(),"lastRelayContactAtMs":peer.last_relay_contact.get(),"lastSessionPeerContactAtMs":peer.last_peer_contact.get()},"admission":{"phase":admission_phase,"progressRevision":peer.admission_progress.borrow().revision().to_string(),"requiredRecords":crate::desktop::admission(state).map(|admission|admission.required.iter().map(|row|serde_json::json!({"roomId":state.room(),"appId":row.app_id,"recordId":row.record_id})).collect::<Vec<_>>()).unwrap_or_default(),"pendingPrerequisites":state.pending_count(),"deadlineAtMs":null},"records":records,"capabilities":{"automaticRecovery":true,"retrySaving":true,"recoverRenderer":true,"durablePeerAcknowledgement":true},"failures":failures})
}

/// Read-only handoff from the committed admission owner to the retained desktop.
pub(crate) fn desktop_state(id: u32) -> Result<StatePeer, String> {
    PEERS.with(|peers| {
        let peer = peers.borrow().get(&id).cloned().ok_or("closed peer")?;
        let state = peer.state.borrow();
        let admission = crate::desktop::admission(&state)?;
        if !admission.complete() {
            return Err("canonical desktop records are not admitted".into());
        }
        if let Some(reason) = required_read_failure(&peer, &state) {
            return Err(reason);
        }
        if let Some(reason) = state.incomplete_reason() {
            return Err(reason);
        }
        Ok(state.clone())
    })
}

/// One admission installation path for retained and immediate committed state.
fn install_candidate(peer: &Peer, mut candidate: StatePeer) -> Result<(), String> {
    let (mut outbox, reconciliation_error) =
        match candidate.reconcile_outbox(&peer.outbox.borrow(), &peer._key, SystemTime::now()) {
            Ok(outbox) => (outbox, None),
            // Never discard newly accepted revocations/prerequisites
            // merely because local outgoing recovery is forbidden.
            Err(error) => (
                peer.outbox
                    .borrow()
                    .iter()
                    .filter(|message| !candidate.covers(message))
                    .cloned()
                    .collect(),
                Some(error),
            ),
        };
    // Acceptance may reconcile author sequences, but cannot retire the
    // immutable original without an exact designated durable receipt.
    for original in peer.outbox.borrow().iter() {
        if !candidate.outbox_message_retired(original)
            && !outbox.iter().any(|other| {
                postcard::to_allocvec(other).ok() == postcard::to_allocvec(original).ok()
            })
        {
            outbox.push(original.clone());
        }
    }
    let install = if candidate.incomplete_reason().is_some() {
        Ok(())
    } else {
        install_records(&peer, &candidate)
    };
    install?;
    stage_journal(peer, &candidate, &outbox)?;
    let journal = StorageKey::Admission {
        workspace: candidate.room().into(),
    };
    let revision = peer
        .cache
        .status(&journal)
        .filter(|status| !status.durable)
        .map(|status| status.revision);
    peer.admission_progress.borrow_mut().accepted(revision);
    *peer.admission_failure.borrow_mut() = reconciliation_error;
    *peer.outbox.borrow_mut() = outbox;
    *peer.state.borrow_mut() = candidate;
    crate::browser::refresh_peer_mounts(peer.id);
    report(peer, "admitting", None);
    Ok(())
}

fn action_context(peer: &Peer) -> Result<serde_json::Value, String> {
    if peer.closed.get() || !peer.transport_connected.get() {
        return Err("action transport disconnected".into());
    }
    if !peer.action_v2.get() {
        return Err("state-only peer: actions unavailable".into());
    }
    let state = peer.state.borrow();
    let target = state.authority();
    let sender = peer._key.verifying_key().to_bytes();
    state.permissions(target.device_public_key, SystemTime::now())?;
    state.permissions(sender, SystemTime::now())?;
    let own = state
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == sender)
        .ok_or("sender certificate missing")?;
    let hex = |bytes: [u8; 32]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(
        serde_json::json!({"room":state.room(),"sender":hex(sender),"senderRoot":hex(own.user_public_key),"target":hex(target.device_public_key),"targetRoot":hex(target.user_public_key),"certificate":serde_json::to_string(target).map_err(|e|e.to_string())?}),
    )
}
#[wasm_bindgen]
pub fn peer_action_notifications() -> bool {
    true
}
#[wasm_bindgen]
pub fn peer_action_context(id: u32) -> Result<String, JsValue> {
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    action_context(&peer)
        .map(|value| value.to_string())
        .map_err(error)
}
#[wasm_bindgen]
pub fn peer_action_request(id: u32, message: String) -> Result<String, JsValue> {
    use tilo_relay::peer_actions::{ActionMessage, DirectedAction};
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    action_context(&peer).map_err(error)?;
    let message: ActionMessage = serde_json::from_str(&message).map_err(error)?;
    if !matches!(
        message,
        ActionMessage::CatalogueRefresh { .. }
            | ActionMessage::InvokeBound { .. }
            | ActionMessage::Status { .. }
            | ActionMessage::Cancel { .. }
    ) {
        return Err(error("unsupported client request"));
    }
    let state = peer.state.borrow();
    let action = DirectedAction {
        room: state.room().parse().map_err(error)?,
        request_id: uuid::Uuid::new_v4(),
        source: peer._key.verifying_key().to_bytes(),
        target: state.authority().device_public_key,
        message,
    };
    let request = action.request_id;
    peer.actions
        .borrow_mut()
        .insert(action.clone())
        .map_err(error)?;
    let result = peer
        .session
        .borrow()
        .as_ref()
        .ok_or_else(|| error("action transport disconnected"))?
        .send_action(action);
    if let Err(failure) = result {
        peer.actions.borrow_mut().forget(request);
        return Err(error(failure));
    }
    Ok(request.to_string())
}
#[wasm_bindgen]
pub fn peer_action_response(id: u32, request: String) -> Result<Option<String>, JsValue> {
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    action_context(&peer).map_err(error)?;
    peer.actions
        .borrow_mut()
        .take(request.parse().map_err(error)?)
        .map(|value| Ok(crate::action_client::browser_response(value).to_string()))
        .transpose()
}
#[wasm_bindgen]
pub fn peer_action_forget(id: u32, request: String) {
    if let (Some(peer), Ok(request)) = (
        PEERS.with(|peers| peers.borrow().get(&id).cloned()),
        request.parse(),
    ) {
        peer.actions.borrow_mut().forget(request);
    }
}
