//! Closed Cloud owner custody. JavaScript carries checkpoints and opaque native
//! enrollment JSON; it cannot install admission or choose the delivery origin.
use super::*;
use crate::peer_cloud::{CloudOwner, CloudStep, EnrolledOwner};
use qos_peer_state::join::{
    self, JoinTransportRequest as Request, JoinTransportResponse as Response,
};

const MAX_ENROLLMENT_JSON: usize = 20 * 1024;
/// A Store that is busy or (re)establishing its native subscription answers
/// 503; a lossy link resets. ~8 jittered attempts span roughly 10 s.
const MEMBERSHIP_RETRIES: u32 = 8;

struct Attempt {
    incoming: crate::peer_delivery::incoming::IncomingBudget,
    owner: RefCell<CloudOwner>,
    origin: String,
    cancel: crate::peer_onboarding::OnboardingCancel,
    alive: Rc<std::cell::Cell<bool>>,
    scope: RefCell<Option<EnrolledOwner>>,
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
        return Err(error("Cloud owner attempt busy or consumed"));
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
        .ok_or_else(|| error("Cloud owner attempt closed"))
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
        Err(error("Cloud owner attempt closed or replaced"))
    } else {
        Ok(())
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// The delivery origin is the page origin; the signed descriptor and the
/// host's onboarding verification bind it to the enrolled host certificate.
fn page_origin() -> Result<String, JsValue> {
    let location = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("location"))
        .map_err(|_| error("Page origin unavailable"))?;
    let origin = js_sys::Reflect::get(&location, &JsValue::from_str("origin"))
        .map_err(|_| error("Page origin unavailable"))?
        .as_string()
        .ok_or_else(|| error("Page origin unavailable"))?;
    if !origin.starts_with("https://") || origin.len() > 256 {
        return Err(error("Cloud owner requires an HTTPS page origin"));
    }
    Ok(origin)
}
fn opaque(text: &str) -> Result<(), JsValue> {
    if text.is_empty() || text.len() > MAX_ENROLLMENT_JSON {
        return Err(error("Invalid native enrollment JSON size"));
    }
    Ok(())
}
fn checkpoint(value: &Attempt, kind: &str, material: &[u8]) -> Result<String, JsValue> {
    live(value)?;
    let scope = value.scope.borrow();
    let namespace = scope.as_ref().map(|scope| {
        serde_json::json!({"profile":format!("certified:{}",scope.room()),
            "device":hex(&scope.certificate().device_public_key),"room":scope.room()})
    });
    Ok(
        serde_json::json!({"kind":kind,"material":material,"namespace":namespace,
        "descriptorUrl":scope.as_ref().map(|_|format!("{}/peer/v3/descriptor",value.origin))})
        .to_string(),
    )
}
/// The caller's exact durable bytes must equal the current acknowledged custody.
fn current(value: &Attempt, material: &[u8]) -> Result<(), JsValue> {
    live(value)?;
    if value.owner.borrow().material() != material {
        return Err(error("Cloud owner checkpoint acknowledgement mismatch"));
    }
    Ok(())
}
async fn request(
    value: &Attempt,
    request: Request,
) -> Result<crate::peer_onboarding::OnboardingReply, JsValue> {
    live(value)?;
    let prepared = {
        let owner = value.owner.borrow();
        let scope = value.scope.borrow();
        let scope = scope
            .as_ref()
            .ok_or_else(|| error("Cloud owner enrollment is incomplete"))?;
        crate::peer_onboarding::prepare_owner(
            crate::peer_onboarding::OwnerOnboarding {
                origin: &value.origin,
                host: scope.host(),
                certificate: scope.certificate(),
                room: scope.room(),
                workspace: scope.workspace(),
                key: owner.device_key().map_err(error)?,
            },
            request,
            &value.incoming,
            now(),
        )
        .map_err(error)?
    };
    let response = crate::peer_onboarding::exchange_invited(prepared, &value.cancel, || {
        live(value).map_err(|value| {
            value
                .as_string()
                .unwrap_or_else(|| "Cloud owner attempt closed".into())
        })
    })
    .await
    .map_err(error)?;
    live(value)?;
    if let Response::Error { code, .. } = response.response() {
        return Err(error(format!("Cloud owner membership rejected ({code})")));
    }
    Ok(response)
}

