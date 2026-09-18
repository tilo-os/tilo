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
use serde::Deserialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use ui::portable_storage::{PortableStorage, StorageKey, PendingWrite};
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
    rejected_checkpoints: RefCell<BTreeSet<Vec<u8>>>,
    last_relay_contact: std::cell::Cell<Option<f64>>,
    last_peer_contact: std::cell::Cell<Option<f64>>,
    batches: RefCell<BTreeMap<String,Vec<PendingWrite>>>,
    next_batch: std::cell::Cell<u64>,
    storage_epoch: RefCell<Option<String>>,
    committed_at: std::cell::Cell<Option<f64>>,
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
    let complete = crate::desktop::admission(&state).is_ok_and(|admission| admission.complete());
    let outgoing=state.outbox_status(&peer.outbox.borrow(),SystemTime::now());
    let incomplete = required_read_failure(peer,&state).or_else(||state.incomplete_reason()).or_else(||peer.admission_failure.borrow().clone())
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
    let health=health(peer,&state,phase=="ready",error.as_deref());
    let synchronized=health["records"].as_array().is_some_and(|records|!records.is_empty()&&records.iter().all(|record|record["acknowledgement"]["coversCurrentBasis"]==true));
    let value = serde_json::json!({"phase":phase,"desktop":{"version":1,"mode":"session"},"room":state.room(),"deviceId":peer._key.verifying_key().to_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),"records":state.rows(),"pendingPrerequisites":state.pending_count(),"persistenceNeeded":!peer.cache.pending_writes().is_empty(),"outgoingPending":peer.outbox.borrow().len(),"outgoingBlocked":outgoing.blocked_expired,"synchronization":if outgoing.blocked_expired>0{"blocked"}else if synchronized{"acknowledged"}else{"pending"},"error":error,"health":health});
    let _ = peer
        .callback
        .call1(&JsValue::NULL, &JsValue::from_str(&value.to_string()));
}
#[wasm_bindgen]
pub async fn connect_peer(
    raw: String,
    identity: Vec<u8>,
    hydrated: JsValue,
    callback: js_sys::Function,
) -> Result<u32, JsValue> {
    connect_owner(None,raw,identity,hydrated,Some(callback)).await
}
#[wasm_bindgen]
pub async fn reconnect_peer(id:u32,raw:String,identity:Vec<u8>)->Result<(),JsValue>{
    connect_owner(Some(id),raw,identity,JsValue::NULL,None).await.map(|_|())
}
async fn connect_owner(existing:Option<u32>,raw:String,identity:Vec<u8>,hydrated:JsValue,callback:Option<js_sys::Function>)->Result<u32,JsValue>{
    let config: Enrollment = serde_json::from_str(&raw).map_err(error)?;
    if config.version != 2 {
        return Err(error("unsupported enrollment version"));
    }
    let key = crate::identity::key(&identity,&raw).map_err(error)?;
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
    let owner=existing.map(|id|PEERS.with(|peers|peers.borrow().get(&id).cloned()).ok_or_else(||error("closed peer"))).transpose()?;
    if let Some(peer)=&owner{if peer.state.borrow().room()!=initial.room()||peer.state.borrow().authority()!=initial.authority()||peer._key.verifying_key()!=key.verifying_key(){return Err(error("replacement transport owner binding mismatch"));}}
    let cache = if let Some(peer)=&owner{peer.cache.clone()}else{crate::browser::load_cache(&profile, &device_hex, &hydrated)?};
    let (state, outbox) = if let Some(peer)=&owner{(peer.state.borrow().clone(),peer.outbox.borrow().clone())}else if let Some(saved) = cache.get(&StorageKey::Admission {
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
        (restored, journal.outbox)
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
        Err(qos_relay::client::ClientError::UnsupportedProtocol) => (RelayClient::connect_to_enrolled_home_peer(
            endpoint.clone(), config.relay_address, config.root_public_key,
            config.host_certificate.device_public_key, config.certificate.clone(),key.clone(),revoked,
        ).await, false),
        result => (result, true),
    };
    let client=match connected {Ok(client)=>client,Err(failure)=>{endpoint.close().await;return Err(error(failure));}};
    let session = match RelayCoeditSession::join(client, workspace, device) {Ok(session)=>Rc::new(session),Err(failure)=>{endpoint.close().await;return Err(error(failure));}};
    let id = existing.unwrap_or_else(||NEXT.with(|next|{let id=next.get();next.set(id+1);id}));
    let peer=if let Some(peer)=owner{
        peer.action_v2.set(action_v2);
        *peer.actions.borrow_mut()=Default::default();
        peer.transport_generation.set(peer.transport_generation.get()+1);
        let prior=peer.endpoint.borrow_mut().replace(endpoint.clone());
        *peer.session.borrow_mut()=Some(session.clone());
        peer.transport_connected.set(true);peer.published.borrow_mut().clear();
        if let Some(prior)=prior{prior.close().await;}
        peer
    }else{Rc::new(Peer {
        actions: RefCell::default(),action_v2: std::cell::Cell::new(action_v2),
        admission_failure: RefCell::new(None),was_ready: std::cell::Cell::new(false),published: RefCell::new(BTreeSet::new()),
        id,cache,outbox:RefCell::new(outbox),endpoint:RefCell::new(Some(endpoint.clone())),session:RefCell::new(Some(session.clone())),state:RefCell::new(state),_key:key,
        callback:callback.ok_or_else(||error("missing status callback"))?,closed:std::cell::Cell::new(false),transport_generation:std::cell::Cell::new(1),transport_connected:std::cell::Cell::new(true),rejected_checkpoints:RefCell::new(BTreeSet::new()),last_relay_contact:std::cell::Cell::new(None),last_peer_contact:std::cell::Cell::new(None),batches:RefCell::new(BTreeMap::new()),next_batch:std::cell::Cell::new(0),storage_epoch:RefCell::new(None),committed_at:std::cell::Cell::new(None),
    })};
    let transport_generation=peer.transport_generation.get();
    let hello = NeighbourMsg::IdentityHello {
        cert: Box::new(config.certificate),
        revocations: vec![],
    };
    let request = NeighbourMsg::SyncReq {
        since: vec![], room: config.bootstrap.room, invite_id: None,
    };
    let setup=(||->Result<(),JsValue>{
        if action_v2 {
            let room = peer.state.borrow().room().parse().map_err(error)?;
            session.enable_actions(room).map_err(error)?;
            session.enable_broker_client(room).map_err(error)?;
        }
        // Rewrite legacy journal framing only through the same atomic cache transaction.
        stage_journal(&peer,&peer.state.borrow(),&peer.outbox.borrow()).map_err(error)?;
        session.publish_presence(postcard::to_allocvec(&hello).map_err(error)?).map_err(error)?;
        session.publish_presence(postcard::to_allocvec(&request).map_err(error)?).map_err(error)?;
        Ok(())
    })();
    if let Err(error)=setup {peer.transport_connected.set(false);endpoint.close().await;return Err(error);}
    PEERS.with(|peers| peers.borrow_mut().insert(id, peer.clone()));
    report(&peer, "admitting", None);
    if action_v2 {
        let peer=peer.clone();let session=session.clone();
        wasm_bindgen_futures::spawn_local(async move {
            while let Some(event)=session.recv_action().await {
                if peer.closed.get()||peer.transport_generation.get()!=transport_generation{return;}
                // Delivered is attested by the same mutually authenticated relay.
                // Current membership/certificate validity is rechecked for every response.
                if action_context(&peer).is_ok() {
                    let root=peer.state.borrow().authority().user_public_key;
                    peer.actions.borrow_mut().receive(event,root);
                }
            }
        });
    }
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(inbound) = session.recv().await {
            if peer.closed.get() || peer.transport_generation.get()!=transport_generation {
                return;
            }
            let (author, data, authenticated) = match inbound {
                Inbound::Delta { author, data } => (author, data, true),
                Inbound::Presence { author, payload } => (author, payload, false),
                Inbound::Ready { .. } => {
                    peer.last_relay_contact.set(Some(js_sys::Date::now()));
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
                    if authenticated && author==peer.state.borrow().authority().device_public_key{peer.last_peer_contact.set(Some(js_sys::Date::now()));}
                    if outcome.changed || outcome.metadata_changed {
                        let (mut outbox,reconciliation_error)=match candidate.reconcile_outbox(&peer.outbox.borrow(),&peer._key,SystemTime::now()) {
                            Ok(outbox)=>(outbox,None),
                            // Never discard newly accepted revocations/prerequisites
                            // merely because local outgoing recovery is forbidden.
                            Err(error)=>(peer.outbox.borrow().iter().filter(|message|!candidate.covers(message)).cloned().collect(),Some(error)),
                        };
                        // Acceptance may reconcile author sequences, but cannot retire the
                        // immutable original without an exact designated durable receipt.
                        for original in peer.outbox.borrow().iter(){
                            let durable=match original {NeighbourMsg::AppDocument{app_id,record_id,..}=>candidate.durable_coverage(app_id,record_id).ok().flatten().is_some()&&candidate.covers(original),_=>false};
                            if !durable && !outbox.iter().any(|other|postcard::to_allocvec(other).ok()==postcard::to_allocvec(original).ok()){outbox.push(original.clone());}
                        }
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
                Err(e) => {
                    if (e.contains("timestamp")||e.contains("expired")) && peer.rejected_checkpoints.borrow().len()<32 && peer.rejected_checkpoints.borrow_mut().insert(data){
                        let request=NeighbourMsg::SyncReq{since:vec![],room:peer.state.borrow().room().into(),invite_id:None};
                        if let Ok(bytes)=postcard::to_allocvec(&request){let _=session.publish_presence(bytes);}
                    }
                    report(&peer,"admitting",Some(e));
                },
            }
        }
        if !peer.closed.get() && peer.transport_generation.get()==transport_generation {
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
/// Disconnect transport without destroying durable cache/outbox ownership.
#[wasm_bindgen]
pub async fn disconnect_peer(id:u32) {
    let peer=PEERS.with(|peers|peers.borrow().get(&id).cloned());
    if let Some(peer)=peer {peer.transport_generation.set(peer.transport_generation.get()+1);peer.transport_connected.set(false);peer.session.borrow_mut().take();let endpoint=peer.endpoint.borrow_mut().take();if let Some(endpoint)=endpoint{endpoint.close().await;}}
}
#[wasm_bindgen]
pub async fn close_peer(id: u32) {
    let peer = PEERS.with(|peers| peers.borrow_mut().remove(&id));
    if let Some(peer) = peer {
        peer.closed.set(true);
        peer.transport_generation.set(peer.transport_generation.get()+1);
        let endpoint=peer.endpoint.borrow_mut().take();if let Some(endpoint)=endpoint{endpoint.close().await;}
        report(&peer, "closed", None);
    }
}

#[wasm_bindgen]
pub fn create_identity()->Vec<u8>{crate::identity::create()}
#[wasm_bindgen]
pub fn identity_request(material:&[u8])->Result<String,JsValue>{crate::identity::request(material).map_err(error)}
#[wasm_bindgen]
pub fn identity_complete(material:&[u8],challenge:&str)->Result<String,JsValue>{crate::identity::complete(material,challenge).map_err(error)}
#[wasm_bindgen]
pub fn bind_identity(material:&[u8],challenge:&str,raw:&str)->Result<Vec<u8>,JsValue>{crate::identity::bind(material,challenge,raw).map_err(error)}
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
    let bytes = Journal {
        state: state.durable()?,
        outbox: outbox.to_vec(),
        records: state.rows(),
        receipts: state.rows().iter().filter_map(|row|state.durable_coverage(&row.app_id,&row.record_id).ok().flatten().map(|receipt|(qos_peer_state::coverage::route_key(&row.app_id,&row.record_id),receipt.receipt_id))).collect(),
    }.encode()
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
        for message in qos_peer_state::outbox::publication_batch(&pending,SystemTime::now()) {
            let Some((channel, qos_peer_state::transport::Retention::Delta)) =
                qos_peer_state::transport::retention(message, false)
            else {
                return Err(error("outbox must contain incremental document state"));
            };
            let bytes = postcard::to_allocvec(message).map_err(error)?;
            if !peer.published.borrow().contains(&bytes) {
                peer.session.borrow().as_ref().ok_or_else(||error("transport unavailable"))?
                    .publish_retained_delta(channel, bytes.clone())
                    .map_err(error)?;
                peer.published.borrow_mut().insert(bytes);
            }
        }
        // Keep durable outgoing bytes until a validated governor checkpoint covers them.
        // A queued transport send alone is not delivery acknowledgement.
        let complete = crate::desktop::admission(&peer.state.borrow()).is_ok_and(|admission| admission.complete());
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

#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct BatchResult { batch_id:String,epoch:String,observed_at_ms:f64,result:BatchOutcome }
#[derive(Deserialize)]
struct BatchOutcome {ok:bool,code:Option<String>}
#[wasm_bindgen]
pub fn peer_pending_batch(id:u32)->Result<String,JsValue>{
    PEERS.with(|peers|{
        let peer=peers.borrow().get(&id).cloned().ok_or_else(||error("closed peer"))?;
        let writes=peer.cache.pending_writes();
        if writes.is_empty(){return Ok(serde_json::json!({"id":"0","writes":[],"records":[]}).to_string())}
        let batch=peer.next_batch.get().checked_add(1).ok_or_else(||error("batch sequence exhausted"))?;peer.next_batch.set(batch);
        let id=format!("{}:{batch}",peer.id);
        let values=writes.iter().map(|write|{let mut value=crate::browser::write_json(write);value["revision"]=write.revision.to_string().into();value}).collect::<Vec<_>>();
        peer.batches.borrow_mut().insert(id.clone(),writes);
        Ok(serde_json::json!({"id":id,"writes":values,"records":peer.state.borrow().rows()}).to_string())
    })
}
#[wasm_bindgen]
pub fn acknowledge_peer_storage_batch(id:u32,raw:String)->Result<(),JsValue>{
    let result:BatchResult=serde_json::from_str(&raw).map_err(error)?;
    if result.epoch.is_empty()||result.epoch.len()>128||!result.observed_at_ms.is_finite()||result.observed_at_ms<0.0{return Err(error("invalid storage batch observation"));}
    PEERS.with(|peers|{
        let peer=peers.borrow().get(&id).cloned().ok_or_else(||error("closed peer"))?;
        if peer.storage_epoch.borrow().as_ref().is_some_and(|epoch|epoch!=&result.epoch){return Err(error("foreign storage epoch"))}
        let writes=peer.batches.borrow_mut().remove(&result.batch_id).ok_or_else(||error("unknown or settled storage batch"))?;
        *peer.storage_epoch.borrow_mut()=Some(result.epoch);
        for write in writes{peer.cache.acknowledge(&write,if result.result.ok{Ok(())}else{Err(result.result.code.clone().unwrap_or_else(||"storage-transaction-failed".into()))}).map_err(error)?;}
        if result.result.ok{peer.committed_at.set(Some(result.observed_at_ms));}
        report(&peer,"admitting",None);Ok(())
    })
}
fn required_read_failure(peer:&Peer,state:&StatePeer)->Option<String>{
    crate::desktop::admission(state).ok()?.required.iter().find_map(|record|state.check_document_read(peer._key.verifying_key().to_bytes(),&record.app_id,&record.record_id,SystemTime::now()).err())
}
fn health(peer:&Peer,state:&StatePeer,ready:bool,reported_error:Option<&str>)->serde_json::Value{
    let journal_status=peer.cache.status(&StorageKey::Admission{workspace:state.room().into()});
    let journal=journal_status.as_ref().and_then(|status|status.committed.as_ref()).and_then(|(_,bytes)|Journal::decode(&bytes.bytes).ok());
    let durable_failure=crate::admission_health::durable_admission_failure(
        state,
        peer._key.verifying_key().to_bytes(),
        SystemTime::now(),
        peer.admission_failure.borrow().clone(),
    );
    let required_failure=required_read_failure(peer,state);
    let durable_failure=durable_failure.or(required_failure);
    let failure=durable_failure.as_deref().or(reported_error);
    let mut failures=failure.map(|text|{
        let (code,message,recovery)=if text.contains("revoked"){("identity-revoked","This device no longer has access.","review-permissions")}else if text.contains("MLS")||text.contains("crypto"){("crypto-unavailable","Encrypted content is waiting for its security prerequisites.","refresh-checkpoint")}else if text.contains("timestamp")||text.contains("expired"){("checkpoint-expired","A fresh copy of the shared state is required.","refresh-checkpoint")}else if text.contains("forbidden")||text.contains("permission"){("permission-required","Access to this record needs permission.","review-permissions")}else if !peer.transport_connected.get(){("transport-ended","The connection was interrupted. Saved work is retained.","retry-connection")}else{("record-install-failed","Shared state could not be loaded yet.","refresh-checkpoint")};
        serde_json::json!({"id":"runtime:admission","code":code,"scope":{"kind":if !peer.transport_connected.get(){"connection"}else{"admission"}},"severity":if durable_failure.is_some(){"error"}else{"notice"},"message":message,"recovery":[if recovery=="review-permissions"{serde_json::json!({"kind":recovery,"failureId":"runtime:admission"})}else{serde_json::json!({"kind":recovery})}],"occurredAtMs":js_sys::Date::now()})
    }).into_iter().collect::<Vec<_>>();
    if let Ok(admission)=crate::desktop::admission(state){for app in admission.unsupported{
        failures.push(serde_json::json!({"id":format!("runtime:application:{app}"),"code":"runtime-unavailable","scope":{"kind":"admission"},"severity":"notice","message":format!("{app}: application integration or required service unavailable"),"recovery":[],"occurredAtMs":js_sys::Date::now()}));
    }}
    let epoch=peer.storage_epoch.borrow().clone();
    let revision=|revision:u64|epoch.as_ref().map(|epoch|serde_json::json!({"epoch":epoch,"revision":revision.to_string()}));
    let pending=peer.cache.pending_writes();
    let records=state.rows().iter().map(|row|{
        let key=storage_key(state.room(),&row.app_id,&row.record_id);
        let status=peer.cache.status(&key);
        let durable_heads=status.as_ref().and_then(|status|status.committed.as_ref()).and_then(|(_,bytes)|journal.as_ref()?.durable_heads(&row.app_id,&row.record_id,&bytes.bytes));
        let durable_basis=durable_heads.as_ref().map(|heads|serde_json::json!({"heads":heads}));
        let is_durable=durable_basis.is_some()&&durable_heads.as_ref()==Some(&row.heads);
        let failed=status.as_ref().is_some_and(|status|status.error.is_some())||journal_status.as_ref().is_some_and(|status|status.error.is_some());
        let receipt=state.durable_coverage(&row.app_id,&row.record_id).ok().flatten().filter(|receipt|journal.as_ref().is_some_and(|journal|journal.receipts.get(&qos_peer_state::coverage::route_key(&row.app_id,&row.record_id))==Some(&receipt.receipt_id)));
        let covered=is_durable&&receipt.is_some()&&state.durable_covers_current(&row.app_id,&row.record_id).unwrap_or(false);
        let outgoing=peer.outbox.borrow().iter().filter(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id==&row.app_id&&record_id==&row.record_id)).count();
        let durable_outgoing=journal.as_ref().map(|journal|journal.outbox.iter().filter(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id==&row.app_id&&record_id==&row.record_id)).count()).unwrap_or(0);
        serde_json::json!({"record":{"roomId":state.room(),"appId":row.app_id,"recordId":row.record_id},"admission":if state.check_document_read(peer._key.verifying_key().to_bytes(),&row.app_id,&row.record_id,SystemTime::now()).is_ok(){"admitted"}else{"unavailable"},"current":{"heads":row.heads},"editing":{"mode":if durable_failure.is_some(){"blocked"}else if peer.transport_connected.get(){"online"}else{"offline-queued"}},"local":{"state":if failed{"failed"}else if is_durable{"durable"}else{"pending"},"durableBasis":durable_basis,"recordRevision":status.as_ref().and_then(|status|status.committed.as_ref()).and_then(|(r,_)|revision(*r)),"journalRevision":journal_status.as_ref().and_then(|status|status.committed.as_ref()).and_then(|(r,_)|revision(*r)),"pendingWrites":pending.iter().filter(|write|write.key==key||matches!(write.key,StorageKey::Admission{..})).count(),"pendingBytes":pending.iter().filter(|write|write.key==key||matches!(write.key,StorageKey::Admission{..})).map(|write|write.value.bytes.len()).sum::<usize>(),"committedAtMs":peer.committed_at.get()},"outgoing":{"pendingOperations":outgoing,"durableOperations":durable_outgoing},"acknowledgement":{"state":if covered{"covered"}else if receipt.is_some(){"pending"}else{"unknown"},"targetPeerId":receipt.as_ref().map(|receipt|receipt.coverage.author.iter().map(|byte|format!("{byte:02x}")).collect::<String>()),"coveredBasis":receipt.as_ref().map(|receipt|serde_json::json!({"heads":receipt.coverage.heads})),"coversCurrentBasis":covered,"receiptId":receipt.map(|receipt|receipt.receipt_id),"observedAtMs":if covered{peer.committed_at.get()}else{None}},"failureIds":[]})
    }).collect::<Vec<_>>();
    let admission_phase=crate::admission_health::admission_phase(state,ready,durable_failure.as_deref());
    serde_json::json!({"version":1,"transport":{"phase":if peer.closed.get(){"closed"}else if peer.transport_connected.get(){"connected"}else{"offline"},"authenticated":peer.transport_connected.get(),"lastRelayContactAtMs":peer.last_relay_contact.get(),"lastSessionPeerContactAtMs":peer.last_peer_contact.get()},"admission":{"phase":admission_phase,"requiredRecords":crate::desktop::admission(state).map(|admission|admission.required.iter().map(|row|serde_json::json!({"roomId":state.room(),"appId":row.app_id,"recordId":row.record_id})).collect::<Vec<_>>()).unwrap_or_default(),"pendingPrerequisites":state.pending_count(),"deadlineAtMs":null},"records":records,"capabilities":{"automaticRecovery":true,"retrySaving":true,"recoverRenderer":true,"durablePeerAcknowledgement":true},"failures":failures})
}

/// Read-only handoff from the committed admission owner to the retained desktop.
pub(crate) fn desktop_state(id:u32)->Result<StatePeer,String>{
    PEERS.with(|peers|{
        let peer=peers.borrow().get(&id).cloned().ok_or("closed peer")?;
        let state=peer.state.borrow();
        let admission=crate::desktop::admission(&state)?;
        if !admission.complete(){return Err("canonical desktop records are not admitted".into())}
        if let Some(reason)=required_read_failure(&peer,&state){return Err(reason)}
        if let Some(reason)=state.incomplete_reason(){return Err(reason)}
        Ok(state.clone())
    })
}

fn action_context(peer:&Peer)->Result<serde_json::Value,String>{
    if peer.closed.get()||!peer.transport_connected.get(){return Err("action transport disconnected".into());}
    if !peer.action_v2.get(){return Err("state-only peer: actions unavailable".into());}
    let state=peer.state.borrow();let target=state.authority();let sender=peer._key.verifying_key().to_bytes();
    state.permissions(target.device_public_key,SystemTime::now())?;
    state.permissions(sender,SystemTime::now())?;
    let own=state.certificates().into_iter().find(|cert|cert.device_public_key==sender).ok_or("sender certificate missing")?;
    let hex=|bytes:[u8;32]|bytes.iter().map(|b|format!("{b:02x}")).collect::<String>();
    Ok(serde_json::json!({"room":state.room(),"sender":hex(sender),"senderRoot":hex(own.user_public_key),"target":hex(target.device_public_key),"targetRoot":hex(target.user_public_key),"certificate":serde_json::to_string(target).map_err(|e|e.to_string())?}))
}
#[wasm_bindgen]
pub fn peer_action_context(id:u32)->Result<String,JsValue>{
    let peer=PEERS.with(|peers|peers.borrow().get(&id).cloned()).ok_or_else(||error("closed peer"))?;
    action_context(&peer).map(|value|value.to_string()).map_err(error)
}
#[wasm_bindgen]
pub fn peer_action_request(id:u32,message:String)->Result<String,JsValue>{
    use qos_relay::peer_actions::{ActionMessage,DirectedAction};
    let peer=PEERS.with(|peers|peers.borrow().get(&id).cloned()).ok_or_else(||error("closed peer"))?;
    action_context(&peer).map_err(error)?;
    let message:ActionMessage=serde_json::from_str(&message).map_err(error)?;
    if !matches!(message,ActionMessage::CatalogueRefresh|ActionMessage::InvokeBound{..}|ActionMessage::Status{..}|ActionMessage::Cancel{..}){return Err(error("unsupported client request"));}
    let state=peer.state.borrow();
    let action=DirectedAction{room:state.room().parse().map_err(error)?,request_id:uuid::Uuid::new_v4(),source:peer._key.verifying_key().to_bytes(),target:state.authority().device_public_key,message};
    let request=action.request_id;
    peer.actions.borrow_mut().insert(action.clone()).map_err(error)?;
    let result=peer.session.borrow().as_ref().ok_or_else(||error("action transport disconnected"))?.send_action(action);
    if let Err(failure)=result{peer.actions.borrow_mut().forget(request);return Err(error(failure));}
    Ok(request.to_string())
}
#[wasm_bindgen]
pub fn peer_action_response(id:u32,request:String)->Result<Option<String>,JsValue>{
    let peer=PEERS.with(|peers|peers.borrow().get(&id).cloned()).ok_or_else(||error("closed peer"))?;
    action_context(&peer).map_err(error)?;
    peer.actions.borrow_mut().take(request.parse().map_err(error)?).map(|value|Ok(crate::action_client::browser_response(value).to_string())).transpose()
}
#[wasm_bindgen]
pub fn peer_action_forget(id:u32,request:String){
    if let (Some(peer),Ok(request))=(PEERS.with(|peers|peers.borrow().get(&id).cloned()),request.parse()){peer.actions.borrow_mut().forget(request);}
}
