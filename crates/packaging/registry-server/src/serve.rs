//! HTTP request handling — the routes the registry exposes.
//!
//! Routes:
//!
//! - `GET  /registry`             → `RegistryHandshake` JSON
//! - `GET  /health`               → `{ "ok": true }`
//! - `GET  /entries`              → owner-signed `SignedRegistrySnapshot`
//! - `GET  /entries/<pkg-id>`     → the same signed snapshot, or 404
//! - `POST /entries`              → queue an `EntryChangeRequest`, 202 on
//!   success, 403 if the publisher isn't allowlisted or the signature
//!   doesn't verify.
//! - `POST /moderation/mutations` → verify and apply a signed report, reply,
//!   appeal, or owner decision.
//! - `GET  /moderation/cases/<id>` → convenience projection (not a proof).
//! - `GET  /moderation/transparency` → bounded public signed hash-chain page.
//!
//! The dispatcher is kept small so the wire shape is auditable at
//! a glance. Business logic (add / update / delete decisions) is
//! in [`apply_change`].

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, header};
use tracing::warn;

use registry_types::{
    EntryAction, EntryChangeRequest, RegistryEntry, RegistryHandshake, SignedModerationMutation,
    VerifyError, verify_change_request,
};

use crate::moderation::{ModerationError, ModerationService};
use crate::moderation_api::{ModerationApiError, ModerationApiService};
use crate::storage::{ServerIdentity, Storage, StorageError};

const MAX_CHANGE_REQUEST_BYTES: usize = 1024 * 1024;

/// Shared per-request state — cheap to clone (an `Arc`).
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    storage: Storage,
    moderation: ModerationService,
    moderation_api: ModerationApiService,
    identity: ServerIdentity,
}

impl AppState {
    /// Build the shared state.
    pub fn new(storage: Storage, identity: ServerIdentity) -> Self {
        let moderation = ModerationService::new(storage.clone());
        let moderation_api = ModerationApiService::new(storage.clone(), identity.clone());
        Self {
            inner: Arc::new(AppStateInner {
                storage,
                moderation,
                moderation_api,
                identity,
            }),
        }
    }

    /// Owner's own view of the identity.
    pub fn identity(&self) -> &ServerIdentity {
        &self.inner.identity
    }
}

/// The one-and-only HTTP handler — dispatched to by
/// `hyper::service_fn`.
///
/// # Errors
///
/// Never actually returns `Err` — every internal failure is caught and
/// turned into a `500` response. The `Result<_, hyper::Error>` return
/// type exists only because `hyper::service::service_fn` requires it.
pub async fn handle(
    state: AppState,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    let res = match (method, path.as_str()) {
        (Method::GET, "/health") => Ok(json_response(
            StatusCode::OK,
            &serde_json::json!({"ok": true}),
        )),
        (Method::GET, "/registry") => handle_get_registry(&state),
        (Method::GET, "/entries") => handle_get_entries(&state),
        (Method::GET, p) if p.starts_with("/entries/") => {
            let id = p.trim_start_matches("/entries/");
            handle_get_entry(&state, id)
        }
        (Method::POST, "/entries") => handle_post_entries(&state, req).await,
        (Method::POST, "/moderation/mutations") => handle_post_moderation(&state, req).await,
        (Method::GET, p) if p.starts_with("/moderation/cases/") => Ok(handle_get_moderation_case(
            &state,
            p.trim_start_matches("/moderation/cases/"),
        )),
        (Method::GET, "/moderation/transparency") => {
            Ok(handle_get_transparency(&state, req.uri().query()))
        }
        _ => Ok(text_response(StatusCode::NOT_FOUND, "not found")),
    };

    match res {
        Ok(r) => Ok(r),
        Err(e) => {
            warn!("handler error: {e}");
            Ok(text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            ))
        }
    }
}

async fn handle_post_moderation(
    state: &AppState,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    const MAX_MODERATION_REQUEST_BYTES: usize = 64 * 1024;
    let body = match Limited::new(req.into_body(), MAX_MODERATION_REQUEST_BYTES)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return Ok(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "moderation request is too large",
            ));
        }
        Err(error) => return Err(HandlerError::Body(error.to_string())),
    };
    let request: SignedModerationMutation = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return Ok(text_response(
                StatusCode::BAD_REQUEST,
                "invalid moderation JSON",
            ));
        }
    };
    let allowlist = state
        .inner
        .storage
        .load_allowlist()
        .map_err(HandlerError::Storage)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match state.inner.moderation_api.apply(&request, now, &allowlist) {
        Ok(case) => Ok(json_response(StatusCode::OK, &case)),
        Err(error) => Ok(moderation_error_response(&error)),
    }
}

