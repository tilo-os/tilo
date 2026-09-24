//! WASM calls operate on the existing Peer key and actual storage custody only.
use super::*;
use crate::peer_journal::AcknowledgedDescriptorFloor;
use crate::peer_media::{self, OwnerView, PeerMedia, PreparedRequest, Step};
use tilo_relay::delivery_v3::descriptor::GenerationExpectation;

pub(super) struct MediaEnrollment {
    pub workspace: u64,
    pub governor: DeviceCert,
    pub host: DeviceCert,
    pub device: DeviceCert,
    pub hint: RefCell<Option<qos_peer_state::join::TransportHint>>,
}
#[derive(Default)]
pub(super) struct MediaState {
    client: Option<PeerMedia>,
}
impl MediaState {
    pub fn expire(&mut self) -> bool {
        self.client
            .as_mut()
            .is_some_and(PeerMedia::expire_authorization)
    }
    pub fn close(&mut self) {
        if let Some(client) = &mut self.client {
            client.close();
        }
        self.client = None;
    }
}
fn owner(id: u32) -> Result<Rc<Peer>, JsValue> {
    let peer = PEERS
        .with(|p| p.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed Peer"))?;
    if peer.closed.get() {
        return Err(error("closed Peer"));
    }
    Ok(peer)
}
pub(super) fn identity(peer: &Peer) -> CursorIdentity {
    let p = &peer.media_enrollment;
    CursorIdentity {
        workspace: p.workspace,
        room: peer.state.borrow().room().into(),
        workspace_root: p.governor.user_public_key,
        device: p.device.device_public_key,
        device_root: p.device.user_public_key,
        host: p.host.device_public_key,
        host_root: p.host.user_public_key,
    }
}
pub(super) fn with_view<T>(
    peer: &Peer,
    operation: impl FnOnce(&OwnerView<'_>) -> Result<T, String>,
) -> Result<T, JsValue> {
    let state = peer.state.borrow();
    let batches = peer.batches.borrow();
    let lease = batches
        .lease_epoch()
        .ok_or_else(|| error("media storage custody unavailable"))?;
    let pins = &peer.media_enrollment;
    let generation = match AcknowledgedDescriptorFloor::read(&peer.cache, &identity(peer)) {
        Ok(value) => value.floor().expectation(),
        Err(error) if peer.journal_storage.borrow().descriptor.is_none() => {
            let _ = error;
            GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            }
        }
        Err(error) => return Err(super::error(error)),
    };
    let view = OwnerView {
        peer: &state,
        key: &peer._key,
        governor: &pins.governor,
        host: &pins.host,
        device: &pins.device,
        workspace: pins.workspace,
        lease_epoch: lease,
        generation,
        now_ns: tilo_relay::clock::now_ns(),
        state_revision: peer.state_revision.get(),
    };
    operation(&view).map_err(error)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn namespace(n: &tilo_relay::delivery_v3::media_wire::MediaNamespace) -> String {
    format!("{}/{}", hex(&n.call), hex(&n.publisher))
}
fn inventory(v: &tilo_relay::delivery_v3::media_wire::PublisherInventory) -> serde_json::Value {
    serde_json::json!({"revision":v.revision.to_string(),"namespaces":v.namespaces.iter().map(namespace).collect::<Vec<_>>()})
}
fn prepared(value: PreparedRequest) -> serde_json::Value {
    serde_json::json!({"kind":"request","id":value.id,"origin":value.origin,"target":value.target,"body":value.body,"proof":value.proof})
}
fn step(value: Step) -> String {
    match value {
        Step::Request(value)=>prepared(value),
        Step::HttpReady=>serde_json::json!({"kind":"ready"}),
        Step::Grant(v)=>serde_json::json!({"kind":"grant","handle":v.handle,"expiresInMs":v.expires_in_ns as f64/1_000_000.0,"presenceInMs":v.presence_in_ns as f64/1_000_000.0,"directions":v.directions,"publishNamespace":namespace(&v.namespace),"inventory":inventory(&v.inventory)}),
        Step::Relay(v)=>serde_json::json!({"kind":"relay","handle":v.handle,"carrier":v.carrier,"endpoint":v.endpoint,"authorization":v.setup,"publishNamespace":namespace(&v.namespace),"inventory":inventory(&v.inventory)}),
        Step::Presence(v)=>serde_json::json!({"kind":"presence","expiresInMs":v.expires_in_ns as f64/1_000_000.0,"inventory":inventory(&v.inventory)}),
        Step::Refreshed=>serde_json::json!({"kind":"refreshed"}),
        Step::Left=>serde_json::json!({"kind":"left"}),
        Step::RelayClosed=>serde_json::json!({"kind":"relay-closed"}),
    }.to_string()
}
#[wasm_bindgen]
pub fn peer_media_bootstrap(id: u32) -> Result<String, JsValue> {
    let peer = owner(id)?;
    let authority = peer
        .delivery
        .borrow()
        .authority
        .clone()
        .ok_or_else(|| error("shared HTTPS custody missing"))?;
    with_view(&peer, |view| {
        if !authority.borrow().product_ready(view) {
            return Err("shared HTTPS custody expired".into());
        }
        let mut media = peer.media.borrow_mut();
        if let Some(client) = &mut media.client {
            client.rebind_authority(view, authority)?;
        } else {
            media.client = Some(PeerMedia::with_authority(authority));
        }
        Ok("{\"kind\":\"ready\"}".into())
    })
}
#[wasm_bindgen]
pub fn peer_media_bootstrap_complete(_id: u32, _bytes: Vec<u8>) -> Result<(), JsValue> {
    Err(error("bootstrap is owned by the shared HTTPS carrier"))
}
#[wasm_bindgen]
pub fn peer_media_start_http(id: u32) -> Result<String, JsValue> {
    peer_media_bootstrap(id)
}
fn command(
    id: u32,
    operation: impl FnOnce(&mut PeerMedia, &OwnerView<'_>) -> Result<Step, String>,
) -> Result<String, JsValue> {
    let peer = owner(id)?;
    with_view(&peer, |view| {
        operation(
            peer.media
                .borrow_mut()
                .client
                .as_mut()
                .ok_or("media custody not installed")?,
            view,
        )
        .map(step)
    })
}
#[wasm_bindgen]
pub fn peer_media_advance(id: u32, request: u32, bytes: Vec<u8>) -> Result<String, JsValue> {
    command(id, |c, v| c.advance(v, request, &bytes))
}
#[wasm_bindgen]
pub fn peer_media_join(
    id: u32,
    call_id: String,
    publish: bool,
    subscribe: bool,
) -> Result<String, JsValue> {
    command(id, |c, v| {
        c.start_join(
            v,
            &call_id,
            tilo_relay::delivery_v3::media_wire::MediaDirections { publish, subscribe },
        )
        .map(Step::Request)
    })
}
#[wasm_bindgen]
pub fn peer_media_renew(id: u32, handle: u32) -> Result<String, JsValue> {
    command(id, |c, v| c.renew(v, handle).map(Step::Request))
}
#[wasm_bindgen]
pub fn peer_media_heartbeat(id: u32, handle: u32) -> Result<String, JsValue> {
    command(id, |c, v| c.heartbeat(v, handle).map(Step::Request))
}
#[wasm_bindgen]
pub fn peer_media_relay(id: u32, handle: u32) -> Result<String, JsValue> {
    command(id, |c, v| c.relay_challenge(v, handle).map(Step::Request))
}
#[wasm_bindgen]
pub fn peer_media_refresh(id: u32, handle: u32, carrier: u32) -> Result<String, JsValue> {
    command(id, |c, v| c.refresh(v, handle, carrier).map(Step::Request))
}
#[wasm_bindgen]
pub fn peer_media_close_relay(id: u32, handle: u32, carrier: u32) -> Result<String, JsValue> {
    command(id, |c, v| {
        c.close_relay(v, handle, carrier).map(Step::Request)
    })
}
#[wasm_bindgen]
pub fn peer_media_leave(id: u32, handle: u32) -> Result<String, JsValue> {
    command(id, |c, v| c.leave(v, handle))
}
#[wasm_bindgen]
pub fn peer_media_expire(id: u32) -> Result<(), JsValue> {
    owner(id)?.media.borrow_mut().expire();
    Ok(())
}
#[wasm_bindgen]
pub fn peer_media_reauthorize(id: u32, handle: u32) -> Result<String, JsValue> {
    command(id, |client, view| {
        client.reauthorize(view, handle).map(Step::Request)
    })
}
#[wasm_bindgen]
pub fn peer_media_reset(id: u32) -> Result<(), JsValue> {
    owner(id)?.media.borrow_mut().close();
    Ok(())
}
/// Candidate discovery is inert; ordinary proofs still require fresh bootstrap + ACK.
#[wasm_bindgen]
pub fn peer_media_update_hint(id: u32, raw: &str) -> Result<(), JsValue> {
    if raw.len() > 1_048_576 {
        return Err(error("media enrollment exceeds bounds"));
    }
    let peer = owner(id)?;
    let enrollment: Enrollment = serde_json::from_str(raw).map_err(error)?;
    if enrollment.version != 2
        || enrollment.root_public_key != peer.media_enrollment.device.user_public_key
    {
        return Err(error("media enrollment root mismatch"));
    }
    let hint = enrollment
        .bootstrap
        .delivery_hint
        .ok_or_else(|| error("installed delivery hint unavailable"))?;
    with_view(&peer, |view| {
        view.validate_enrollment_hint(
            enrollment
                .bootstrap
                .workspace_id
                .parse()
                .map_err(|_| "invalid workspace")?,
            &enrollment.bootstrap.room,
            &enrollment.bootstrap.directory_author,
            &enrollment.host_certificate,
            &enrollment.certificate,
            &hint,
        )
    })?;
    peer.media_enrollment.hint.replace(Some(hint));
    Ok(())
}
#[wasm_bindgen]
pub fn peer_media_current(id: u32) -> bool {
    let Ok(peer) = owner(id) else { return false };
    with_view(&peer, |view| {
        Ok(peer
            .media
            .borrow()
            .client
            .as_ref()
            .is_some_and(|client| client.current(view)))
    })
    .unwrap_or(false)
}
#[wasm_bindgen]
pub fn peer_media_supported(id: u32) -> bool {
    let Ok(peer) = owner(id) else { return false };
    with_view(&peer, |view| {
        Ok(peer
            .media
            .borrow()
            .client
            .as_ref()
            .is_some_and(|client| client.supported(view)))
    })
    .unwrap_or(false)
}

#[wasm_bindgen]
pub fn peer_media_forget(id: u32, request: u32) -> Result<(), JsValue> {
    if let Some(client) = &mut owner(id)?.media.borrow_mut().client {
        client.forget(request);
    }
    Ok(())
}
#[wasm_bindgen]
pub fn peer_media_can_publish(id: u32) -> bool {
    let Ok(peer) = owner(id) else { return false };
    with_view(&peer, |view| {
        peer.media
            .borrow()
            .client
            .as_ref()
            .ok_or("media custody missing")?
            .can_publish(view)
    })
    .unwrap_or(false)
}
