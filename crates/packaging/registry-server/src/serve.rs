//! HTTP request handling — the routes the registry exposes.
//!
//! Routes:
//!
//! - `GET  /registry`             → `RegistryHandshake` JSON
//! - `GET  /health`               → `{ "ok": true }`
//! - `GET  /metrics`              → when the channel authorization and the
//!   published snapshot expire, for the operator's alerts (`crate::metrics`)
//! - `GET  /entries`              → owner-signed `SignedRegistrySnapshot`,
//!   expired or not (clients judge freshness)
//! - `GET  /entries?format=compact` → its `CompactRegistrySnapshot`, or the
//!   full snapshot while no current compact form exists
//! - `GET  /indexes/<blake3>.json` → a version index a compact snapshot names
//! - `GET  /entries/<pkg-id>`     → the same signed snapshot, or 404
//! - `POST /entries`              → queue an `EntryChangeRequest`, 202 on
//!   success, 403 if the publisher isn't allowlisted or the signature
//!   doesn't verify.
//! - `POST /moderation/mutations` → verify and apply a signed report, reply,
//!   appeal, or owner decision.
//! - `GET  /moderation/cases/<id>` → convenience projection (not a proof).
//! - `GET  /moderation/transparency` → bounded public signed hash-chain page.
//! - `GET  /blobs/<blake3>`       → a reviewed payload blob, or one chunk of
//!   a reviewed blob kept as chunks.
//! - `GET  /blobs/<blake3>.chunks` → the chunk list of a reviewed blob kept as
//!   chunks (see `tilo_app_package::chunks`).
//! - `GET  /thin/<root>.tapp`     → a reviewed package's thin form, by
//!   package root.
//! - `GET  /artifacts/<blake3>.tapp` → an operator-imported full package.
//! - `GET  /blobs/<blake3>.zst`, `GET /artifacts/<blake3>.tapp.zst` → the
//!   compressed twin of a payload object or imported package
//!   (`tilo_app_package::compressed`), as `application/zstd`; 404 when it
//!   has none, and the client takes the raw object.
//!
//! The last five, and `/indexes/`, are content addressed and served as
//! immutable, with exactly one `Cache-Control`, so an edge cache in front
//! may keep them forever. `/entries`
//! carries the snapshot hash as its `ETag`, so a client revalidating an
//! unchanged catalogue receives an empty `304`.
//!
//! Cross-origin reads are off by default. With configured origins
//! ([`ServeOptions::cors_origins`]) a browser page on exactly those origins may
//! read GET responses; no request method other than GET is ever granted, and
//! credentials are never allowed.
//!
//! The dispatcher is kept small so the wire shape is auditable at
//! a glance. Business logic (add / update / delete decisions) is
//! in `apply_change`.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, header};
use tracing::warn;

use registry_types::{
    EntryAction, EntryChangeRequest, RegistryEntry, RegistryHandshake, SignedModerationMutation,
    VerifyError, verify_change_request,
};

use crate::applications::{
    self, ApplicationFailure, ApplicationStatus, NetworkProofs, ProofSource, StoredApplication,
};
use crate::catalogue::{Catalogue, FullSnapshot, Published};
use crate::moderation::{ModerationError, ModerationService};
use crate::moderation_api::{ModerationApiError, ModerationApiService};
use crate::storage::{ServerIdentity, Storage, StorageError};
use registry_types::application::PublisherApplication;

const MAX_CHANGE_REQUEST_BYTES: usize = 1024 * 1024;

/// Shared per-request state — cheap to clone (an `Arc`).
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    storage: Storage,
    catalogue: Catalogue,
    moderation: ModerationService,
    moderation_api: ModerationApiService,
    identity: ServerIdentity,
    options: ServeOptions,
    proofs: Arc<dyn ProofSource + Send + Sync>,
}

/// Serving policy beyond the data directory.
#[derive(Clone, Debug, Default)]
pub struct ServeOptions {
    /// Exact origins (`scheme://host[:port]`) whose pages may read GET
    /// responses cross-origin. Empty means no CORS headers at all.
    pub cors_origins: Vec<String>,
}

