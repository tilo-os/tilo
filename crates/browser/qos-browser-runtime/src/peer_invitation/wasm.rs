//! Closed invited custody. JavaScript carries checkpoints; it cannot install admission.
use super::*;
use crate::peer_invitation::InvitedIdentity;
use qos_peer_state::join::{
    self, JoinTransportRequest as Request, JoinTransportResponse as Response,
};
use tilo_relay::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostTrust};

struct Attempt {
    incoming: crate::peer_delivery::incoming::IncomingBudget,
    identity: RefCell<InvitedIdentity>,
    host: IrohWorkspaceHostHintV1,
    trust: WorkspaceHostTrust,
    origin: Option<String>,
    delivery_host: DeviceCert,
    cancel: crate::peer_onboarding::OnboardingCancel,
    alive: Rc<std::cell::Cell<bool>>,
    checkpoint: RefCell<Option<Vec<u8>>>,
    bootstrap: RefCell<Option<Rc<crate::peer_onboarding::OnboardingReply>>>,
    state: RefCell<Option<StatePeer>>,
    hint: RefCell<Option<join::TransportHint>>,
    busy: std::cell::Cell<bool>,
    consumed: std::cell::Cell<bool>,
    current_owner: Option<(u32, u64)>,
}
struct Operation<'a>(&'a Attempt);
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.0.busy.set(false);
    }
}
fn operation(value: &Attempt) -> Result<Operation<'_>, JsValue> {
    live(value)?;
    if value.consumed.get() || value.busy.replace(true) {
        return Err(error("invitation attempt busy or consumed"));
    }
    Ok(Operation(value))
}
thread_local! {
    static ATTEMPTS: RefCell<BTreeMap<u32, Rc<Attempt>>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT_ATTEMPT: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}