fn text(value: &JsValue) -> String {
    value.as_string().unwrap_or_default()
}
/// Transient membership exchange failures: HTTP 429/502/503/504 (outer or the
/// signed in-band reply) and a browser network failure. Signed refusals,
/// verification failures and cancellation are never retried.
fn transient(value: &JsValue) -> bool {
    let message = text(value);
    ["429", "502", "503", "504"].iter().any(|status| {
        message == format!("onboarding HTTP reply rejected ({status})")
            || message == format!("Cloud owner membership rejected ({status})")
    }) || message == "onboarding browser operation failed"
        || message == "onboarding fetch failed"
}
async fn pause(milliseconds: f64) -> Result<(), JsValue> {
    use wasm_bindgen::JsCast;
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let global = js_sys::global();
        let scheduled = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
            .ok()
            .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
            .map(|timer| timer.call2(&global, &resolve, &JsValue::from_f64(milliseconds)));
        if !matches!(scheduled, Some(Ok(_))) {
            let _ = reject.call1(
                &JsValue::NULL,
                &error("Cloud owner retry timer unavailable"),
            );
        }
    });
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map(|_| ())
}
/// Jittered exponential backoff before retry `attempt` (1-based); the
/// attempt must still be live (not cancelled or replaced) afterwards.
async fn backoff(value: &Attempt, attempt: u32) -> Result<(), JsValue> {
    let base = (200.0 * 2f64.powi(attempt.saturating_sub(1).min(8) as i32)).min(2000.0);
    pause((base * (0.5 + js_sys::Math::random() * 0.5)).round()).await?;
    // `cloud_cancel` clears liveness together with the onboarding cancel.
    live(value)
}