/// Whether `origin` is a serialized web origin as a browser sends it in
/// `Origin`: `http` or `https`, a lower-case DNS name or IPv4 address, an
/// optional port, and nothing else — no path, wildcard or `null`.
pub fn valid_cors_origin(origin: &str) -> bool {
    let Some(authority) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return false;
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte))
        && port.is_none_or(|port| {
            (1..=5).contains(&port.len()) && port.bytes().all(|byte| byte.is_ascii_digit())
        })
}

impl AppState {
    /// Build the shared state with default serving options.
    pub fn new(storage: Storage, identity: ServerIdentity) -> Self {
        Self::with_options(storage, identity, ServeOptions::default())
    }

    /// Build the shared state.
    pub fn with_options(storage: Storage, identity: ServerIdentity, options: ServeOptions) -> Self {
        Self::with_proof_source(
            storage,
            identity,
            options,
            Arc::new(NetworkProofs::default()),
        )
    }

    /// Build the shared state, reading namespace proofs from `proofs`.
    pub fn with_proof_source(
        storage: Storage,
        identity: ServerIdentity,
        options: ServeOptions,
        proofs: Arc<dyn ProofSource + Send + Sync>,
    ) -> Self {
        let moderation = ModerationService::new(storage.clone());
        let moderation_api = ModerationApiService::new(storage.clone(), identity.clone());
        Self {
            inner: Arc::new(AppStateInner {
                storage,
                catalogue: Catalogue::default(),
                moderation,
                moderation_api,
                identity,
                options,
                proofs,
            }),
        }
    }

    fn allowed_origin(&self, origin: Option<&str>) -> Option<String> {
        let origin = origin?;
        self.inner
            .options
            .cors_origins
            .iter()
            .any(|allowed| allowed == origin)
            .then(|| origin.to_string())
    }

