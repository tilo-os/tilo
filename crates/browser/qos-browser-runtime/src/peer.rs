//! Browser transport uses the same enrolled relay client and protocol as native.
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use qos_relay::{
    RelayClient,
    coedit::{Inbound, RelayCoeditSession},
};
use qos_user_identity::{
    DeviceCert, DirectoryUpdate, RevocationEntry, RevocationList, WorkspaceDirectory,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use ui::portable_storage::{PortableStorage, StorageKey};
use wasm_bindgen::prelude::*;
use web_time::SystemTime;
#[derive(Deserialize)]
struct Enrollment {
    version: u32,
    device_secret: [u8; 32],
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
#[derive(Serialize, Deserialize)]
struct Journal {
    state: Vec<u8>,
    outbox: Vec<NeighbourMsg>,
}
struct Peer {
    admission_failure: RefCell<Option<String>>,
    was_ready: std::cell::Cell<bool>,
    published: RefCell<BTreeSet<Vec<u8>>>,
    id: u32,
    cache: PortableStorage,
    outbox: RefCell<Vec<NeighbourMsg>>,
    endpoint: Endpoint,
    session: Rc<RelayCoeditSession>,
    state: RefCell<StatePeer>,
    _key: SigningKey,
    callback: js_sys::Function,
    closed: std::cell::Cell<bool>,
}
thread_local! {static PEERS:RefCell<BTreeMap<u32,Rc<Peer>>>=const{RefCell::new(BTreeMap::new())};static NEXT:std::cell::Cell<u32>=const{std::cell::Cell::new(1)};}
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn report(peer: &Peer, phase: &str, error: Option<String>) {
    let state = peer.state.borrow();
    let rows = state.rows();
    let complete = state.pending_count() == 0
        && rows.iter().any(|r| {
            r.app_id == "dev.qos.word-processor"
                && r.record_id == "tab:0000000000000001"
                && !r.heads.is_empty()
        })
        && rows
            .iter()
            .any(|r| r.app_id == "dev.qos.chat" && r.record_id.is_empty() && !r.heads.is_empty());
    let outgoing=state.outbox_status(&peer.outbox.borrow(),SystemTime::now());
    let incomplete = state.incomplete_reason().or_else(||peer.admission_failure.borrow().clone())
        .or_else(||(outgoing.blocked_expired>0).then(||"unsent durable edits await a fresh authorized checkpoint for reconciliation".into()));
    let phase = if incomplete.is_some() && !matches!(phase, "closed" | "failed") {
        "incomplete"
    } else if phase == "admitting" && complete && peer.cache.pending_writes().is_empty() {
        "ready"
    } else {
        phase
    };
    let phase=if phase=="admitting"&&peer.was_ready.get()&&complete{"ready"}else{phase};
    if phase=="ready"{peer.was_ready.set(true);}
    let error = error.or(incomplete);
    let value = serde_json::json!({"phase":phase,"room":state.room(),"deviceId":peer._key.verifying_key().to_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),"records":state.rows(),"pendingPrerequisites":state.pending_count(),"persistenceNeeded":!peer.cache.pending_writes().is_empty(),"outgoingPending":outgoing.pending,"outgoingBlocked":outgoing.blocked_expired,"synchronization":if outgoing.blocked_expired>0{"blocked"}else if outgoing.pending>0{"pending"}else{"acknowledged"},"error":error});
    let _ = peer
        .callback
        .call1(&JsValue::NULL, &JsValue::from_str(&value.to_string()));
}
#[wasm_bindgen]
pub async fn connect_peer(
    raw: String,
    hydrated: JsValue,
    callback: js_sys::Function,
) -> Result<u32, JsValue> {
    let config: Enrollment = serde_json::from_str(&raw).map_err(error)?;
    if config.version != 1 {
        return Err(error("unsupported enrollment version"));
    }
    let key = SigningKey::from_bytes(&config.device_secret);
    let device = key.verifying_key().to_bytes();
    if device != config.certificate.device_public_key
        || config.certificate.user_public_key != config.root_public_key
        || config.host_certificate.user_public_key != config.root_public_key
    {
        return Err(error("enrollment key/root binding mismatch"));
    }
    let now = qos_relay::clock::now_ns();
    let mut revoked = RevocationList::new();
    for entry in config.revocations {
        if entry.user_public_key != config.root_public_key {
            return Err(error("revocation root mismatch"));
        }
        revoked.insert(entry).map_err(error)?;
    }
    qos_user_identity::verify_device_cert_at(&config.certificate, now).map_err(error)?;
    qos_user_identity::verify_device_cert_at(&config.host_certificate, now).map_err(error)?;
    if revoked.is_revoked(&device) || revoked.is_revoked(&config.host_certificate.device_public_key)
    {
        return Err(error("enrolled device revoked"));
    }
    let workspace = config
        .bootstrap
        .workspace_id
        .parse::<u64>()
        .map_err(error)?;
    if qos_relay::room_workspace_id(&config.bootstrap.room).map_err(error)? != workspace {
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
    let state = StatePeer::new(
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
    let cache = crate::browser::load_cache(&profile, &device_hex, &hydrated)?;
    let (state, outbox) = if let Some(saved) = cache.get(&StorageKey::Admission {
        workspace: config.bootstrap.room.clone(),
    }) {
        let journal: Journal = postcard::from_bytes(&saved.bytes).map_err(error)?;
        let restored = StatePeer::restore(
            &journal.state,
            &config.bootstrap.room,
            state.authority().clone(),
            config.bootstrap.certified_roster.clone(),
            revoked.clone(),
            SystemTime::now(),
        )
        .map_err(error)?;
        (restored, journal.outbox)
    } else {
        (state, vec![])
    };
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Custom(RelayMap::from(config.packet_relay)))
        .bind()
        .await
        .map_err(error)?;
    let connected = RelayClient::connect_to_enrolled_home_peer(
        endpoint.clone(),
        config.relay_address,
        config.root_public_key,
        config.host_certificate.device_public_key,
        config.certificate.clone(),
        key.clone(),
        revoked,
    )
    .await;
    let client=match connected {Ok(client)=>client,Err(failure)=>{endpoint.close().await;return Err(error(failure));}};
    let session = match RelayCoeditSession::join(client, workspace, device) {Ok(session)=>Rc::new(session),Err(failure)=>{endpoint.close().await;return Err(error(failure));}};
    let id = NEXT.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let peer = Rc::new(Peer {
        admission_failure: RefCell::new(None),
        was_ready: std::cell::Cell::new(false),
        published: RefCell::new(BTreeSet::new()),
        id,
        cache,
        outbox: RefCell::new(outbox),
        endpoint,
        session: session.clone(),
        state: RefCell::new(state),
        _key: key,
        callback,
        closed: std::cell::Cell::new(false),
    });
    let hello = NeighbourMsg::IdentityHello {
        cert: Box::new(config.certificate),
        revocations: vec![],
    };
    let request = NeighbourMsg::SyncReq {
        since: vec![], room: config.bootstrap.room, invite_id: None,
    };
    let setup=(||->Result<(),JsValue>{
        session.publish_presence(postcard::to_allocvec(&hello).map_err(error)?).map_err(error)?;
        session.publish_presence(postcard::to_allocvec(&request).map_err(error)?).map_err(error)?;
        Ok(())
    })();
    if let Err(error)=setup { peer.endpoint.close().await; return Err(error); }
    PEERS.with(|peers| peers.borrow_mut().insert(id, peer.clone()));
    report(&peer, "admitting", None);
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(inbound) = session.recv().await {
            if peer.closed.get() {
                return;
            }
            let (author, data, authenticated) = match inbound {
                Inbound::Delta { author, data } => (author, data, true),
                Inbound::Presence { author, payload } => (author, payload, false),
                Inbound::Ready { .. } => {
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
            if authenticated && matches!(message,NeighbourMsg::SyncResp{..}|NeighbourMsg::AppDocument{..}) {
                if let Err(error)=crate::browser::prepare_peer_remote(peer.id){mark_failure(peer.id,error);continue;}
            }
            let mut candidate = peer.state.borrow().clone();
            let result = candidate.receive_from(
                if authenticated {
                    TransportOrigin::RelayDelta(author)
                } else {
                    TransportOrigin::UntrustedPresence(author)
                },
                message,
                SystemTime::now(),
            );
            match result {
                Ok(outcome) => {
                    if outcome.changed || outcome.metadata_changed {
                        let (outbox,reconciliation_error)=match candidate.reconcile_outbox(&peer.outbox.borrow(),&peer._key,SystemTime::now()) {
                            Ok(outbox)=>(outbox,None),
                            // Never discard newly accepted revocations/prerequisites
                            // merely because local outgoing recovery is forbidden.
                            Err(error)=>(peer.outbox.borrow().iter().filter(|message|!candidate.covers(message)).cloned().collect(),Some(error)),
                        };
                        let install = if candidate.incomplete_reason().is_some() {
                            Ok(())
                        } else {
                            install_records(&peer, &candidate)
                        };
                        match install.and_then(|()| stage_journal(&peer, &candidate, &outbox)) {
                            Ok(()) => {
                                *peer.admission_failure.borrow_mut()=reconciliation_error;
                                *peer.outbox.borrow_mut() = outbox;
                                *peer.state.borrow_mut() = candidate;
                                crate::browser::refresh_peer_mounts(peer.id);
                                report(&peer, "admitting", None);
                            }
                            Err(e) => { *peer.admission_failure.borrow_mut()=Some(e.clone());report(&peer, "incomplete", Some(e)); },
                        }
                    } else {
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
                Err(e) => report(&peer, "admitting", Some(e)),
            }
        }
        if !peer.closed.get() {
            report(
                &peer,
                "failed",
                Some("certified relay stream disconnected".into()),
            );
        }
    });
    Ok(id)
}
/// Disconnect transport without destroying durable cache/outbox ownership.
#[wasm_bindgen]
pub async fn disconnect_peer(id:u32) {
    let peer=PEERS.with(|peers|peers.borrow().get(&id).cloned());
    if let Some(peer)=peer {peer.closed.set(true);peer.endpoint.close().await;}
}
#[wasm_bindgen]
pub async fn close_peer(id: u32) {
    let peer = PEERS.with(|peers| peers.borrow_mut().remove(&id));
    if let Some(peer) = peer {
        peer.closed.set(true);
        peer.endpoint.close().await;
        report(&peer, "closed", None);
    }
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
    let bytes = postcard::to_allocvec(&Journal {
        state: state.durable()?,
        outbox: outbox.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    peer.cache.put(
        StorageKey::Admission {
            workspace: state.room().into(),
        },
        bytes,
        None,
    )
}
pub(crate) fn local_document(
    id: u32,
    app: &str,
    record: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or("certified peer is closed")?;
        let mut candidate = peer.state.borrow().clone();
        let prior = candidate
            .document(app, record)
            .ok_or("canonical native record not admitted")?;
        let before = automerge::AutoCommit::load(&prior).map_err(|e| e.to_string())?;
        let mut current = automerge::AutoCommit::load(&bytes).map_err(|e| e.to_string())?;
        if current.base_heads() == before.base_heads() {
            return Ok(());
        }
        let delta = current.save_after(&before.base_heads());
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
        report(&peer, "admitting", None);
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
        if peer.closed.get() { return Ok(()); }
        let pending = peer.outbox.borrow().clone();
        for message in &pending {
            if !qos_peer_state::outbox::fresh(message,SystemTime::now()) {continue;}
            let Some((channel, qos_peer_state::transport::Retention::Delta)) =
                qos_peer_state::transport::retention(message, false)
            else {
                return Err(error("outbox must contain incremental document state"));
            };
            let bytes = postcard::to_allocvec(message).map_err(error)?;
            if !peer.published.borrow().contains(&bytes) {
                peer.session
                    .publish_retained_delta(channel, bytes.clone())
                    .map_err(error)?;
                peer.published.borrow_mut().insert(bytes);
            }
        }
        // Keep durable outgoing bytes until a validated governor checkpoint covers them.
        // A queued transport send alone is not delivery acknowledgement.
        let rows = peer.state.borrow().rows();
        let complete = peer.state.borrow().pending_count() == 0
            && rows.iter().any(|r| {
                r.app_id == "dev.qos.word-processor"
                    && r.record_id == "tab:0000000000000001"
                    && !r.heads.is_empty()
            })
            && rows.iter().any(|r| {
                r.app_id == "dev.qos.chat" && r.record_id.is_empty() && !r.heads.is_empty()
            });
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

pub(crate) fn mark_failure(id:u32,failure:String){PEERS.with(|peers|if let Some(peer)=peers.borrow().get(&id){*peer.admission_failure.borrow_mut()=Some(failure.clone());report(peer,"incomplete",Some(failure));});}