#[wasm_bindgen]
pub fn cloud_identity_scope(account_id: String, machine_id: String) -> Result<String, JsValue> {
    crate::peer_cloud::scope(&account_id, &machine_id).map_err(error)
}
#[wasm_bindgen]
pub fn cloud_identity_create(
    account_id: String,
    machine_id: String,
    label: String,
) -> Result<Vec<u8>, JsValue> {
    let budget =
        crate::peer_delivery::incoming::IncomingBudget::new(64 * 1024 * 1024).map_err(error)?;
    crate::peer_cloud::create(&account_id, &machine_id, &label, &budget, now()).map_err(error)
}
#[wasm_bindgen]
pub fn cloud_identity_public(
    material: Vec<u8>,
    account_id: String,
    machine_id: String,
) -> Result<String, JsValue> {
    let budget =
        crate::peer_delivery::incoming::IncomingBudget::new(64 * 1024 * 1024).map_err(error)?;
    CloudOwner::restore(&material, &account_id, &machine_id, &budget, now())
        .and_then(|owner| owner.public())
        .map_err(error)
}
#[wasm_bindgen]
pub fn cloud_begin(
    material: Vec<u8>,
    account_id: String,
    machine_id: String,
    existing: Option<u32>,
) -> Result<u32, JsValue> {
    let incoming = incoming_for_owner(existing)?;
    let owner = CloudOwner::restore(&material, &account_id, &machine_id, &incoming, now())
        .map_err(error)?;
    let origin = page_origin()?;
    let scope = match owner.step().map_err(error)? {
        CloudStep::Enrolled => Some(owner.enrolled().map_err(error)?),
        _ => None,
    };
    let current_owner = existing
        .map(|id| {
            let peer = PEERS
                .with(|all| all.borrow().get(&id).cloned())
                .ok_or_else(|| error("closed Peer"))?;
            let scope = scope
                .as_ref()
                .ok_or_else(|| error("Cloud owner current owner is not enrolled"))?;
            if peer.closed.get()
                || peer.media_enrollment.device != *scope.certificate()
                || peer.media_enrollment.governor != *scope.host()
                || peer.data_host != *scope.host()
                || peer.media_enrollment.host != *scope.host()
                || peer.state.borrow().room() != scope.room()
            {
                return Err(error("Cloud owner current owner mismatch"));
            }
            Ok::<_, JsValue>((id, peer.transport_generation.get()))
        })
        .transpose()?;
    let value = Rc::new(Attempt {
        incoming,
        owner: RefCell::new(owner),
        origin,
        cancel: crate::peer_onboarding::OnboardingCancel::new(),
        alive: Rc::new(std::cell::Cell::new(true)),
        scope: RefCell::new(scope),
        bootstrap: RefCell::new(None),
        state: RefCell::new(None),
        hint: RefCell::new(None),
        busy: std::cell::Cell::new(false),
        consumed: std::cell::Cell::new(false),
        current_owner,
    });
    ATTEMPTS.with(|all| {
        if all.borrow().len() >= 8 {
            return Err(error("Too many Cloud owner attempts"));
        }
        let id = NEXT_ATTEMPT.with(|next| {
            let id = next.get();
            next.set(
                id.checked_add(1)
                    .ok_or_else(|| error("Cloud owner handle overflow"))?,
            );
            Ok::<_, JsValue>(id)
        })?;
        all.borrow_mut().insert(id, value);
        Ok(id)
    })
}
#[wasm_bindgen]
pub fn cloud_cancel(id: u32) -> js_sys::Promise {
    if let Some(value) = ATTEMPTS.with(|all| all.borrow_mut().remove(&id)) {
        value.alive.set(false);
        value.cancel.cancel();
        if let Ok(mut owner) = value.owner.try_borrow_mut() {
            owner.cancel();
        }
    }
    js_sys::Promise::resolve(&JsValue::UNDEFINED)
}
/// Next remote operation from durable custody: `inspect`, `apply` (with the
/// deterministic operation key and opaque signed request) or `enrolled`.
#[wasm_bindgen]
pub fn cloud_step(id: u32) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    Ok(match value.owner.borrow().step().map_err(error)? {
        CloudStep::Inspect => serde_json::json!({"step":"inspect"}),
        CloudStep::Apply {
            operation_key,
            payload,
        } => serde_json::json!({"step":"apply","operationKey":operation_key,"payload":payload}),
        CloudStep::Enrolled => serde_json::json!({"step":"enrolled"}),
    }
    .to_string())
}
/// Signs the enrollment request for the Store's opaque challenge JSON.
#[wasm_bindgen]
pub fn cloud_enroll(id: u32, challenge: String) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    opaque(&challenge)?;
    let material = value
        .owner
        .borrow_mut()
        .enroll(&challenge, now())
        .map_err(error)?;
    checkpoint(&value, "checkpoint", &material)
}
/// Verifies the Store.apply reply against the retained signed request.
#[wasm_bindgen]
pub fn cloud_accept(id: u32, reply: String) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    opaque(&reply)?;
    let material = value
        .owner
        .borrow_mut()
        .accept(&reply, now())
        .map_err(error)?;
    checkpoint(&value, "checkpoint", &material)
}
/// Exact durable ACK of the last checkpoint; only then does custody advance.
#[wasm_bindgen]
pub fn cloud_acknowledge(id: u32, material: Vec<u8>) -> Result<(), JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    value
        .owner
        .borrow_mut()
        .acknowledge(&material, now())
        .map_err(error)?;
    let enrolled = matches!(
        value.owner.borrow().step().map_err(error)?,
        CloudStep::Enrolled
    );
    if enrolled && value.scope.borrow().is_none() {
        let scope = value.owner.borrow().enrolled().map_err(error)?;
        *value.scope.borrow_mut() = Some(scope);
    }
    Ok(())
}
/// Owner membership refresh against the page origin with a fresh request ID,
/// then bounded admission of the signed governance as the structural owner.
#[wasm_bindgen]
pub async fn cloud_complete(id: u32, material: Vec<u8>) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    current(&value, &material)?;
    let _request_credit = value.incoming.reserve(2 * 262_144).map_err(error)?;
    if value.bootstrap.borrow().is_some() {
        return Err(error("Cloud owner membership already admitted"));
    }
    if value.scope.borrow().is_none() {
        return Err(error("Cloud owner enrollment is incomplete"));
    }
    let (nonce, _) = tilo_user_identity::DeviceIdentity::generate("Membership request", now());
    let request_id = nonce.to_bytes();
    // A transient failure retries in place with jittered backoff: the same
    // request ID for the challenge, and a freshly signed proof (and wrapper)
    // while that challenge is unexpired; an expired one is challenged again.
    let mut failures = 0u32;
    let response = 'membership: loop {
        let challenge_request = value
            .owner
            .borrow()
            .refresh_challenge(request_id)
            .map_err(error)?;
        let response = match request(&value, challenge_request).await {
            Ok(response) => response,
            Err(failure) if transient(&failure) && failures < MEMBERSHIP_RETRIES => {
                failures += 1;
                backoff(&value, failures).await?;
                continue 'membership;
            }
            Err(failure) => return Err(failure),
        };
        let challenge = match response.response() {
            Response::MembershipChallenge(response) => response.challenge.clone(),
            _ => return Err(error("Cloud owner membership challenge response mismatch")),
        };
        drop(response);
        loop {
            let proof = match value
                .owner
                .borrow()
                .refresh_proof(&challenge, request_id, now())
            {
                Ok(proof) => proof,
                Err(refusal)
                    if failures > 0 && refusal.starts_with(crate::peer_cloud::CLOCK_SKEW) =>
                {
                    continue 'membership;
                }
                Err(refusal) => return Err(error(refusal)),
            };
            match request(&value, proof).await {
                Ok(response) => break 'membership response,
                Err(failure) if transient(&failure) && failures < MEMBERSHIP_RETRIES => {
                    failures += 1;
                    backoff(&value, failures).await?;
                }
                Err(failure) => return Err(failure),
            }
        }
    };
    let state = {
        let Response::OwnerMembershipComplete(bootstrap) = response.response() else {
            return Err(error("Cloud owner membership completion response mismatch"));
        };
        let scope = value.scope.borrow();
        let scope = scope
            .as_ref()
            .ok_or_else(|| error("Cloud owner enrollment is incomplete"))?;
        value
            .owner
            .borrow()
            .admit(scope, bootstrap, now())
            .map_err(error)?
    };
    *value.state.borrow_mut() = Some(state);
    *value.bootstrap.borrow_mut() = Some(Rc::new(response));
    let material = value.owner.borrow().material().to_vec();
    checkpoint(&value, "admitted", &material)
}
/// Store.apply refused the retained signed request (the adapter reported
/// `apply-refused`): stage a return to Inspect with the same keys. The JS
/// source saves and acknowledges this checkpoint like any other.
#[wasm_bindgen]
pub fn cloud_reset_apply(id: u32) -> Result<String, JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    let material = value.owner.borrow_mut().reset_apply(now()).map_err(error)?;
    checkpoint(&value, "checkpoint", &material)
}
#[wasm_bindgen]
pub fn cloud_descriptor(id: u32, bytes: Vec<u8>) -> Result<(), JsValue> {
    let value = attempt(id)?;
    let _operation = operation(&value)?;
    if value.state.borrow().is_none() || bytes.is_empty() || bytes.len() > 24_576 {
        return Err(error("Invalid Cloud owner descriptor discovery"));
    }
    let scope = value.scope.borrow();
    let scope = scope
        .as_ref()
        .ok_or_else(|| error("Cloud owner enrollment is incomplete"))?;
    let hint: join::TransportHint = postcard::from_bytes(&bytes).map_err(error)?;
    if postcard::to_stdvec(&hint).map_err(error)? != bytes || hint.host != *scope.host() {
        return Err(error("Cloud owner descriptor pin/canonical mismatch"));
    }
    let state = value.state.borrow();
    let state = state
        .as_ref()
        .ok_or_else(|| error("No admitted Cloud owner"))?;
    let authority = tilo_relay::delivery_v3::descriptor::DescriptorAuthority {
        peer: state,
        pinned_governor: scope.host(),
        pinned_host: scope.host(),
        workspace: scope.workspace(),
        generation: tilo_relay::delivery_v3::descriptor::GenerationExpectation::FirstAccepted {
            minimum_generation: 1,
        },
        allowed_delivery_capabilities: tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1,
        now: SystemTime::now(),
    };
    let preflight = tilo_relay::delivery_v3::descriptor::verify_transport_hint(&hint, &authority)
        .map_err(error)?;
    if preflight.origin() != value.origin.as_str() {
        return Err(error(
            "Cloud owner descriptor origin differs from the page origin",
        ));
    }
    // Discovery is inert; fresh signed bootstrap and the durable floor ACK
    // still authorize the descriptor before ordinary delivery/media proofs.
    *value.hint.borrow_mut() = Some(hint);
    Ok(())
}
fn config(value: &Attempt) -> Result<ValidatedOwner, JsValue> {
    live(value)?;
    let initial = value
        .state
        .borrow()
        .clone()
        .ok_or_else(|| error("No admitted Cloud owner"))?;
    let mut revoked = RevocationList::new();
    for entry in initial.revocations() {
        revoked.insert(entry).map_err(error)?;
    }
    let scope = value.scope.borrow();
    let scope = scope
        .as_ref()
        .ok_or_else(|| error("Cloud owner enrollment is incomplete"))?;
    Ok(ValidatedOwner {
        incoming: value.incoming.clone(),
        key: value.owner.borrow().device_key().map_err(error)?,
        initial,
        revoked,
        certificate: scope.certificate().clone(),
        host_certificate: scope.host().clone(),
        delivery_host: scope.host().clone(),
        workspace: scope.workspace(),
        room: scope.room().into(),
        delivery_hint: value.hint.borrow().clone(),
        alive: Some(value.alive.clone()),
        governance: GovernanceSource::Cloud(
            value
                .bootstrap
                .borrow()
                .as_ref()
                .ok_or_else(|| error("No admitted Cloud owner bootstrap"))?
                .clone(),
        ),
    })
}
#[wasm_bindgen]
pub async fn cloud_connect(
    id: u32,
    material: Vec<u8>,
    hydrated: JsValue,
    callback: js_sys::Function,
) -> Result<u32, JsValue> {
    let value = attempt(id)?;
    current(&value, &material)?;
    let _operation = operation(&value)?;
    value.consumed.set(true);
    let peer = connect_validated(None, config(&value)?, hydrated, Some(callback)).await?;
    live(&value)?;
    Ok(peer)
}
#[wasm_bindgen]
pub async fn cloud_reconnect(id: u32, material: Vec<u8>, peer: u32) -> Result<(), JsValue> {
    let value = attempt(id)?;
    current(&value, &material)?;
    let _operation = operation(&value)?;
    value.consumed.set(true);
    connect_validated(Some(peer), config(&value)?, JsValue::NULL, None).await?;
    live(&value)?;
    Ok(())
}
/// Transport-only reconnect of an existing, still valid Cloud owner: no
/// membership refresh (no host commits). The retained Peer keeps its admitted
/// state; the same device key and pinned host bootstrap a fresh delivery
/// session. Any doubt about the binding (enrollment, owner, appointment,
/// certificate validity, delivery hint) refuses with `refresh required`, and
/// the caller then runs the full owner membership refresh instead.
#[wasm_bindgen]
pub async fn cloud_resume_transport(
    peer: u32,
    material: Vec<u8>,
    account_id: String,
    machine_id: String,
) -> Result<(), JsValue> {
    let required = |reason: &str| error(format!("Cloud owner refresh required: {reason}"));
    let incoming = incoming_for_owner(Some(peer))?;
    let owner = CloudOwner::restore(&material, &account_id, &machine_id, &incoming, now())
        .map_err(|_| required("custody"))?;
    if !matches!(owner.step().map_err(error)?, CloudStep::Enrolled) {
        return Err(required("enrollment incomplete"));
    }
    let scope = owner.enrolled().map_err(error)?;
    let current = PEERS
        .with(|all| all.borrow().get(&peer).cloned())
        .ok_or_else(|| error("closed Peer"))?;
    if current.closed.get()
        || current.media_enrollment.device != *scope.certificate()
        || current.media_enrollment.governor != *scope.host()
        || current.data_host != *scope.host()
        || current.media_enrollment.host != *scope.host()
        || current.state.borrow().room() != scope.room()
        || current.media_enrollment.workspace != scope.workspace()
    {
        return Err(required("owner changed"));
    }
    let time = SystemTime::now();
    let state = current.state.borrow().clone();
    state
        .verify_current_owner(scope.certificate(), time)
        .map_err(|_| required("not the current owner"))?;
    if !state.certificates().contains(scope.host())
        || state
            .permissions(scope.host().device_public_key, time)
            .is_err()
    {
        return Err(required("host appointment"));
    }
    let hint = current
        .media_enrollment
        .hint
        .borrow()
        .clone()
        .ok_or_else(|| required("delivery hint"))?;
    if hint.host != *scope.host() {
        return Err(required("delivery hint"));
    }
    let mut revoked = RevocationList::new();
    for entry in state.revocations() {
        revoked.insert(entry).map_err(error)?;
    }
    let config = ValidatedOwner {
        incoming,
        key: owner.device_key().map_err(error)?,
        initial: state,
        revoked,
        certificate: scope.certificate().clone(),
        host_certificate: scope.host().clone(),
        delivery_host: scope.host().clone(),
        workspace: scope.workspace(),
        room: scope.room().into(),
        delivery_hint: Some(hint),
        alive: None,
        governance: GovernanceSource::Empty,
    };
    connect_validated(Some(peer), config, JsValue::NULL, None).await?;
    Ok(())
}
#[wasm_bindgen]
pub fn cloud_refresh_media(id: u32, material: Vec<u8>, peer: u32) -> Result<(), JsValue> {
    let value = attempt(id)?;
    current(&value, &material)?;
    let _operation = operation(&value)?;
    value.consumed.set(true);
    let peer = PEERS
        .with(|all| all.borrow().get(&peer).cloned())
        .ok_or_else(|| error("closed Peer"))?;
    {
        let scope = value.scope.borrow();
        let scope = scope
            .as_ref()
            .ok_or_else(|| error("Cloud owner enrollment is incomplete"))?;
        if peer.closed.get()
            || peer.media_enrollment.device != *scope.certificate()
            || peer.data_host != *scope.host()
            || peer.media_enrollment.host != *scope.host()
            || peer.media_enrollment.governor != *scope.host()
            || peer.state.borrow().room() != scope.room()
        {
            return Err(error("Cloud owner media refresh owner changed"));
        }
    }
    crate::browser::prepare_peer_remote(peer.id).map_err(error)?;
    // Merge the authenticated owner refresh's retained governance, then
    // recheck the structural owner before persisting this candidate.
    let governance = GovernanceSource::Cloud(
        value
            .bootstrap
            .borrow()
            .as_ref()
            .ok_or_else(|| error("No Cloud owner membership bootstrap"))?
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
        .verify_current_owner(&peer.media_enrollment.device, SystemTime::now())
        .map_err(error)?;
    install_candidate(&peer, candidate).map_err(error)?;
    *peer.media_enrollment.hint.borrow_mut() = value.hint.borrow().clone();
    Ok(())
}