    /// How many times the published catalogue has been read and verified:
    /// once per published revision, however many clients ask for it.
    pub fn catalogue_loads(&self) -> u64 {
        self.inner.catalogue.loads()
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
    let origin = header_text(&req, header::ORIGIN);
    let if_none_match = header_text(&req, header::IF_NONE_MATCH);
    let is_get = method == Method::GET;

    let res = match (method, path.as_str()) {
        (Method::GET, "/health") => Ok(json_response(
            StatusCode::OK,
            &serde_json::json!({"ok": true}),
        )),
        (Method::GET, "/registry") => handle_get_registry(&state),
        (Method::GET, "/metrics") => published(&state)
            .await
            .map(|published| crate::metrics::response(&published)),
        (Method::GET, "/entries") => {
            let format = SnapshotFormat::requested(req.uri().query());
            published(&state)
                .await
                .map(|published| handle_get_entries(&published, format, if_none_match.as_deref()))
        }
        (Method::GET, p) if p.starts_with("/indexes/") => {
            let name = p.trim_start_matches("/indexes/").to_owned();
            read_file(&state, move |state| handle_get_version_index(state, &name)).await
        }
        (Method::GET, p) if p.starts_with("/entries/") && p.ends_with("/pending") => {
            let id = p
                .trim_start_matches("/entries/")
                .trim_end_matches("/pending")
                .to_owned();
            let read = PendingRead::from_request(&req);
            read_file(&state, move |state| handle_get_pending(state, &id, &read)).await
        }
        (Method::GET, p) if p.starts_with("/entries/") => {
            let id = p.trim_start_matches("/entries/");
            published(&state)
                .await
                .map(|published| handle_get_entry(&published, id))
        }
        (Method::POST, "/entries") => handle_post_entries(&state, req).await,
        (Method::GET, p) if p.starts_with("/blobs/") => {
            let name = p.trim_start_matches("/blobs/").to_owned();
            read_file(&state, move |state| handle_get_blob(state, &name)).await
        }
        (Method::GET, p) if p.starts_with("/thin/") => {
            let name = p.trim_start_matches("/thin/").to_owned();
            read_file(&state, move |state| {
                content_addressed(&name, |root| state.inner.storage.load_thin_package(root))
            })
            .await
        }
        (Method::GET, p) if p.starts_with("/artifacts/") => {
            let name = p.trim_start_matches("/artifacts/").to_owned();
            read_file(&state, move |state| handle_get_artifact(state, &name)).await
        }
        (Method::PUT, p) if p.starts_with("/uploads/") => {
            let name = p.trim_start_matches("/uploads/").to_owned();
            handle_put_upload(&state, &name, req).await
        }
        (Method::POST, "/publishers/applications") => handle_post_application(&state, req).await,
        (Method::GET, p) if p.starts_with("/publishers/applications/") => {
            let pubkey = p.trim_start_matches("/publishers/applications/").to_owned();
            blocking(&state, move |state| handle_get_application(state, &pubkey)).await
        }
        (Method::POST, "/moderation/mutations") => handle_post_moderation(&state, req).await,
        (Method::GET, p) if p.starts_with("/moderation/cases/") => {
            let case_id = p.trim_start_matches("/moderation/cases/").to_owned();
            blocking(&state, move |state| {
                Ok(handle_get_moderation_case(state, &case_id))
            })
            .await
        }
        (Method::GET, "/moderation/transparency") => {
            let query = req.uri().query().map(str::to_owned);
            blocking(&state, move |state| {
                Ok(handle_get_transparency(state, query.as_deref()))
            })
            .await
        }
        _ => Ok(text_response(StatusCode::NOT_FOUND, "not found")),
    };

    let mut response = res.unwrap_or_else(|e| {
        warn!("handler error: {e}");
        text_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    });
    if is_get && !state.inner.options.cors_origins.is_empty() {
        let headers = response.headers_mut();
        headers.append(header::VARY, header::HeaderValue::from_static("origin"));
        if let Some(value) = state
            .allowed_origin(origin.as_deref())
            .and_then(|origin| header::HeaderValue::from_str(&origin).ok())
        {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        }
    }
    Ok(response)
}

fn header_text(req: &Request<Incoming>, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Serve `<64 hex>.tapp` from a loader that re-checks the bytes against the
/// name, as an immutable response.
fn content_addressed(
    name: &str,
    load: impl FnOnce(&str) -> Result<Option<Vec<u8>>, StorageError>,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let Some(digest) = name.strip_suffix(".tapp") else {
        return Ok(text_response(StatusCode::NOT_FOUND, "not found"));
    };
    match load(digest).map_err(HandlerError::Storage)? {
        Some(bytes) => Ok(immutable_response(bytes)),
        None => Ok(text_response(StatusCode::NOT_FOUND, "not found")),
    }
}

/// Run a handler step that reads whole storage files on the I/O pool, off
/// the async workers.
async fn read_file<F>(state: &AppState, op: F) -> Result<Response<Full<Bytes>>, HandlerError>
where
    F: FnOnce(&AppState) -> Result<Response<Full<Bytes>>, HandlerError> + Send + 'static,
{
    let state = state.clone();
    tilo_rt::io::run(move || Ok(op(&state)))
        .await
        .map_err(HandlerError::Blocking)?
}

/// Run a handler step with other blocking storage work — directory scans,
/// and mutations that wait on the registry lock — on the runtime's blocking
/// pool, off the async workers.
async fn blocking<F>(state: &AppState, op: F) -> Result<Response<Full<Bytes>>, HandlerError>
where
    F: FnOnce(&AppState) -> Result<Response<Full<Bytes>>, HandlerError> + Send + 'static,
{
    let state = state.clone();
    tokio::task::spawn_blocking(move || op(&state))
        .await
        .map_err(|error| HandlerError::Blocking(std::io::Error::other(error.to_string())))?
}

async fn handle_post_application(
    state: &AppState,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    const MAX_APPLICATION_BYTES: usize = 16 * 1024;
    let body = match Limited::new(req.into_body(), MAX_APPLICATION_BYTES)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return Ok(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "publisher application is too large",
            ));
        }
        Err(error) => return Err(HandlerError::Body(error.to_string())),
    };
    let Ok(application) = serde_json::from_slice::<PublisherApplication>(&body) else {
        return Ok(text_response(
            StatusCode::BAD_REQUEST,
            "invalid publisher application JSON",
        ));
    };
    blocking(state, move |state| {
        let now = tilo_clock::unix_now();
        match applications::submit(
            &state.inner.storage,
            application,
            now,
            state.inner.proofs.as_ref(),
        ) {
            Ok(stored) => Ok(json_response(
                StatusCode::ACCEPTED,
                &ApplicationStatusBody::from(&stored),
            )),
            Err(ApplicationFailure::Rejected(rejection)) => Ok(text_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                &rejection.to_string(),
            )),
            Err(ApplicationFailure::QueueFull) => Ok(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "the registry is not taking publisher applications right now",
            )),
            Err(ApplicationFailure::Storage(error)) => Err(HandlerError::Storage(error)),
            Err(other) => Ok(text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &other.to_string(),
            )),
        }
    })
    .await
}