fn handle_get_moderation_case(state: &AppState, case_id: &str) -> Response<Full<Bytes>> {
    match state.inner.moderation_api.case(case_id) {
        Ok(Some(case)) => json_response(StatusCode::OK, &case),
        Ok(None) => text_response(StatusCode::NOT_FOUND, "no such moderation case"),
        Err(error) => moderation_error_response(&error),
    }
}

fn handle_get_transparency(state: &AppState, query: Option<&str>) -> Response<Full<Bytes>> {
    let mut from = 1;
    let mut limit = 50;
    for pair in query.unwrap_or_default().split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "from" => from = value.parse().unwrap_or(0),
            "limit" => limit = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    match state.inner.moderation_api.transparency(from, limit) {
        Ok(page) => json_response(StatusCode::OK, &page),
        Err(error) => moderation_error_response(&error),
    }
}

fn moderation_api_status(error: &ModerationApiError) -> StatusCode {
    match error {
        ModerationApiError::Invalid(_) | ModerationApiError::BadSignature => {
            StatusCode::BAD_REQUEST
        }
        ModerationApiError::Expired => StatusCode::UNAUTHORIZED,
        ModerationApiError::Forbidden => StatusCode::FORBIDDEN,
        ModerationApiError::NotFound => StatusCode::NOT_FOUND,
        ModerationApiError::RequestIdReused => StatusCode::CONFLICT,
        ModerationApiError::CorruptLog
        | ModerationApiError::Moderation(_)
        | ModerationApiError::Unavailable
        | ModerationApiError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn moderation_error_response(error: &ModerationApiError) -> Response<Full<Bytes>> {
    let status = moderation_api_status(error);
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        warn!(%error, "moderation endpoint failed");
        text_response(status, "moderation service unavailable")
    } else {
        text_response(status, &error.to_string())
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "matches the Result<_, HandlerError> signature every route handler in the dispatch \
              match shares; this route just happens to have no fallible step"
)]
fn handle_get_registry(state: &AppState) -> Result<Response<Full<Bytes>>, HandlerError> {
    let ident = state.identity();
    let hs = RegistryHandshake::new(
        &ident.owner_pubkey_hex,
        &ident.name,
        &ident.description,
        &ident.base_url,
    );
    Ok(json_response(StatusCode::OK, &hs))
}

fn handle_get_entries(state: &AppState) -> Result<Response<Full<Bytes>>, HandlerError> {
    let snapshot = state
        .inner
        .storage
        .load_snapshot()
        .map_err(HandlerError::Storage)?;
    match snapshot {
        Some(snapshot) if snapshot_is_current(state, &snapshot) => {
            Ok(json_response(StatusCode::OK, &snapshot))
        }
        Some(_) => Ok(text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "published registry snapshot failed verification or expired",
        )),
        None => Ok(text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no owner-signed snapshot has been published",
        )),
    }
}

fn handle_get_entry(
    state: &AppState,
    package_id: &str,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    match state
        .inner
        .storage
        .load_snapshot()
        .map_err(HandlerError::Storage)?
    {
        Some(snapshot)
            if snapshot_is_current(state, &snapshot)
                && snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.package_id == package_id) =>
        {
            // Return the complete signed object: a detached row would discard
            // the owner's authorization and inclusion proof.
            Ok(json_response(StatusCode::OK, &snapshot))
        }
        Some(_) => Ok(text_response(StatusCode::NOT_FOUND, "no such entry")),
        None => Ok(text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no owner-signed snapshot has been published",
        )),
    }
}

fn snapshot_is_current(
    state: &AppState,
    snapshot: &registry_types::SignedRegistrySnapshot,
) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    registry_types::verify_snapshot(snapshot, &state.identity().owner_pubkey_hex, now).is_ok()
}

