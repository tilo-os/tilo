//! The only browser onboarding network executor. No arbitrary URL or signer API.
use super::*;
use js_sys::{Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn get(value: &JsValue, name: &str) -> Result<JsValue, String> {
    Reflect::get(value, &JsValue::from_str(name))
        .map_err(|_| "onboarding browser property unavailable".into())
}
fn set(value: &JsValue, name: &str, field: &JsValue) -> Result<(), String> {
    Reflect::set(value, &JsValue::from_str(name), field)
        .map(|_| ())
        .map_err(|_| "onboarding browser option unavailable".into())
}
fn call(value: &JsValue, name: &str, arg: Option<&JsValue>) -> Result<JsValue, String> {
    let function: Function = get(value, name)?
        .dyn_into()
        .map_err(|_| "onboarding browser method unavailable")?;
    match arg {
        Some(arg) => function.call1(value, arg),
        None => function.call0(value),
    }
    .map_err(|_| "onboarding browser method failed".into())
}
async fn settle(value: JsValue) -> Result<JsValue, String> {
    let promise: Promise = value
        .dyn_into()
        .map_err(|_| "onboarding browser promise unavailable")?;
    JsFuture::from(promise)
        .await
        .map_err(|_| "onboarding browser operation failed".into())
}
pub(super) fn abort(controller: &JsValue) {
    let _ = call(controller, "abort", None);
}
fn current(
    cancel: &OnboardingCancel,
    check: &impl Fn() -> Result<(), String>,
    deadline: u64,
) -> Result<(), String> {
    check()?;
    cancel.check()?;
    if tilo_relay::clock::now_ns() >= deadline {
        return Err("original onboarding deadline elapsed".into());
    }
    Ok(())
}

pub(super) async fn fetch(
    origin: &str,
    path: &str,
    body: &[u8],
    maximum: usize,
    deadline: u64,
    budget: &IncomingBudget,
    cancel: &OnboardingCancel,
    check: &impl Fn() -> Result<(), String>,
) -> Result<ChargedBytes, String> {
    current(cancel, check, deadline)?;
    let remaining = deadline
        .checked_sub(tilo_relay::clock::now_ns())
        .filter(|n| *n > 0)
        .ok_or("onboarding fetch expired")?;
    // Origin is borrowed only from an already verified closed wrapper/context.
    if !origin.starts_with("https://") {
        return Err("invalid onboarding origin".into());
    }
    if !(wire::OnboardingOperation::from_target("POST", path).is_ok()
        || path == wire::HANDOFF_CHUNKS_PATH)
        || body.len() > wire::MAX_REQUEST_BYTES
        || maximum > 1024 * 1024
    {
        return Err("invalid closed onboarding fetch".into());
    }
    if cancel.controller.borrow().is_some() {
        return Err("onboarding network operation busy".into());
    }
    // Application-owned JS request bytes and a fixed BYOB read view are charged
    // before construction. The browser network stack is not claimed as native
    // allocation qualification. Unsupported BYOB streams fail closed.
    let _js = budget.reserve(
        body.len()
            .checked_add(64 * 1024 + 16 * 1024)
            .ok_or("onboarding browser charge overflow")?,
    )?;
    let mut output = ChargedBytes::zeroed(maximum, budget)?;
    let global = js_sys::global();
    let constructor: Function = get(&global, "AbortController")?
        .dyn_into()
        .map_err(|_| "AbortController unavailable")?;
    let controller = Reflect::construct(&constructor, &js_sys::Array::new())
        .map_err(|_| "AbortController unavailable")?;
    *cancel.controller.borrow_mut() = Some(controller.clone().into());
    let timer_controller: JsValue = controller.clone().into();
    let timeout =
        wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || abort(&timer_controller));
    let mut timer = None;
    let mut reader: Option<JsValue> = None;
    let result: Result<(), String> = async {
        let schedule: Function = get(&global, "setTimeout")?
            .dyn_into()
            .map_err(|_| "onboarding deadline timer unavailable")?;
        timer = Some(
            schedule
                .call2(
                    &global,
                    timeout.as_ref(),
                    &JsValue::from_f64((remaining / 1_000_000).max(1) as f64),
                )
                .map_err(|_| "onboarding deadline timer failed")?,
        );
        let options = Object::new();
        set(&options, "method", &JsValue::from_str("POST"))?;
        set(&options, "redirect", &JsValue::from_str("error"))?;
        set(&options, "credentials", &JsValue::from_str("omit"))?;
        set(&options, "cache", &JsValue::from_str("no-store"))?;
        set(&options, "signal", &get(&controller, "signal")?)?;
        let headers = Object::new();
        set(
            &headers,
            "Content-Type",
            &JsValue::from_str("application/octet-stream"),
        )?;
        set(&options, "headers", &headers)?;
        let request = Uint8Array::new_with_length(body.len() as u32);
        request.copy_from(body);
        set(&options, "body", &request)?;
        let fetch: Function = get(&global, "fetch")?
            .dyn_into()
            .map_err(|_| "fetch unavailable")?;
        let response = settle(
            fetch
                .call2(
                    &global,
                    &JsValue::from_str(&format!("{origin}{path}")),
                    &options,
                )
                .map_err(|_| "onboarding fetch failed")?,
        )
        .await?;
        current(cancel, check, deadline)?;
        if get(&response, "ok")?.as_bool() != Some(true)
            || get(&response, "redirected")?.as_bool() != Some(false)
        {
            // Carry only the exact status out so callers can retry a busy or
            // unreachable host (429/502/503/504); nothing else is exposed.
            return Err(match get(&response, "status")?.as_f64() {
                Some(status) if (100.0..=599.0).contains(&status) && status.fract() == 0.0 => {
                    format!("onboarding HTTP reply rejected ({})", status as u16)
                }
                _ => "onboarding HTTP reply rejected".into(),
            });
        }
        let stream = get(&response, "body")?;
        let mode = Object::new();
        set(&mode, "mode", &JsValue::from_str("byob"))?;
        reader = Some(call(&stream, "getReader", Some(&mode))?);
        let active = reader.as_ref().unwrap();
        let mut position: usize = 0;
        let capacity = (maximum + 1).min(64 * 1024);
        let mut view = Uint8Array::new_with_length(capacity as u32);
        loop {
            current(cancel, check, deadline)?;
            let previous = position;
            let read = settle(call(active, "read", Some(&view))?).await?;
            current(cancel, check, deadline)?;
            let done = get(&read, "done")?
                .as_bool()
                .ok_or("invalid onboarding stream result")?;
            let value = get(&read, "value")?;
            if !value.is_undefined() {
                let bytes: Uint8Array = value
                    .dyn_into()
                    .map_err(|_| "onboarding stream did not return bytes")?;
                let length = bytes.length() as usize;
                let end = position
                    .checked_add(length)
                    .filter(|end| *end <= maximum)
                    .ok_or("onboarding HTTP reply exceeds bound")?;
                bytes.copy_to(&mut output.bytes[position..end]);
                position = end;
                // BYOB transfers the same backing to the returned view. Reuse
                // it rather than allocating one ArrayBuffer per network read.
                if !done {
                    view = Uint8Array::new_with_byte_offset_and_length(
                        &bytes.buffer(),
                        0,
                        (maximum - position + 1).min(capacity) as u32,
                    );
                }
            }
            if done {
                break;
            }
            if position == previous {
                return Err("onboarding stream made no progress".into());
            }
        }
        output.bytes.truncate(position);
        current(cancel, check, deadline)?;
        Ok(())
    }
    .await;
    // Always join cancellation of the actual reader before releasing its held
    // request/view/destination credits, including generation changes and abort.
    abort(&controller);
    if let Some(reader) = reader {
        if let Ok(promise) = call(&reader, "cancel", None) {
            let _ = settle(promise).await;
        }
        let _ = call(&reader, "releaseLock", None);
    }
    if let Some(timer) = timer {
        let _ = call(&global, "clearTimeout", Some(&timer));
    }
    drop(timeout);
    cancel.controller.borrow_mut().take();
    result?;
    current(cancel, check, deadline)?;
    Ok(output)
}