/// What anyone may learn about an application: not the applicant's contact.
#[derive(serde::Serialize)]
struct ApplicationStatusBody<'a> {
    pubkey_hex: &'a str,
    status: ApplicationStatus,
    namespaces: Vec<&'a str>,
    received_at_unix: u64,
    decided_at_unix: Option<u64>,
    reason: Option<&'a str>,
}

impl<'a> From<&'a StoredApplication> for ApplicationStatusBody<'a> {
    fn from(stored: &'a StoredApplication) -> Self {
        Self {
            pubkey_hex: &stored.application.pubkey_hex,
            status: stored.status,
            namespaces: stored
                .verified
                .iter()
                .map(|claim| claim.namespace.as_str())
                .collect(),
            received_at_unix: stored.received_at_unix,
            decided_at_unix: stored.decided_at_unix,
            reason: stored.reason.as_deref(),
        }
    }
}

fn handle_get_application(
    state: &AppState,
    pubkey_hex: &str,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    match state.inner.storage.load_publisher_application(pubkey_hex) {
        Ok(Some(stored)) => Ok(json_response(
            StatusCode::OK,
            &ApplicationStatusBody::from(&stored),
        )),
        Ok(None) => Ok(text_response(
            StatusCode::NOT_FOUND,
            "no application from this key",
        )),
        Err(StorageError::Parse(_)) => Ok(text_response(
            StatusCode::BAD_REQUEST,
            "expected a 32-byte hex public key",
        )),
        Err(error) => Err(HandlerError::Storage(error)),
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
    blocking(state, move |state| apply_moderation(state, &request)).await
}

fn apply_moderation(
    state: &AppState,
    request: &SignedModerationMutation,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let allowlist = state
        .inner
        .storage
        .load_allowlist()
        .map_err(HandlerError::Storage)?;
    let now = tilo_clock::unix_now();
    match state.inner.moderation_api.apply(request, now, &allowlist) {
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

fn handle_get_version_index(
    state: &AppState,
    name: &str,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let Some(digest) = name.strip_suffix(".json") else {
        return Ok(text_response(StatusCode::NOT_FOUND, "not found"));
    };
    match state
        .inner
        .storage
        .load_version_index(digest)
        .map_err(HandlerError::Storage)?
    {
        Some(bytes) => Ok(immutable_response_as(bytes, "application/json")),
        None => Ok(text_response(StatusCode::NOT_FOUND, "not found")),
    }
}

/// Which form of the snapshot a client asked for. Clients that do not ask get
/// the full snapshot, which every released client understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotFormat {
    Full,
    Compact,
}

impl SnapshotFormat {
    /// The form named by a `format=` query parameter; anything else is full.
    pub fn requested(query: Option<&str>) -> Self {
        let compact =
            query.is_some_and(|query| query.split('&').any(|pair| pair == "format=compact"));
        if compact { Self::Compact } else { Self::Full }
    }
}

/// The catalogue of the revision on disk, read and verified only when it is
/// a new one.
async fn published(state: &AppState) -> Result<Arc<Published>, HandlerError> {
    state
        .inner
        .catalogue
        .current(&state.inner.storage, &state.identity().owner_pubkey_hex)
        .await
        .map_err(HandlerError::Storage)
}

fn handle_get_entries(
    published: &Published,
    format: SnapshotFormat,
    if_none_match: Option<&str>,
) -> Response<Full<Bytes>> {
    // An authentic snapshot is served whether or not it has expired: every
    // client checks freshness itself, and only with the signed bytes can a
    // device tell "the catalogue expired" from "the registry is unreachable".
    // Refusing it here also failed readiness on every replica at once (the
    // snapshot is shared state), so the ingress answered "no available
    // server" for every route, `/registry` and uploads included.
    if format == SnapshotFormat::Compact
        && let Some(compact) = &published.compact
    {
        return revalidated_json(compact, if_none_match);
    }
    match &published.full {
        // Clients revalidate on every visit; the snapshot hash names the
        // exact signed bytes, so an unchanged catalogue costs a 304.
        FullSnapshot::Verified { encoded, .. } => revalidated_json(encoded, if_none_match),
        FullSnapshot::Refused => text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "published registry snapshot failed verification",
        ),
        FullSnapshot::Missing => text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no owner-signed snapshot has been published",
        ),
    }
}

