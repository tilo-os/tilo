//! Wasm exports over the existing certified peer; no independent identity or storage.
use super::*;
use tilo_relay::peer_actions::{ActionMessage, DirectedAction};

fn current(id: u32) -> Result<(Rc<Peer>, serde_json::Value), JsValue> {
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    let identity = action_context(&peer).map_err(error)?;
    if !peer.transport_connected.get() || !peer.action_v2.get() {
        return Err(error("agent reads protocol not negotiated"));
    }
    Ok((peer, identity))
}

#[wasm_bindgen]
pub fn peer_agent_reads_supported(id: u32) -> bool {
    current(id).is_ok()
}

#[wasm_bindgen]
pub fn peer_agent_read_request(id: u32, message: String) -> Result<String, JsValue> {
    let (peer, identity) = current(id)?;
    if message.len() > qos_agent_control_protocol::MAX_REQUEST_FRAME_BYTES + 256 {
        return Err(error("agent read request too large"));
    }
    let message: ActionMessage = serde_json::from_str(&message).map_err(error)?;
    if !matches!(message, ActionMessage::AgentReadV1 { .. }) || !message.validate() {
        return Err(error("unsupported agent read"));
    }
    let generation = peer.transport_generation.get();
    let binding = peer
        .agent_reads
        .borrow()
        .binding(&identity, generation)
        .ok_or_else(|| error("current certified agent provider missing"))?;
    let state = peer.state.borrow();
    let root = state.authority().user_public_key;
    let action = DirectedAction {
        room: state.room().parse().map_err(error)?,
        request_id: uuid::Uuid::new_v4(),
        source: peer._key.verifying_key().to_bytes(),
        target: state.authority().device_public_key,
        message,
    };
    drop(state);
    let request = action.request_id;
    peer.agent_reads
        .borrow_mut()
        .insert(action.clone(), root, binding, identity, generation)
        .map_err(error)?;
    let result = delivery_api::queue_action(&peer, action);
    if let Err(failure) = result {
        peer.agent_reads.borrow_mut().forget(request);
        return Err(error(failure));
    }
    Ok(request.to_string())
}

#[wasm_bindgen]
pub fn peer_agent_read_response(id: u32, request: String) -> Result<Option<String>, JsValue> {
    let (peer, identity) = current(id)?;
    let message = peer.agent_reads.borrow_mut().take(
        request.parse().map_err(error)?,
        &identity,
        peer.transport_generation.get(),
    );
    message
        .map(|message| serde_json::to_string(&message).map_err(error))
        .transpose()
}

#[wasm_bindgen]
pub fn peer_agent_read_forget(id: u32, request: String) {
    if let (Some(peer), Ok(request)) = (
        PEERS.with(|peers| peers.borrow().get(&id).cloned()),
        request.parse(),
    ) {
        peer.agent_reads.borrow_mut().forget(request);
    }
}