pub(crate) async fn exchange_invited(
    prepared: PreparedJoin,
    cancel: &OnboardingCancel,
    check: impl Fn() -> Result<(), String>,
) -> Result<OnboardingReply, String> {
    cancel.check()?;
    check()?;
    let request =
        wire::OnboardingRequest::decode(prepared.request_bytes()).map_err(|e| e.to_string())?;
    if tilo_relay::clock::now_ns() >= request.expires_at_ns {
        return Err("onboarding request expired".into());
    }
    let raw = fetch(
        request.scope.origin,
        request.operation.path(),
        prepared.request_bytes(),
        1024 * 1024,
        request.expires_at_ns,
        &prepared.budget,
        cancel,
        &check,
    )
    .await?;
    let reply = prepared.verify_reply(&raw.bytes, tilo_relay::clock::now_ns())?;
    let mut deadline = request.expires_at_ns;
    let result = match reply.kind {
        wire::ReplyBodyKind::InlineLegacy => OnboardingReply::decode(reply.body, &prepared.budget)?,
        wire::ReplyBodyKind::HandoffManifest => {
            let context = handoff::ManifestContext {
                scope: request.scope,
                purpose: wire::HandoffPurpose::JoinResponse,
                device: prepared.key.verifying_key().to_bytes(),
                request_digest: request.request_digest().map_err(|e| e.to_string())?,
                request_nonce: request.nonce,
                expires_at_ns: request.expires_at_ns,
            };
            let transfer = handoff::Handoff::start(
                reply.body,
                context,
                &prepared.budget,
                tilo_relay::clock::now_ns(),
            )?;
            deadline = deadline.min(transfer.expires_at_ns());
            let bytes = transfer_all(
                transfer,
                request.scope.origin,
                &prepared.key,
                &prepared.budget,
                cancel,
                &check,
            )
            .await?;
            OnboardingReply::decode(&bytes.bytes, &prepared.budget)?
        }
    };
    current(cancel, &check, deadline)?;
    Ok(result)
}
pub(super) async fn transfer_all(
    mut transfer: handoff::Handoff,
    origin: &str,
    key: &SigningKey,
    budget: &IncomingBudget,
    cancel: &OnboardingCancel,
    check: &impl Fn() -> Result<(), String>,
) -> Result<ChargedBytes, String> {
    // Captured once: later ranges can shorten this deadline, never renew it.
    let deadline = transfer.expires_at_ns();
    while transfer.remaining() != 0 {
        current(cancel, check, deadline)?;
        let request = transfer.request(key, tilo_relay::clock::now_ns())?;
        let body = encode(&request, 512, budget)?;
        let chunk = fetch(
            origin,
            wire::HANDOFF_CHUNKS_PATH,
            &body.bytes,
            wire::MAX_HANDOFF_CHUNK_BYTES,
            request.expires_at_ns,
            budget,
            cancel,
            check,
        )
        .await?;
        transfer.push(&request, &chunk.bytes, tilo_relay::clock::now_ns())?;
    }
    current(cancel, check, deadline)?;
    transfer.finish(tilo_relay::clock::now_ns())
}