/// `body` as JSON with `ETag: "<hash>"`, or an empty `304` when the client
/// already holds it.
fn revalidated_json(
    encoded: &crate::catalogue::Encoded,
    if_none_match: Option<&str>,
) -> Response<Full<Bytes>> {
    let etag = encoded
        .etag
        .as_ref()
        .and_then(|hash| header::HeaderValue::from_str(&format!("\"{hash}\"")).ok());
    let fresh = etag.as_ref().is_some_and(|etag| {
        if_none_match.is_some_and(|candidates| {
            candidates
                .split(',')
                .any(|candidate| candidate.trim().as_bytes() == etag.as_bytes())
        })
    });
    let mut response = if fresh {
        empty_response(StatusCode::NOT_MODIFIED)
    } else {
        json_bytes_response(StatusCode::OK, encoded.body.clone())
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    if let Some(etag) = etag {
        headers.insert(header::ETAG, etag);
    }
    response
}

fn handle_get_entry(published: &Published, package_id: &str) -> Response<Full<Bytes>> {
    match &published.full {
        FullSnapshot::Verified { encoded, packages } if packages.contains(package_id) => {
            // Return the complete signed object: a detached row would discard
            // the owner's authorization and inclusion proof.
            json_bytes_response(StatusCode::OK, encoded.body.clone())
        }
        FullSnapshot::Verified { .. } | FullSnapshot::Refused => {
            text_response(StatusCode::NOT_FOUND, "no such entry")
        }
        FullSnapshot::Missing => text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no owner-signed snapshot has been published",
        ),
    }
}

/// The signed headers of a pending-entry read.
struct PendingRead {
    publisher: Option<String>,
    signed_at: Option<String>,
    signature: Option<String>,
}

impl PendingRead {
    fn from_request(req: &Request<Incoming>) -> Self {
        let text = |name: &'static str| header_text(req, header::HeaderName::from_static(name));
        Self {
            publisher: text(registry_types::upload::UPLOAD_PUBLISHER_HEADER),
            signed_at: text(registry_types::pending::PENDING_TIME_HEADER),
            signature: text(registry_types::upload::UPLOAD_SIGNATURE_HEADER),
        }
    }
}