async fn handle_post_entries(
    state: &AppState,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let body = match Limited::new(req.into_body(), MAX_CHANGE_REQUEST_BYTES)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return Ok(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "change request is too large",
            ));
        }
        Err(error) => return Err(HandlerError::Body(error.to_string())),
    };
    let change: EntryChangeRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return Ok(text_response(
                StatusCode::BAD_REQUEST,
                &format!("bad JSON: {e}"),
            ));
        }
    };

    let allow_snapshot = state
        .inner
        .storage
        .load_allowlist()
        .map_err(HandlerError::Storage)?
        .all_pubkey_bytes();

    // Verify the signature FIRST — no touching disk before the
    // request is proven authorised.
    if let Err(e) = verify_change_request(&change, &allow_snapshot) {
        warn!(
            "rejecting change request for {}: {e:?}",
            change.entry.package_id
        );
        let (code, msg) = match e {
            VerifyError::PublisherNotAllowed => (
                StatusCode::FORBIDDEN,
                "publisher not in allowlist".to_string(),
            ),
            VerifyError::BadSignature | VerifyError::EntrySignatureMismatch => {
                (StatusCode::FORBIDDEN, e.to_string())
            }
            _ => (StatusCode::BAD_REQUEST, e.to_string()),
        };
        return Ok(text_response(code, &msg));
    }

    // Business rules layered on top of the signature check.
    apply_change(state, change)
}

/// Apply a verified change request. Returns the resulting HTTP
/// response so [`handle_post_entries`] just forwards.
fn apply_change(
    state: &AppState,
    change: EntryChangeRequest,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    if change.action == EntryAction::Add && !publisher_may_add_new(state, &change.entry) {
        return Ok(text_response(
            StatusCode::FORBIDDEN,
            "publisher not allowed to submit new entries",
        ));
    }
    let submitted_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match state.inner.moderation.submit(change, submitted_at) {
        Ok(submission) => Ok(json_response(
            StatusCode::ACCEPTED,
            &serde_json::json!({
                "status": "pending_review",
                "id": submission.submission_id,
            }),
        )),
        Err(error) => match moderation_status(&error) {
            Some(status) => Ok(text_response(status, &error.to_string())),
            None => Err(HandlerError::Moderation(error)),
        },
    }
}

fn moderation_status(error: &ModerationError) -> Option<StatusCode> {
    match error {
        ModerationError::NoPublicEntry(_) => Some(StatusCode::NOT_FOUND),
        ModerationError::WrongPublisher(_) | ModerationError::PendingOwnedByAnotherPublisher(_) => {
            Some(StatusCode::FORBIDDEN)
        }
        ModerationError::EntryAlreadyExists(_)
        | ModerationError::StaleTimestamp { .. }
        | ModerationError::PendingTimestampNotNewer { .. } => Some(StatusCode::CONFLICT),
        _ => None,
    }
}

/// Cheap async lookup: is this publisher allowed to POST a new
/// entry (rather than only editing an existing one)?
fn publisher_may_add_new(state: &AppState, entry: &RegistryEntry) -> bool {
    let Ok(bytes) = hex::decode(&entry.publisher_pubkey_hex) else {
        return false;
    };
    if bytes.len() != 32 {
        return false;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    state
        .inner
        .storage
        .load_allowlist()
        .ok()
        .and_then(|allowlist| allowlist.get(&key).cloned())
        .is_some_and(|row| row.can_add_new)
}

// ---- response helpers ------------------------------------------

// Both `.expect()`s below can only fail on an invalid status/header, and
// every status and header here is a hardcoded constant — never a value
// that could make `Response::builder()` reject it.
#[allow(
    clippy::expect_used,
    reason = "status codes and header values are hardcoded constants, never attacker- or \
              caller-controlled, so the builder cannot actually fail here"
)]
fn json_response<T: serde::Serialize>(code: StatusCode, body: &T) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(bytes)))
        .expect("build JSON response")
}

#[allow(
    clippy::expect_used,
    reason = "status codes and header values are hardcoded constants, never attacker- or \
              caller-controlled, so the builder cannot actually fail here"
)]
fn text_response(code: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("build text response")
}

#[derive(Debug, thiserror::Error)]
enum HandlerError {
    #[error("body: {0}")]
    Body(String),
    #[error("storage: {0}")]
    Storage(StorageError),
    #[error("moderation: {0}")]
    Moderation(ModerationError),
}