fn now() -> u64 {
    tilo_relay::clock::now_ns()
}
fn attempt(id: u32) -> Result<Rc<Attempt>, JsValue> {
    ATTEMPTS
        .with(|all| all.borrow().get(&id).cloned())
        .ok_or_else(|| error("invitation attempt closed"))
}
fn live(value: &Attempt) -> Result<(), JsValue> {
    let replaced = value.current_owner.is_some_and(|(id, generation)| {
        PEERS.with(|all| {
            all.borrow().get(&id).is_none_or(|peer| {
                peer.closed.get() || peer.transport_generation.get() != generation
            })
        })
    });
    if !value.alive.get() || replaced {
        Err(error("invitation attempt closed or replaced"))
    } else {
        Ok(())
    }
}
fn checkpoint(value: &Attempt, kind: &str) -> Result<String, JsValue> {
    live(value)?;
    let material = value.identity.borrow().encode().map_err(error)?;
    *value.checkpoint.borrow_mut() = Some(material.clone());
    Ok(serde_json::json!({"kind":kind,"material":material,
        "namespace":{"profile":format!("certified:{}",value.host.room),
        "device":value.identity.borrow().certificate().device_public_key.iter().map(|b|format!("{b:02x}")).collect::<String>(),
        "room":value.host.room},
        "descriptorUrl":value.origin.as_ref().map(|origin|format!("{origin}/peer/v3/descriptor"))}).to_string())
}
fn acknowledge(value: &Attempt, material: &[u8]) -> Result<(), JsValue> {
    live(value)?;
    if value.checkpoint.borrow().as_deref() != Some(material) {
        return Err(error("invitation checkpoint acknowledgement mismatch"));
    }
    // Decode the exact ACKed material again at the current clock. No unsigned
    // authority DTO is accepted at this boundary.
    let restored =
        InvitedIdentity::read_with_budget(material, now(), &value.incoming).map_err(error)?;
    if restored.public().map_err(error)? != value.identity.borrow().public().map_err(error)? {
        return Err(error("invitation identity changed"));
    }
    Ok(())
}
async fn request(
    value: &Attempt,
    request: Request,
) -> Result<crate::peer_onboarding::OnboardingReply, JsValue> {
    live(value)?;
    let prepared = crate::peer_onboarding::prepare_invited(
        &value.identity.borrow(),
        request,
        &value.incoming,
        now(),
    )
    .map_err(error)?;
    let response = crate::peer_onboarding::exchange_invited(prepared, &value.cancel, || {
        live(value).map_err(|value| {
            value
                .as_string()
                .unwrap_or_else(|| "invitation attempt closed".into())
        })
    })
    .await
    .map_err(error)?;
    live(value)?;
    if let Response::Error { code, .. } = response.response() {
        return Err(error(format!("Workspace join rejected ({code})")));
    }
    Ok(response)
}
#[wasm_bindgen]
pub fn invited_identity_scope(text: String) -> Result<String, JsValue> {
    InvitedIdentity::scope(&text).map_err(error)
}
#[wasm_bindgen]
pub fn invited_identity_create(text: String, label: String) -> Result<Vec<u8>, JsValue> {
    InvitedIdentity::create(&text, &label, now()).map_err(error)
}
#[wasm_bindgen]
pub fn invited_identity_public(material: Vec<u8>) -> Result<String, JsValue> {
    InvitedIdentity::read(&material, now())
        .and_then(|value| value.public())
        .map_err(error)
}
#[wasm_bindgen]
pub fn invited_begin(material: Vec<u8>, existing: Option<u32>) -> Result<u32, JsValue> {
    let incoming = incoming_for_owner(existing)?;
    let identity = InvitedIdentity::read_with_budget(&material, now(), &incoming).map_err(error)?;
    let invitation = identity.retained_invitation().map_err(error)?;
    let mut hints = invitation
        .grant()
        .transport
        .iter()
        .filter(|hint| hint.kind == join::TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1);
    let hint = hints
        .next()
        .ok_or_else(|| error("Invitation has no signed workspace host"))?;
    if hints.next().is_some() {
        return Err(error("Invitation has ambiguous workspace hosts"));
    }
    let host = IrohWorkspaceHostHintV1::from_transport_hint(hint).map_err(error)?;
    let trust = host
        .validate_invitation(invitation.grant())
        .map_err(error)?;
    let selected = identity.selected_delivery_pin().map_err(error)?;
    let delivery_host = selected
        .as_ref()
        .map(|(host, _)| host.clone())
        .unwrap_or_else(|| host.host_device_certificate.clone());
    let origin = selected.map(|(_, origin)| origin);
    existing
        .map(|id| {
            let peer = PEERS
                .with(|all| all.borrow().get(&id).cloned())
                .ok_or_else(|| error("closed Peer"))?;
            if peer.closed.get()
                || peer.media_enrollment.device != *identity.certificate()
                || peer.media_enrollment.governor != host.governing_authority
                || peer.data_host != host.host_device_certificate
                || peer.media_enrollment.host != delivery_host
                || peer.state.borrow().room() != host.room
            {
                return Err(error("invited current owner mismatch"));
            }
            Ok::<_, JsValue>(())
        })
        .transpose()?;
    let current_owner = existing
        .map(|id| {
            PEERS.with(|all| {
                all.borrow()
                    .get(&id)
                    .map(|peer| (id, peer.transport_generation.get()))
                    .ok_or_else(|| error("closed Peer"))
            })
        })
        .transpose()?;
    let value = Rc::new(Attempt {
        incoming,
        identity: RefCell::new(identity),
        host,
        trust,
        origin,
        delivery_host,
        cancel: crate::peer_onboarding::OnboardingCancel::new(),
        alive: Rc::new(std::cell::Cell::new(true)),
        checkpoint: RefCell::new(None),
        bootstrap: RefCell::new(None),
        state: RefCell::new(None),
        hint: RefCell::new(None),
        busy: std::cell::Cell::new(false),
        consumed: std::cell::Cell::new(false),
        current_owner,
    });
    ATTEMPTS.with(|all| {
        if all.borrow().len() >= 8 {
            return Err(error("Too many invitation attempts"));
        }
        let id = NEXT_ATTEMPT.with(|next| {
            let id = next.get();
            next.set(
                id.checked_add(1)
                    .ok_or_else(|| error("invitation handle overflow"))?,
            );
            Ok::<_, JsValue>(id)
        })?;
        all.borrow_mut().insert(id, value);
        Ok(id)
    })
}
#[wasm_bindgen]
pub fn invited_cancel(id: u32) -> js_sys::Promise {
    if let Some(value) = ATTEMPTS.with(|all| all.borrow_mut().remove(&id)) {
        value.alive.set(false);
        value.cancel.cancel();
    }
    // The JS source joins every raw operation before releasing its lease. The
    // closed executor joins the actual reader cancellation before settling.
    js_sys::Promise::resolve(&JsValue::UNDEFINED)
}
#[wasm_bindgen]
pub async fn invited_prepare(id: u32) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    let _request_credit = value.incoming.reserve(2 * 262_144).map_err(error)?;
    if value.identity.borrow().admitted() || value.identity.borrow().completion().is_some() {
        return checkpoint(&value, "checkpoint");
    }
    let joined = value.identity.borrow().invitation(now()).map_err(error)?;
    let inspection = request(
        &value,
        Request::Inspect {
            signed_invitation: joined.signed_invitation().to_vec(),
        },
    )
    .await?;
    match inspection.response() {
        Response::Inspection(inspection)
            if inspection.invite_id == joined.invite_id()
                && inspection.grant == *joined.grant() => {}
        _ => return Err(error("Workspace invitation inspection mismatch")),
    }
    drop(inspection);
    let requested = join::JoinChallengeRequest {
        signed_invitation: joined.signed_invitation().to_vec(),
        device_cert: value.identity.borrow().certificate().clone(),
        request_id: value.identity.borrow().request_id(),
    };
    let response = request(&value, Request::Challenge(requested)).await?;
    let challenge = match response.response() {
        Response::Challenge(response) => response.challenge.clone(),
        _ => return Err(error("Workspace challenge response mismatch")),
    };
    drop(response);
    value
        .identity
        .borrow_mut()
        .checkpoint_challenge(challenge, now())
        .map_err(error)?;
    checkpoint(&value, "checkpoint")
}
#[wasm_bindgen]
pub async fn invited_complete(id: u32, material: Vec<u8>) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    acknowledge(&value, &material)?;
    let _request_credit = value.incoming.reserve(2 * 262_144).map_err(error)?;
    if value.bootstrap.borrow().is_some() {
        return Err(error("invitation completion already admitted"));
    }
    let already_admitted = value.identity.borrow().admitted();
    let response = if already_admitted {
        let (nonce, _) = tilo_user_identity::DeviceIdentity::generate("Membership request", now());
        let request_id = nonce.to_bytes();
        let requested = join::MembershipChallengeRequest {
            room: value.host.room.clone(),
            workspace_id: value.host.workspace_id,
            device_cert: value.identity.borrow().certificate().clone(),
            request_id,
        };
        let response = request(&value, Request::MembershipChallenge(requested)).await?;
        let challenge = match response.response() {
            Response::MembershipChallenge(response) => response.challenge.clone(),
            _ => return Err(error("Membership challenge response mismatch")),
        };
        drop(response);
        let identity = value.identity.borrow();
        let expected = join::MembershipChallenge::new(
            &value.host.room,
            value.host.workspace_id,
            &value.host.governing_authority,
            identity.certificate(),
            request_id,
            challenge.nonce,
            challenge.expires_at_ns,
            now(),
        )
        .map_err(error)?;
        if expected != challenge
            || challenge.nonce == [0; 32]
            || challenge.expires_at_ns.saturating_sub(now()) > 60_000_000_000
        {
            return Err(error(
                "Membership challenge does not match retained identity",
            ));
        }
        let proof =
            join::sign_membership_proof(challenge, &identity.device_key()).map_err(error)?;
        drop(identity);
        request(
            &value,
            Request::MembershipComplete(join::MembershipCompleteRequest { proof }),
        )
        .await?
    } else {
        let identity = value.identity.borrow();
        let bytes = identity
            .completion()
            .ok_or_else(|| error("No ACKed redemption completion"))?;
        let charge = crate::peer_onboarding::preflight_completion(bytes).map_err(error)?;
        let _completion_credit = value.incoming.reserve(charge).map_err(error)?;
        let complete: join::JoinCompleteRequest = postcard::from_bytes(bytes).map_err(error)?;
        drop(identity);
        request(&value, Request::Complete(complete)).await?
    };
    let bootstrap = match response.response() {
        Response::Complete(value) if !already_admitted => value,
        Response::MembershipComplete(value) if already_admitted => value,
        _ => return Err(error("Workspace completion response mismatch")),
    };
    if value.host.validate_bootstrap(&bootstrap).map_err(error)? != value.trust {
        return Err(error("Workspace bootstrap transport trust changed"));
    }
    let state = value
        .identity
        .borrow_mut()
        .admit_bootstrap(&bootstrap, now())
        .map_err(error)?;
    if !state
        .certificates()
        .contains(&value.host.host_device_certificate)
    {
        return Err(error("Invited data host is not currently appointed"));
    }
    state
        .permissions(
            value.host.host_device_certificate.device_public_key,
            SystemTime::now(),
        )
        .map_err(error)?;
    let delivery = value
        .identity
        .borrow()
        .delivery_pin(&state, now())
        .map_err(error)?;
    if delivery
        .as_ref()
        .map(|(host, origin)| (host, origin.as_str()))
        != value
            .origin
            .as_ref()
            .map(|origin| (&value.delivery_host, origin.as_str()))
    {
        return Err(error("Invited delivery appointment changed"));
    }
    *value.state.borrow_mut() = Some(state);
    *value.bootstrap.borrow_mut() = Some(Rc::new(response));
    checkpoint(&value, "admitted")
}
#[wasm_bindgen]
pub fn invited_descriptor(id: u32, bytes: Vec<u8>) -> Result<(), JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    if value.state.borrow().is_none()
        || value.origin.is_none()
        || bytes.is_empty()
        || bytes.len() > 24_576
    {
        return Err(error("Invalid invited descriptor discovery"));
    }
    let hint: join::TransportHint = postcard::from_bytes(&bytes).map_err(error)?;
    if postcard::to_stdvec(&hint).map_err(error)? != bytes || hint.host != value.delivery_host {
        return Err(error("Invited descriptor pin/canonical mismatch"));
    }
    let state = value.state.borrow();
    let state = state
        .as_ref()
        .ok_or_else(|| error("No admitted invitation"))?;
    let authority = tilo_relay::delivery_v3::descriptor::DescriptorAuthority {
        peer: state,
        pinned_governor: &value.host.governing_authority,
        pinned_host: &value.delivery_host,
        workspace: value.host.workspace_id,
        generation: tilo_relay::delivery_v3::descriptor::GenerationExpectation::FirstAccepted {
            minimum_generation: 1,
        },
        allowed_delivery_capabilities: tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1,
        now: SystemTime::now(),
    };
    let preflight = tilo_relay::delivery_v3::descriptor::verify_transport_hint(&hint, &authority)
        .map_err(error)?;
    if Some(preflight.origin()) != value.origin.as_deref() {
        return Err(error("Invited descriptor origin changed"));
    }
    // Discovery is inert. Existing fresh signed bootstrap and durable floor ACK
    // still perform all descriptor authorization before ordinary media proofs.
    *value.hint.borrow_mut() = Some(hint);
    Ok(())
}
fn config(value: &Attempt) -> Result<ValidatedOwner, JsValue> {
    live(value)?;
    let initial = value
        .state
        .borrow()
        .clone()
        .ok_or_else(|| error("No admitted invitation"))?;
    let mut revoked = RevocationList::new();
    for entry in initial.revocations() {
        revoked.insert(entry).map_err(error)?;
    }
    Ok(ValidatedOwner {
        incoming: value.incoming.clone(),
        key: value.identity.borrow().device_key(),
        initial,
        revoked,
        certificate: value.identity.borrow().certificate().clone(),
        host_certificate: value.host.host_device_certificate.clone(),
        delivery_host: value.delivery_host.clone(),
        workspace: value.host.workspace_id,
        room: value.host.room.clone(),
        delivery_hint: value.hint.borrow().clone(),
        alive: Some(value.alive.clone()),
        governance: GovernanceSource::Invited(
            value
                .bootstrap
                .borrow()
                .as_ref()
                .ok_or_else(|| error("No admitted bootstrap"))?
                .clone(),
        ),
    })
}
#[wasm_bindgen]
pub async fn invited_connect(
    id: u32,
    material: Vec<u8>,
    hydrated: JsValue,
    callback: js_sys::Function,
) -> Result<u32, JsValue> {
    let value = attempt(id)?;
    acknowledge(&value, &material)?;
    let _operation = operation(&value)?;
    value.consumed.set(true);
    let peer = connect_validated(None, config(&value)?, hydrated, Some(callback)).await?;
    live(&value)?;
    Ok(peer)
}
#[wasm_bindgen]
pub async fn invited_reconnect(id: u32, material: Vec<u8>, peer: u32) -> Result<(), JsValue> {
    let value = attempt(id)?;
    acknowledge(&value, &material)?;
    let _operation = operation(&value)?;
    value.consumed.set(true);
    connect_validated(Some(peer), config(&value)?, JsValue::NULL, None).await?;
    live(&value)?;
    Ok(())
}
#[wasm_bindgen]
pub fn invited_refresh_media(id: u32, material: Vec<u8>, peer: u32) -> Result<(), JsValue> {
    let value = attempt(id)?;
    acknowledge(&value, &material)?;
    let _operation = operation(&value)?;
    value.consumed.set(true);
    let peer = PEERS
        .with(|all| all.borrow().get(&peer).cloned())
        .ok_or_else(|| error("closed Peer"))?;
    if peer.closed.get()
        || peer.media_enrollment.device != *value.identity.borrow().certificate()
        || peer.data_host != value.host.host_device_certificate
        || peer.media_enrollment.host != value.delivery_host
        || peer.media_enrollment.governor != value.host.governing_authority
        || peer.state.borrow().room() != value.host.room
    {
        return Err(error("Invited media refresh owner changed"));
    }
    crate::browser::prepare_peer_remote(peer.id).map_err(error)?;
    // The authenticated membership Complete retains native bootstrap bytes.
    // Recheck current membership after merging, before persisting this candidate.
    let governance = GovernanceSource::Invited(
        value
            .bootstrap
            .borrow()
            .as_ref()
            .ok_or_else(|| error("No membership bootstrap"))?
            .clone(),
    );
    let now = SystemTime::now();
    let mut cut = crate::peer_delivery::admission::CutAdmission::begin(
        &peer.state.borrow(),
        peer.incoming.clone(),
        now,
    )
    .map_err(error)?;
    for bytes in governance.messages() {
        cut.push_retained(bytes, now).map_err(error)?;
    }
    let candidate = cut.finish(now).map_err(error)?;
    candidate
        .permissions(peer._key.verifying_key().to_bytes(), SystemTime::now())
        .map_err(error)?;
    install_candidate(&peer, candidate).map_err(error)?;
    *peer.media_enrollment.hint.borrow_mut() = value.hint.borrow().clone();
    Ok(())
}
