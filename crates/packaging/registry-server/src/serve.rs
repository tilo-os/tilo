//! HTTP request handling — the routes the registry exposes.
//!
//! Routes:
//!
//! - `GET  /registry`             → `RegistryHandshake` JSON
//! - `GET  /health`               → `{ "ok": true }`
//! - `GET  /entries`              → `[RegistryEntry, …]`
//! - `GET  /entries/<pkg-id>`     → single `RegistryEntry` or 404
//! - `POST /entries`              → `EntryChangeRequest` body,
//!                                 200 on success, 403 if the
//!                                 publisher isn't allowlisted or
//!                                 the sig doesn't verify.
//!
//! The dispatcher is kept small so the wire shape is auditable at
//! a glance. Business logic (add / update / delete decisions) is
//! in [`apply_change`].

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, header};
use tokio::sync::RwLock;
use tracing::{info, warn};

use registry_types::{
    EntryAction, EntryChangeRequest, RegistryEntry, RegistryHandshake, VerifyError,
    verify_change_request,
};

use crate::allowlist::PublisherAllowlist;
use crate::storage::{ServerIdentity, Storage, StorageError};

/// Shared per-request state — cheap to clone (an `Arc`).
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    storage: Storage,
    identity: ServerIdentity,
    /// Owner allowlist — hot-read on every POST, hot-write on
    /// `add-publisher`, so guard with an RwLock.
    allowlist: RwLock<PublisherAllowlist>,
}

impl AppState {
    /// Build the shared state.
    pub fn new(storage: Storage, identity: ServerIdentity, allowlist: PublisherAllowlist) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                storage,
                identity,
                allowlist: RwLock::new(allowlist),
            }),
        }
    }

    /// Owner's own view of the identity.
    pub fn identity(&self) -> &ServerIdentity {
        &self.inner.identity
    }
}