/// A publisher's own pending change for `package_id`. Anyone else, and a
/// publisher with nothing pending, gets the same 404.
fn handle_get_pending(
    state: &AppState,
    package_id: &str,
    read: &PendingRead,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    if !registry_types::namespace::valid_package_id(package_id) {
        return Ok(text_response(StatusCode::NOT_FOUND, "no such entry"));
    }
    let Some(publisher) = read
        .publisher
        .as_deref()
        .and_then(|hex_key| hex::decode(hex_key).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        return Ok(text_response(
            StatusCode::UNAUTHORIZED,
            "a pending read names its publisher key",
        ));
    };
    let now = tilo_clock::unix_now();
    let verified = read
        .signed_at
        .as_deref()
        .and_then(|text| text.parse::<u64>().ok())
        .zip(read.signature.as_deref())
        .is_some_and(|(signed_at, signature)| {
            registry_types::pending::verify_pending_read(
                &publisher, package_id, signed_at, signature, now,
            )
        });
    if !verified {
        return Ok(text_response(
            StatusCode::FORBIDDEN,
            "pending read signature does not verify",
        ));
    }
    let pending = state
        .inner
        .storage
        .load_pending(package_id)
        .map_err(HandlerError::Storage)?
        .filter(|pending| {
            hex::decode(&pending.change.entry.publisher_pubkey_hex)
                .is_ok_and(|owner| owner == publisher)
        });
    Ok(match pending {
        Some(pending) => json_response(StatusCode::OK, &pending.change),
        None => text_response(StatusCode::NOT_FOUND, "no pending entry of yours"),
    })
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

    blocking(state, move |state| verify_and_apply_change(state, change)).await
}

/// Verify a parsed change request against the allowlist and apply it.
fn verify_and_apply_change(
    state: &AppState,
    change: EntryChangeRequest,
) -> Result<Response<Full<Bytes>>, HandlerError> {
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
/// response so [`verify_and_apply_change`] just forwards.
fn apply_change(
    state: &AppState,
    change: EntryChangeRequest,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let publisher = publisher_row(state, &change.entry);
    if change.action == EntryAction::Add && !publisher.as_ref().is_some_and(|row| row.can_add_new) {
        return Ok(text_response(
            StatusCode::FORBIDDEN,
            "publisher not allowed to submit new entries",
        ));
    }
    if change.action != EntryAction::Delete
        && !publisher
            .as_ref()
            .is_some_and(|row| row.owns_package_id(&change.entry.package_id))
    {
        return Ok(text_response(
            StatusCode::FORBIDDEN,
            &format!(
                "package id {} is outside this publisher's verified namespaces",
                change.entry.package_id
            ),
        ));
    }
    let submitted_at = tilo_clock::unix_now();
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
        | ModerationError::PendingTimestampNotNewer { .. }
        | ModerationError::DuplicateVersionIdentity { .. }
        | ModerationError::VersionHistoryRemoved { .. }
        | ModerationError::VersionIdentityReused { .. }
        | ModerationError::KindChanged { .. } => Some(StatusCode::CONFLICT),
        ModerationError::UnsupportedPackageTarget { .. } => Some(StatusCode::BAD_REQUEST),
        ModerationError::BotProgram { .. } => Some(StatusCode::UNPROCESSABLE_ENTITY),
        _ => None,
    }
}

/// `PUT /uploads/<blake3>.tapp` or `PUT /uploads/<blake3>.program`: an
/// allowlisted publisher's signed upload of a package it signed, or of a bot
/// program, kept privately for review (`registry_types::upload`). Answers
/// `201` with the URL a version binding these bytes names.
async fn handle_put_upload(
    state: &AppState,
    name: &str,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let Some((digest, kind)) = parse_upload_name(name) else {
        return Ok(empty_response(StatusCode::NOT_FOUND));
    };
    let publisher = header_text(
        &req,
        header::HeaderName::from_static(registry_types::upload::UPLOAD_PUBLISHER_HEADER),
    );
    let signature = header_text(
        &req,
        header::HeaderName::from_static(registry_types::upload::UPLOAD_SIGNATURE_HEADER),
    );
    let Some(publisher) = publisher
        .and_then(|hex_key| hex::decode(hex_key).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        return Ok(text_response(
            StatusCode::UNAUTHORIZED,
            "an upload names its publisher key",
        ));
    };
    if !signature.is_some_and(|signature| {
        registry_types::upload::verify_upload(&publisher, &digest, &signature)
    }) {
        return Ok(text_response(
            StatusCode::FORBIDDEN,
            "upload signature does not verify",
        ));
    }
    let allowlisted = state
        .inner
        .storage
        .load_allowlist()
        .map_err(HandlerError::Storage)?
        .get(&publisher)
        .is_some();
    if !allowlisted {
        return Ok(text_response(
            StatusCode::FORBIDDEN,
            "publisher not in allowlist",
        ));
    }
    let limit = match kind {
        UploadKind::Package => usize::try_from(
            tilo_app_package::MAX_DESKTOP_APP_TAPP_BYTES.max(tilo_app_package::MAX_APP_TAPP_BYTES),
        )
        .unwrap_or(usize::MAX),
        UploadKind::Program => tilo_automation::program::MAX_PROGRAM_BYTES,
    };
    let body = match Limited::new(req.into_body(), limit).collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return Ok(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload is too large",
            ));
        }
        Err(error) => return Err(HandlerError::Body(error.to_string())),
    };
    blocking(state, move |state| match kind {
        UploadKind::Package => store_upload(state, &publisher, &digest, &body),
        UploadKind::Program => store_program_upload(state, &digest, &body),
    })
    .await
}

#[derive(Clone, Copy)]
enum UploadKind {
    Package,
    Program,
}

fn parse_upload_name(name: &str) -> Option<(String, UploadKind)> {
    let (digest, extension) = name.rsplit_once('.')?;
    let kind = match extension {
        "tapp" => UploadKind::Package,
        "program" => UploadKind::Program,
        _ => return None,
    };
    (digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| (digest.to_ascii_lowercase(), kind))
}

/// Keep a bot program for review. Only the canonical encoding of a Shortcut
/// that passes the structural checks is kept: the entry signs these exact
/// bytes, so the registry never re-encodes them.
fn store_program_upload(
    state: &AppState,
    digest: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>, HandlerError> {
    if !blake3::hash(body).to_hex().eq_ignore_ascii_case(digest) {
        return Ok(text_response(
            StatusCode::BAD_REQUEST,
            "upload does not hash to its name",
        ));
    }
    let program = match tilo_automation::program::parse_program(body) {
        Ok(program) => program,
        Err(error) => {
            return Ok(text_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("not a canonical bot program: {error}"),
            ));
        }
    };
    if let Err(errors) = tilo_automation::program::check_structure(&program) {
        let reasons: Vec<String> = errors.iter().map(ToString::to_string).collect();
        return Ok(text_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &reasons.join("; "),
        ));
    }
    state
        .inner
        .storage
        .save_program_upload(body)
        .map_err(HandlerError::Storage)?;
    Ok(json_response(
        StatusCode::CREATED,
        &serde_json::json!({
            "blake3": digest,
            "url": crate::moderation::program_url(&state.inner.identity.base_url, digest),
        }),
    ))
}