/// The one-and-only HTTP handler — dispatched to by
/// hyper::service_fn.
pub async fn handle(
    state: AppState,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    let res = match (method, path.as_str()) {
        (Method::GET, "/health") => Ok(json_response(StatusCode::OK, &serde_json::json!({"ok": true}))),
        (Method::GET, "/registry") => handle_get_registry(&state).await,
        (Method::GET, "/entries") => handle_get_entries(&state).await,
        (Method::GET, p) if p.starts_with("/entries/") => {
            let id = p.trim_start_matches("/entries/");
            handle_get_entry(&state, id).await
        }
        (Method::POST, "/entries") => handle_post_entries(&state, req).await,
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

async fn handle_get_registry(state: &AppState) -> Result<Response<Full<Bytes>>, HandlerError> {
    let ident = state.identity();
    let hs = RegistryHandshake::new(
        &ident.owner_pubkey_hex,
        &ident.name,
        &ident.description,
        &ident.base_url,
    );
    Ok(json_response(StatusCode::OK, &hs))
}

async fn handle_get_entries(state: &AppState) -> Result<Response<Full<Bytes>>, HandlerError> {
    let entries = state
        .inner
        .storage
        .load_entries()
        .map_err(HandlerError::Storage)?;
    Ok(json_response(StatusCode::OK, &entries))
}

async fn handle_get_entry(
    state: &AppState,
    package_id: &str,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    match state
        .inner
        .storage
        .load_entry(package_id)
        .map_err(HandlerError::Storage)?
    {
        Some(e) => Ok(json_response(StatusCode::OK, &e)),
        None => Ok(text_response(StatusCode::NOT_FOUND, "no such entry")),
    }
}

async fn handle_post_entries(
    state: &AppState,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let body = req
        .into_body()
        .collect()
        .await
        .map_err(|e| HandlerError::Body(e.to_string()))?
        .to_bytes();
    let change: EntryChangeRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return Ok(text_response(
                StatusCode::BAD_REQUEST,
                &format!("bad JSON: {e}"),
            ));
        }
    };

    let allow_snapshot = {
        let g = state.inner.allowlist.read().await;
        g.all_pubkey_bytes()
    };

    // Verify the signature FIRST — no touching disk before the
    // request is proven authorised.
    if let Err(e) = verify_change_request(&change, &allow_snapshot) {
        warn!("rejecting change request for {}: {e:?}", change.entry.package_id);
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
    apply_change(state, change).await
}

/// Apply a verified change request. Returns the resulting HTTP
/// response so [`handle_post_entries`] just forwards.
async fn apply_change(
    state: &AppState,
    change: EntryChangeRequest,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let existing = state
        .inner
        .storage
        .load_entry(&change.entry.package_id)
        .map_err(HandlerError::Storage)?;

    match change.action {
        EntryAction::Add => {
            if existing.is_some() {
                return Ok(text_response(
                    StatusCode::CONFLICT,
                    "entry already exists — POST with action=update instead",
                ));
            }
            // The owner's allowlist can gate "may add NEW entries" —
            // publishers with `can_add_new = false` may only edit
            // their own existing rows.
            if !publisher_may_add_new(state, &change.entry).await {
                return Ok(text_response(
                    StatusCode::FORBIDDEN,
                    "publisher not allowed to add new entries",
                ));
            }
            state
                .inner
                .storage
                .save_entry(&change.entry)
                .map_err(HandlerError::Storage)?;
            info!("added entry {}", change.entry.package_id);
            Ok(json_response(
                StatusCode::CREATED,
                &serde_json::json!({"status": "created", "id": change.entry.package_id}),
            ))
        }
        EntryAction::Update => {
            let Some(existing) = existing else {
                return Ok(text_response(
                    StatusCode::NOT_FOUND,
                    "no such entry to update",
                ));
            };
            if existing.publisher_pubkey_hex != change.entry.publisher_pubkey_hex {
                return Ok(text_response(
                    StatusCode::FORBIDDEN,
                    "publisher pubkey doesn't match the stored entry's — a publisher may only edit their own entries",
                ));
            }
            if change.entry.updated_at_unix <= existing.updated_at_unix {
                return Ok(text_response(
                    StatusCode::CONFLICT,
                    "updated_at_unix must be strictly greater than the stored value",
                ));
            }
            state
                .inner
                .storage
                .save_entry(&change.entry)
                .map_err(HandlerError::Storage)?;
            info!("updated entry {}", change.entry.package_id);
            Ok(json_response(
                StatusCode::OK,
                &serde_json::json!({"status": "updated", "id": change.entry.package_id}),
            ))
        }
        EntryAction::Delete => {
            let Some(existing) = existing else {
                return Ok(text_response(
                    StatusCode::NOT_FOUND,
                    "no such entry to delete",
                ));
            };
            if existing.publisher_pubkey_hex != change.entry.publisher_pubkey_hex {
                return Ok(text_response(
                    StatusCode::FORBIDDEN,
                    "publisher pubkey doesn't match the stored entry's",
                ));
            }
            state
                .inner
                .storage
                .delete_entry(&change.entry.package_id)
                .map_err(HandlerError::Storage)?;
            info!("deleted entry {}", change.entry.package_id);
            Ok(json_response(
                StatusCode::OK,
                &serde_json::json!({"status": "deleted", "id": change.entry.package_id}),
            ))
        }
    }
}

/// Cheap async lookup: is this publisher allowed to POST a new
/// entry (rather than only editing an existing one)?
async fn publisher_may_add_new(state: &AppState, entry: &RegistryEntry) -> bool {
    let Ok(bytes) = hex::decode(&entry.publisher_pubkey_hex) else {
        return false;
    };
    if bytes.len() != 32 {
        return false;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    let g = state.inner.allowlist.read().await;
    g.get(&key).map(|row| row.can_add_new).unwrap_or(false)
}

// ---- response helpers ------------------------------------------

fn json_response<T: serde::Serialize>(code: StatusCode, body: &T) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(bytes)))
        .expect("build JSON response")
}

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
}