fn store_upload(
    state: &AppState,
    publisher: &[u8; 32],
    digest: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>, HandlerError> {
    if !blake3::hash(body).to_hex().eq_ignore_ascii_case(digest) {
        return Ok(text_response(
            StatusCode::BAD_REQUEST,
            "upload does not hash to its name",
        ));
    }
    let signed_by_publisher = app_registry::tapp::load(body).is_ok_and(|meta| {
        app_registry::verify_bytes(body, meta.package_id, *publisher).is_ok()
            && tilo_app_package::app_size_limit(&meta.manifest)
                .is_none_or(|limit| body.len() as u64 <= limit)
    });
    if !signed_by_publisher {
        return Ok(text_response(
            StatusCode::BAD_REQUEST,
            "upload is not a package this publisher signed, within its size limit",
        ));
    }
    state
        .inner
        .storage
        .save_upload(body)
        .map_err(HandlerError::Storage)?;
    Ok(json_response(
        StatusCode::CREATED,
        &serde_json::json!({
            "blake3": digest,
            "url": crate::moderation::artifact_url(&state.inner.identity.base_url, digest),
        }),
    ))
}

fn publisher_row(state: &AppState, entry: &RegistryEntry) -> Option<crate::PublisherRow> {
    let key: [u8; 32] = hex::decode(&entry.publisher_pubkey_hex)
        .ok()?
        .try_into()
        .ok()?;
    state
        .inner
        .storage
        .load_allowlist()
        .ok()
        .and_then(|allowlist| allowlist.get(&key).cloned())
}

/// A payload blob of a reviewed package, by BLAKE3 hash, so a client that
/// downloaded a thin package fetches only the blobs it lacks. Content
/// addressing makes the response self-verifying; clients check every blob.
fn handle_get_blob(state: &AppState, name: &str) -> Result<Response<Full<Bytes>>, HandlerError> {
    if let Some(blake3_hex) = name.strip_suffix(tilo_app_package::compressed::SUFFIX) {
        return Ok(
            match state
                .inner
                .storage
                .load_payload_twin(blake3_hex)
                .map_err(HandlerError::Storage)?
            {
                Some(twin) => immutable_response_as(twin, tilo_app_package::compressed::MEDIA_TYPE),
                None => text_response(StatusCode::NOT_FOUND, "not found"),
            },
        );
    }
    if let Some(blake3_hex) = name.strip_suffix(tilo_app_package::chunks::CHUNKS_SUFFIX) {
        return Ok(
            match state
                .inner
                .storage
                .load_chunk_list(blake3_hex)
                .map_err(HandlerError::Storage)?
            {
                Some(list) => immutable_response_as(list.encode(), "application/json"),
                None => text_response(StatusCode::NOT_FOUND, "not found"),
            },
        );
    }
    let blake3_hex = name;
    match state
        .inner
        .storage
        .load_payload_blob(blake3_hex)
        .map_err(HandlerError::Storage)?
    {
        Some(bytes) => {
            // `WebAssembly.compileStreaming` accepts only `application/wasm`,
            // and a blob's name says nothing about its use, so the type comes
            // from the WebAssembly 1.0 binary header.
            let content_type = if bytes.starts_with(b"\0asm\x01\0\0\0") {
                "application/wasm"
            } else {
                "application/octet-stream"
            };
            Ok(immutable_response_as(bytes, content_type))
        }
        None => Ok(text_response(StatusCode::NOT_FOUND, "not found")),
    }
}

/// An imported package, or its compressed twin.
fn handle_get_artifact(
    state: &AppState,
    name: &str,
) -> Result<Response<Full<Bytes>>, HandlerError> {
    let storage = &state.inner.storage;
    match name.strip_suffix(tilo_app_package::compressed::SUFFIX) {
        Some(raw) => {
            content_addressed(raw, |digest| storage.load_artifact_twin(digest)).map(compressed_twin)
        }
        None => content_addressed(name, |digest| storage.load_artifact(digest)),
    }
}

/// Relabel a content-addressed response as the compressed twin it carries.
/// The body is a zstd frame, not an encoding of the response: no
/// `Content-Encoding`, so nothing between here and the client decodes it.
fn compressed_twin(mut response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    if response.status() == StatusCode::OK {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static(tilo_app_package::compressed::MEDIA_TYPE),
        );
    }
    response
}

/// Content-addressed bytes: the URL names the content, so every cache may
/// keep the response forever.
fn immutable_response(bytes: Vec<u8>) -> Response<Full<Bytes>> {
    immutable_response_as(bytes, "application/octet-stream")
}

fn immutable_response_as(bytes: Vec<u8>, content_type: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap_or_else(|_| text_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
}

fn empty_response(code: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = code;
    response
}

fn json_response<T: serde::Serialize>(code: StatusCode, body: &T) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    json_bytes_response(code, Bytes::from(bytes))
}

#[allow(
    clippy::expect_used,
    reason = "the status is an already-validated `StatusCode` and the header is a hardcoded \
              constant, so the builder cannot actually fail here"
)]
fn json_bytes_response(code: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(body))
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
    #[error("blocking storage task: {0}")]
    Blocking(std::io::Error),
    #[error("moderation: {0}")]
    Moderation(ModerationError),
}
