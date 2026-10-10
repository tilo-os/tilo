//! The page's certified peer: `tilo_browser_peer` behind wasm-bindgen for
//! `web/peer.mjs`, reaching the host over HTTPS only.
//!
//! The device key stays in custody (`web/device-custody.mjs`): either a
//! non-extractable `WebCrypto` Ed25519 key that signs through a JavaScript
//! callback, or — where `WebCrypto` lacks Ed25519 — a seed unsealed from
//! `IndexedDB` straight into this module, which keeps it only in wasm
//! memory. Nothing here generates or holds a user root: a page that holds
//! one (`web/root-identity.mjs`) lends it as a signer, the same shape as
//! custody, to [`issue_device_cert`] and [`certify_with_root`].
//!
//! The peer record (certificates and the commit outbox) is kept in the
//! profile's durable store when the page names a profile
//! ([`DurablePeerStore`]), committed to `IndexedDB` with everything else.
//! Once started, the certified user becomes the toolkit's local identity
//! (`ui::local_identity`), so mounts attribute edits to that person.
//!
//! HTTP goes through the page's `post(path, session, body)` (its `fetch`).
//! The live plane runs in JavaScript over WebTransport (`web/live-transport.mjs`,
//! with the HTTPS fallback); this module issues its grant
//! ([`WasmPeer::live_grant`]) and encodes and validates its frames with the
//! host's canonical codec. Documents reach the desktop's
//! `CollaborationManager` through [`WasmPeer::collab_transport`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Function, Promise, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use tilo_browser_peer::{
    DeviceSigner, DocumentRoute, FailureSubject, Host, HttpClient, HttpFuture, LocalState,
    PeerConfig, PeerEvent, PeerEvents, PeerHandle, PeerRecord, PeerSnapshot, PeerStore, SignError,
    SignFuture,
};
use tilo_relay_protocol::live::{
    DragPhase, LiveAnchor, LiveFrame, LiveObject, LiveParticipant, LiveRecord, OpsFrame,
    ParticipantKind, PresenceFrame, PreviewOp,
};
use tilo_user_identity::{DeviceCert, RevocationList};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

fn js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

/// The page store without a surface; a desktop surface; its asset.
fn failure_subject(surface: Option<String>, asset: Option<String>) -> FailureSubject {
    match (surface, asset) {
        (None, _) => FailureSubject::Storage,
        (Some(surface), None) => FailureSubject::Surface(surface),
        (Some(surface), Some(asset)) => FailureSubject::Asset { surface, asset },
    }
}

/// A device signer backed by page custody.
enum PageSigner {
    /// `{publicKey: Uint8Array(32), sign(message): Promise<Uint8Array(64)>}`
    /// over a non-extractable `WebCrypto` key.
    WebCrypto {
        public_key: [u8; 32],
        sign: Function,
        this: JsValue,
    },
    /// A seed the page unsealed into this module; zeroized on drop by
    /// `ed25519-dalek`.
    Seed(ed25519_dalek::SigningKey),
}

impl PageSigner {
    fn from_js(custody: &JsValue) -> Result<Self, JsError> {
        let seed = Reflect::get(custody, &"seed".into()).map_err(|_| js_error("custody"))?;
        if !seed.is_undefined() && !seed.is_null() {
            let seed: [u8; 32] = Uint8Array::new(&seed)
                .to_vec()
                .try_into()
                .map_err(|_| js_error("device seed must be 32 bytes"))?;
            return Ok(Self::Seed(ed25519_dalek::SigningKey::from_bytes(&seed)));
        }
        let public_key: [u8; 32] = Uint8Array::new(
            &Reflect::get(custody, &"publicKey".into()).map_err(|_| js_error("custody"))?,
        )
        .to_vec()
        .try_into()
        .map_err(|_| js_error("device public key must be 32 bytes"))?;
        let sign: Function = Reflect::get(custody, &"sign".into())
            .map_err(|_| js_error("custody"))?
            .dyn_into()
            .map_err(|_| js_error("custody.sign must be a function"))?;
        Ok(Self::WebCrypto {
            public_key,
            sign,
            this: custody.clone(),
        })
    }
}

impl DeviceSigner for PageSigner {
    fn device_public_key(&self) -> [u8; 32] {
        match self {
            Self::WebCrypto { public_key, .. } => *public_key,
            Self::Seed(key) => key.verifying_key().to_bytes(),
        }
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        match self {
            Self::Seed(key) => DeviceSigner::sign(key, message),
            Self::WebCrypto { sign, this, .. } => {
                let called = sign.call1(this, &Uint8Array::from(message.as_slice()).into());
                Box::pin(async move {
                    let promise: Promise = called
                        .map_err(|_| SignError("custody refused to sign".into()))?
                        .dyn_into()
                        .map_err(|_| SignError("custody.sign must return a promise".into()))?;
                    let signature = JsFuture::from(promise)
                        .await
                        .map_err(|_| SignError("custody signing failed".into()))?;
                    Uint8Array::new(&signature)
                        .to_vec()
                        .try_into()
                        .map_err(|_| SignError("signature must be 64 bytes".into()))
                })
            }
        }
    }
}

/// Where a profile keeps its peers' records, one per user root. Changing it
/// orphans stored outboxes.
const PEER_PACKAGE: &str = "so.tilo.browser-peer";

/// The peer record in the profile's durable store. A save is accepted by the
/// store and committed by the page's durable host with the profile's other
/// writes; a failed commit keeps the key queued for retry, and the host's
/// `flush` reports it.
struct DurablePeerStore {
    store: Rc<durable_store::MemoryStore>,
    key: durable_store::StorageKey,
    /// Channel membership and the edits held for it, beside the record.
    channels_key: durable_store::StorageKey,
    /// Seals the member state in `channels_key`. The page keeps it wrapped
    /// by a non-extractable `WebCrypto` key (`channel-vault.mjs`), never in
    /// this store.
    channel_vault: [u8; 32],
}

thread_local! {
    static NEXT_PEER: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

impl DurablePeerStore {
    fn open(
        namespace: &str,
        user_public_key: &[u8; 32],
        channel_vault: [u8; 32],
    ) -> Result<Self, JsError> {
        #[derive(Deserialize)]
        struct Namespace {
            profile: String,
            device: String,
        }
        let namespace: Namespace = serde_json::from_str(namespace).map_err(js_error)?;
        let store = crate::storage::store(&namespace.profile, &namespace.device)
            .map_err(|_| js_error("open the profile's storage before starting its peer"))?;
        Ok(Self {
            store: Rc::clone(&store),
            key: durable_store::StorageKey::record(
                PEER_PACKAGE,
                crate::storage::PROFILE_WORKSPACE,
                hex::encode(user_public_key),
            ),
            channels_key: durable_store::StorageKey::record(
                PEER_PACKAGE,
                crate::storage::PROFILE_WORKSPACE,
                format!("{}.channels", hex::encode(user_public_key)),
            ),
            channel_vault,
        })
    }
}

impl PeerStore for DurablePeerStore {
    fn load(&self) -> Option<PeerRecord> {
        use durable_store::DurableStore as _;
        let stored = self.store.read(&self.key).ok()??;
        postcard::from_bytes(&stored.bytes).ok()
    }

    fn save(&self, record: &PeerRecord) -> Result<(), String> {
        use durable_store::DurableStore as _;
        let bytes = postcard::to_stdvec(record).map_err(|error| error.to_string())?;
        self.store
            .write(&self.key, durable_store::WriteOp::Replace(bytes), None)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        use durable_store::DurableStore as _;
        Some(self.store.read(&self.channels_key).ok()??.bytes)
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        use durable_store::DurableStore as _;
        self.store
            .write(
                &self.channels_key,
                durable_store::WriteOp::Replace(bytes.to_vec()),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.channel_vault)
    }
}

/// The peer record in page storage: loaded before start, saved through a
/// callback that writes it to `IndexedDB`.
struct PageStore {
    current: RefCell<Option<PeerRecord>>,
    save: Function,
}

impl PeerStore for PageStore {
    fn load(&self) -> Option<PeerRecord> {
        self.current.borrow().clone()
    }

    fn save(&self, record: &PeerRecord) -> Result<(), String> {
        let bytes = postcard::to_stdvec(record).map_err(|error| error.to_string())?;
        *self.current.borrow_mut() = Some(record.clone());
        self.save
            .call1(&JsValue::NULL, &Uint8Array::from(bytes.as_slice()).into())
            .map(|_| ())
            .map_err(|_| "the page store refused the write".to_string())
    }
}

/// HTTP through the page: `post(path, session, body)` resolves to
/// `{status, body}`, rejecting only when the host was not reached.
struct PageHttp(Function);

impl HttpClient for PageHttp {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        let called = self.0.call3(
            &JsValue::NULL,
            &path.into(),
            &session.map_or(JsValue::NULL, JsValue::from),
            &Uint8Array::from(body.as_slice()).into(),
        );
        Box::pin(async move {
            let promise: Promise = called
                .map_err(|_| "post threw".to_string())?
                .dyn_into()
                .map_err(|_| "post must return a promise".to_string())?;
            let answer = JsFuture::from(promise)
                .await
                .map_err(|error| error.as_string().unwrap_or_else(|| "fetch failed".into()))?;
            let status = Reflect::get(&answer, &"status".into())
                .ok()
                .and_then(|value| value.as_f64())
                .ok_or("post answer has no status")?;
            let body =
                Reflect::get(&answer, &"body".into()).map_err(|_| "post answer has no body")?;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an HTTP status is a small positive integer"
            )]
            Ok((status as u16, Uint8Array::new(&body).to_vec()))
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectConfig {
    cert: DeviceCert,
    host_cert: DeviceCert,
    /// The person's chosen name, for history and presence; empty for none.
    #[serde(default)]
    display_name: String,
}

/// The certified user as the toolkit's local identity.
fn install_identity(cert: &DeviceCert, display_name: String) {
    let key = cert.user_public_key;
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: key,
            display_name,
            avatar_color: [key[0], key[1], key[2]],
        },
    ))));
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Certified<'a> {
    cert: &'a DeviceCert,
    host_cert: &'a DeviceCert,
}

async fn post_decoded<T: serde::de::DeserializeOwned>(
    post: &PageHttp,
    route: &str,
    body: Vec<u8>,
) -> Result<T, JsError> {
    let path = format!("{}{route}", tilo_relay_protocol::web::API_PREFIX);
    let (status, bytes) = post.post(&path, None, body).await.map_err(js_error)?;
    if status != 200 {
        return Err(js_error(format!("host answered {status}")));
    }
    postcard::from_bytes(&bytes).map_err(js_error)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsHostInfo {
    host_cert: DeviceCert,
    webtransport_port: u16,
    certificate_sha256: Option<String>,
}

/// What the host publishes before certification: its certificate, the
/// WebTransport port and, for a development certificate, its SHA-256 for
/// `serverCertificateHashes`. Resolves to JSON.
///
/// # Errors
///
/// Rejects when the host is unreachable or answers badly.
#[wasm_bindgen(js_name = hostInfo)]
pub async fn host_info(post: Function) -> Result<String, JsError> {
    let info: tilo_relay_protocol::web::HostInfo =
        post_decoded(&PageHttp(post), "/host", Vec::new()).await?;
    serde_json::to_string(&JsHostInfo {
        host_cert: info.host_cert,
        webtransport_port: info.webtransport_port,
        certificate_sha256: info.certificate_sha256,
    })
    .map_err(js_error)
}

/// Certify this page's device key through the host's HTTPS origin. Calls
/// `onPairingCode(code)` once the host queued the request. Resolves to
/// `{cert, hostCert}` JSON for [`WasmPeer::start`].
///
/// # Errors
///
/// Rejects when the request is denied, expires, or the host misbehaves.
#[wasm_bindgen(js_name = certifyDevice)]
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "the page's HTTP client stays on its one thread"
)]
pub async fn certify_device(
    post: Function,
    user_public_key_hex: String,
    label: String,
    custody: JsValue,
    on_pairing_code: Function,
) -> Result<String, JsError> {
    let signer = PageSigner::from_js(&custody)?;
    let user: [u8; 32] = hex::decode(user_public_key_hex)
        .map_err(js_error)?
        .try_into()
        .map_err(|_| js_error("user public key must be 32 bytes"))?;
    let host = Host::Https(Arc::new(PageHttp(post)));
    let (cert, host_cert) = tilo_browser_peer::certify(&host, user, &label, &signer, |code| {
        let _ = on_pairing_code.call1(&JsValue::NULL, &code.into());
    })
    .await
    .map_err(js_error)?;
    serde_json::to_string(&Certified {
        cert: &cert,
        host_cert: &host_cert,
    })
    .map_err(js_error)
}

fn device_key(text: &str) -> Result<[u8; 32], JsError> {
    hex::decode(text)
        .map_err(js_error)?
        .try_into()
        .map_err(|_| js_error("device public key must be 32 bytes"))
}

/// Certify `devicePublicKeyHex` with the user root the page holds (`root`:
/// `{publicKey, sign}` from `root-identity.mjs`) as `role`: `browser` (a
/// member, 30 days), `node` (hosts and governs the person's workspaces,
/// admits no devices) or `session` (the session beside a node that runs
/// the person's apps and agents: a member, one year). Resolves to the
/// certificate JSON.
///
/// # Errors
///
/// Rejects for an unknown role, the root's own key, or a failed signature.
#[wasm_bindgen(js_name = issueDeviceCert)]
pub async fn issue_device_cert(
    root: JsValue,
    device_public_key_hex: String,
    label: String,
    role: String,
) -> Result<String, JsError> {
    let role = match role.as_str() {
        "browser" => tilo_browser_peer::IssuedRole::Browser,
        "node" => tilo_browser_peer::IssuedRole::Node,
        "session" => tilo_browser_peer::IssuedRole::Session,
        _ => return Err(js_error("role must be browser, node or session")),
    };
    let root = PageSigner::from_js(&root)?;
    let cert = tilo_browser_peer::issue_device_cert(
        &root,
        device_key(&device_public_key_hex)?,
        &label,
        role,
    )
    .await
    .map_err(js_error)?;
    serde_json::to_string(&cert).map_err(js_error)
}

fn join_reply_json(reply: tilo_relay_protocol::rooms::JoinReply) -> serde_json::Value {
    use tilo_relay_protocol::rooms::JoinReply;
    match reply {
        JoinReply::Invitation {
            room,
            workspace_id,
            name,
            role,
            expires_at_ns,
            inviter,
        } => serde_json::json!({
            "kind": "invitation", "room": room, "workspaceId": workspace_id.to_string(),
            "name": name, "role": role, "expiresAtNs": expires_at_ns.to_string(),
            "inviter": inviter.map(|inviter| serde_json::json!({
                "name": inviter.name, "userPublicKey": hex::encode(inviter.user_public_key),
            })),
        }),
        JoinReply::Challenge { .. } => serde_json::json!({"kind": "challenge"}),
        JoinReply::Waiting => serde_json::json!({"kind": "waiting"}),
        JoinReply::Admitted {
            room,
            workspace_id,
            name,
            role,
        } => serde_json::json!({
            "kind": "admitted", "room": room, "workspaceId": workspace_id.to_string(),
            "name": name, "role": role,
        }),
        JoinReply::Declined => serde_json::json!({"kind": "declined"}),
        JoinReply::Refused(reason) => serde_json::json!({"kind": "refused", "reason": reason}),
    }
}

fn request_id(text: &str) -> Result<[u8; 32], JsError> {
    hex::decode(text)
        .map_err(js_error)?
        .try_into()
        .map_err(|_| js_error("request id must be 32 bytes"))
}

/// What `invitation` (the signed invitation's bytes) offers at the host
/// `post` reaches: `{kind: 'invitation', room, workspaceId, name, role,
/// expiresAtNs}`, or `{kind: 'refused', reason}`. Resolves to JSON.
///
/// # Errors
///
/// Rejects when the host is not reached or answers badly.
#[wasm_bindgen(js_name = inspectInvitation)]
pub async fn inspect_invitation(post: Function, invitation: Vec<u8>) -> Result<String, JsError> {
    let reply = tilo_browser_peer::invite::join(
        &PageHttp(post),
        &tilo_relay_protocol::rooms::JoinStep::Inspect { invitation },
    )
    .await
    .map_err(js_error)?;
    Ok(join_reply_json(reply).to_string())
}

/// Ask to join with `invitation` as this page's device (`custody`),
/// certified by the person's own root (`root`) as `label`, named `name` to
/// the room's owner, under request `requestIdHex`; with `admissionToken`,
/// the one-time token of the owner's pre-admission, the host admits it at
/// once. Resolves to JSON
/// `{reply, cert}`: the host's answer (`waiting`, `admitted`, `declined`,
/// `refused`) and the device certificate the page then connects with.
///
/// # Errors
///
/// Rejects when the host is not reached, answers badly, or a key does not
/// sign.
#[wasm_bindgen(js_name = askToJoin)]
pub async fn ask_to_join(
    post: Function,
    invitation: Vec<u8>,
    root: JsValue,
    custody: JsValue,
    request_id_hex: String,
    name: String,
    label: String,
    admission_token: Option<Vec<u8>>,
) -> Result<String, JsError> {
    let root = PageSigner::from_js(&root)?;
    let signer = PageSigner::from_js(&custody)?;
    let cert = tilo_browser_peer::invite::guest_certificate(
        &root,
        signer.device_public_key(),
        &label,
        ui::time::unix_now_ns(),
    )
    .await
    .map_err(js_error)?;
    let request_id = request_id(&request_id_hex)?;
    let reply = match admission_token {
        None => {
            tilo_browser_peer::invite::ask_to_join(
                &PageHttp(post),
                &invitation,
                &cert,
                &signer,
                request_id,
                &name,
            )
            .await
        }
        Some(token) => {
            tilo_browser_peer::invite::ask_to_join_pre_admitted(
                &PageHttp(post),
                &invitation,
                &cert,
                &signer,
                request_id,
                &name,
                &token,
            )
            .await
        }
    }
    .map_err(js_error)?;
    Ok(serde_json::json!({"reply": join_reply_json(reply), "cert": cert}).to_string())
}

/// Where join request `requestIdHex` of device `devicePublicKeyHex` stands:
/// JSON as [`ask_to_join`]'s `reply`.
///
/// # Errors
///
/// Rejects when the host is not reached or answers badly.
#[wasm_bindgen(js_name = joinStatus)]
pub async fn join_status(
    post: Function,
    request_id_hex: String,
    device_public_key_hex: String,
) -> Result<String, JsError> {
    let reply = tilo_browser_peer::invite::join(
        &PageHttp(post),
        &tilo_relay_protocol::rooms::JoinStep::Status {
            request_id: request_id(&request_id_hex)?,
            device_public_key: device_key(&device_public_key_hex)?,
        },
    )
    .await
    .map_err(js_error)?;
    Ok(join_reply_json(reply).to_string())
}

/// The canonical encoding of a certificate (JSON in): what a native device
/// keeps on disk.
///
/// # Errors
///
/// Throws for JSON that is not a certificate.
#[wasm_bindgen(js_name = deviceCertCanonical)]
pub fn device_cert_canonical(cert_json: &str) -> Result<Vec<u8>, JsError> {
    let cert: DeviceCert = serde_json::from_str(cert_json).map_err(js_error)?;
    tilo_user_identity::canonical::canonical_bytes(&cert).map_err(js_error)
}

/// Certify this page's device key (`custody`) with the user root the page
/// holds, for the host at `post`, whose certificate must be a host of that
/// root. Resolves to `{cert, hostCert}` JSON, as [`certify_device`] does.
///
/// # Errors
///
/// Rejects when the host is unreachable or not a host of this root.
#[wasm_bindgen(js_name = certifyWithRoot)]
pub async fn certify_with_root(
    post: Function,
    root: JsValue,
    custody: JsValue,
    label: String,
) -> Result<String, JsError> {
    let root = PageSigner::from_js(&root)?;
    let signer = PageSigner::from_js(&custody)?;
    let (cert, host_cert) =
        tilo_browser_peer::certify_with_root(&PageHttp(post), &root, &signer, &label)
            .await
            .map_err(js_error)?;
    serde_json::to_string(&Certified {
        cert: &cert,
        host_cert: &host_cert,
    })
    .map_err(js_error)
}

// ---- Live frames as the JavaScript shape of `web/live-presence.mjs` -------

#[derive(Serialize, Deserialize)]
struct JsRecord {
    app: String,
    record: String,
}

#[derive(Serialize, Deserialize)]
struct JsSelection {
    anchor: String,
    head: String,
}

#[derive(Serialize, Deserialize)]
struct JsObject {
    record: Option<JsRecord>,
    id: String,
}

#[derive(Serialize, Deserialize)]
struct JsDrag {
    object: JsObject,
    phase: String,
    position: [f32; 2],
    size: Option<[f32; 2]>,
}

/// `{kind: 'person' | 'agent', label, color: '#rrggbb', activity}`.
#[derive(Serialize, Deserialize)]
struct JsParticipant {
    kind: String,
    label: String,
    color: String,
    #[serde(default)]
    activity: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsPresence {
    seq: u64,
    sent_at_ms: u64,
    focus: Option<JsRecord>,
    pointer: Option<[f32; 2]>,
    selection: Option<JsSelection>,
    drag: Option<JsDrag>,
    #[serde(default)]
    participant: Option<JsParticipant>,
}

fn participant_from_js(value: JsParticipant) -> Result<LiveParticipant, JsError> {
    let kind = match value.kind.as_str() {
        "person" => ParticipantKind::Person,
        "agent" => ParticipantKind::Agent,
        _ => return Err(js_error("participant kind")),
    };
    let color: [u8; 3] = value
        .color
        .strip_prefix('#')
        .and_then(|hex| hex::decode(hex).ok())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| js_error("participant colour must be #rrggbb"))?;
    Ok(LiveParticipant {
        kind,
        label: value.label,
        color,
        activity: value.activity,
    })
}

fn participant_to_js(value: LiveParticipant) -> JsParticipant {
    JsParticipant {
        kind: match value.kind {
            ParticipantKind::Person => "person",
            ParticipantKind::Agent => "agent",
        }
        .into(),
        label: value.label,
        color: tilo_relay_protocol::agents::color_hex(value.color),
        activity: value.activity,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum JsPreviewOp {
    Insert { at: String, text: String },
    Delete { at: String, len: u32 },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsDecoded {
    Presence {
        frame: JsPresence,
    },
    Preview {
        record: JsRecord,
        seq: u64,
        ops: Vec<JsPreviewOp>,
    },
    PreviewEnd {
        record: JsRecord,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsDurable {
    Durable { record: JsRecord },
}

fn record_to_js(record: LiveRecord) -> JsRecord {
    JsRecord {
        app: record.app,
        record: record.record,
    }
}

fn record_from_js(record: JsRecord) -> LiveRecord {
    LiveRecord {
        app: record.app,
        record: record.record,
    }
}

fn anchor(text: &str) -> Result<LiveAnchor, JsError> {
    Ok(LiveAnchor {
        cursor: hex::decode(text).map_err(js_error)?,
    })
}

fn phase_from_js(text: &str) -> Result<DragPhase, JsError> {
    Ok(match text {
        "start" => DragPhase::Start,
        "move" => DragPhase::Move,
        "end" => DragPhase::End,
        "cancel" => DragPhase::Cancel,
        _ => return Err(js_error("drag phase")),
    })
}

fn phase_to_js(phase: DragPhase) -> String {
    match phase {
        DragPhase::Start => "start",
        DragPhase::Move => "move",
        DragPhase::End => "end",
        DragPhase::Cancel => "cancel",
    }
    .into()
}

fn presence_from_js(value: JsPresence) -> Result<PresenceFrame, JsError> {
    Ok(PresenceFrame {
        seq: value.seq,
        sent_at_ms: value.sent_at_ms,
        focus: value.focus.map(record_from_js),
        pointer: value.pointer,
        selection: value
            .selection
            .map(|selection| {
                Ok::<_, JsError>((anchor(&selection.anchor)?, anchor(&selection.head)?))
            })
            .transpose()?,
        drag: value
            .drag
            .map(|drag| {
                Ok::<_, JsError>((
                    LiveObject {
                        record: drag.object.record.map(record_from_js),
                        id: hex::decode(drag.object.id).map_err(js_error)?,
                    },
                    phase_from_js(&drag.phase)?,
                    drag.position,
                    drag.size,
                ))
            })
            .transpose()?,
        participant: value.participant.map(participant_from_js).transpose()?,
    })
}

fn presence_to_js(frame: PresenceFrame) -> JsPresence {
    JsPresence {
        seq: frame.seq,
        sent_at_ms: frame.sent_at_ms,
        focus: frame.focus.map(record_to_js),
        pointer: frame.pointer,
        selection: frame.selection.map(|(anchor, head)| JsSelection {
            anchor: hex::encode(anchor.cursor),
            head: hex::encode(head.cursor),
        }),
        drag: frame.drag.map(|(object, phase, position, size)| JsDrag {
            object: JsObject {
                record: object.record.map(record_to_js),
                id: hex::encode(object.id),
            },
            phase: phase_to_js(phase),
            position,
            size,
        }),
        participant: frame.participant.map(participant_to_js),
    }
}

fn ops_to_js(ops: Vec<PreviewOp>) -> Vec<JsPreviewOp> {
    ops.into_iter()
        .map(|op| match op {
            PreviewOp::Insert { at, text } => JsPreviewOp::Insert {
                at: hex::encode(at.cursor),
                text,
            },
            PreviewOp::Delete { at, len } => JsPreviewOp::Delete {
                at: hex::encode(at.cursor),
                len,
            },
        })
        .collect()
}

fn ops_from_js(ops: Vec<JsPreviewOp>) -> Result<Vec<PreviewOp>, JsError> {
    ops.into_iter()
        .map(|op| {
            Ok(match op {
                JsPreviewOp::Insert { at, text } => PreviewOp::Insert {
                    at: anchor(&at)?,
                    text,
                },
                JsPreviewOp::Delete { at, len } => PreviewOp::Delete {
                    at: anchor(&at)?,
                    len,
                },
            })
        })
        .collect()
}

fn workspace(room: &str) -> Result<u64, JsError> {
    tilo_relay_protocol::room_workspace_id(room).map_err(|_| js_error("room must be a uuid"))
}

/// Encode one presence value from `live-presence.mjs` (JSON, with its own
/// `seq`) as the canonical live frame the host relays.
///
/// # Errors
///
/// Throws for a value outside the wire bounds.
#[wasm_bindgen(js_name = encodeLivePresence)]
pub fn encode_live_presence(value_json: &str) -> Result<Vec<u8>, JsError> {
    let frame = presence_from_js(serde_json::from_str(value_json).map_err(js_error)?)?;
    LiveFrame::Presence(frame).encode().map_err(js_error)
}

/// Encode preview operations (JSON array) for a record (JSON).
///
/// # Errors
///
/// Throws for operations outside the wire bounds.
#[wasm_bindgen(js_name = encodeLivePreview)]
pub fn encode_live_preview(
    record_json: &str,
    seq: u64,
    ops_json: &str,
) -> Result<Vec<u8>, JsError> {
    let record = record_from_js(serde_json::from_str(record_json).map_err(js_error)?);
    let ops = ops_from_js(serde_json::from_str(ops_json).map_err(js_error)?)?;
    LiveFrame::Ops(OpsFrame::Preview { seq, record, ops })
        .encode()
        .map_err(js_error)
}

/// Encode the end of this device's previews for a record (JSON).
///
/// # Errors
///
/// Throws for a malformed record.
#[wasm_bindgen(js_name = encodeLivePreviewEnd)]
pub fn encode_live_preview_end(record_json: &str) -> Result<Vec<u8>, JsError> {
    let record = record_from_js(serde_json::from_str(record_json).map_err(js_error)?);
    LiveFrame::Ops(OpsFrame::End { record })
        .encode()
        .map_err(js_error)
}

/// Decode and validate one relayed live frame to JSON
/// (`{kind: 'presence'|'preview'|'previewEnd', …}`).
///
/// # Errors
///
/// Throws for a frame that is not canonical or out of bounds.
#[wasm_bindgen(js_name = decodeLiveFrame)]
pub fn decode_live_frame(frame: &[u8]) -> Result<String, JsError> {
    let decoded = match LiveFrame::decode(frame).map_err(js_error)? {
        LiveFrame::Presence(presence) => JsDecoded::Presence {
            frame: presence_to_js(presence),
        },
        LiveFrame::Ops(OpsFrame::Preview { seq, record, ops }) => JsDecoded::Preview {
            record: record_to_js(record),
            seq,
            ops: ops_to_js(ops),
        },
        LiveFrame::Ops(OpsFrame::End { record }) => JsDecoded::PreviewEnd {
            record: record_to_js(record),
        },
        LiveFrame::Ops(OpsFrame::Semantic { .. }) => {
            return Err(JsError::new(
                "semantic activity requires an authenticated record consumer",
            ));
        }
    };
    serde_json::to_string(&decoded).map_err(js_error)
}

/// The `ops/<record digest>` track name of a record (JSON).
///
/// # Errors
///
/// Throws for a malformed record.
#[wasm_bindgen(js_name = liveRecordTrack)]
pub fn live_record_track(record_json: &str) -> Result<String, JsError> {
    let record = record_from_js(serde_json::from_str(record_json).map_err(js_error)?);
    Ok(format!("ops/{}", hex::encode(record.digest())))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsGrant {
    token: String,
    room: String,
    publisher: String,
    expires_at_ns: u64,
}

fn hex32(text: &str, what: &str) -> Result<[u8; 32], JsError> {
    hex::decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| js_error(format!("{what} must be 32 bytes of hex")))
}

/// `{publish, subscribe}`.
#[derive(Serialize, Deserialize)]
struct JsDirections {
    publish: bool,
    subscribe: bool,
}

impl From<tilo_relay_protocol::voice::Directions> for JsDirections {
    fn from(value: tilo_relay_protocol::voice::Directions) -> Self {
        Self {
            publish: value.publish,
            subscribe: value.subscribe,
        }
    }
}

/// One call participant: device and person as hex.
#[derive(Serialize)]
struct JsVoiceParticipant {
    device: String,
    user: String,
    namespace: String,
    publishes: bool,
}

fn voice_participants(
    participants: Vec<tilo_relay_protocol::voice::VoiceParticipant>,
) -> Vec<JsVoiceParticipant> {
    participants
        .into_iter()
        .map(|participant| JsVoiceParticipant {
            device: hex::encode(participant.device_public_key),
            user: hex::encode(participant.user_public_key),
            namespace: participant.namespace,
            publishes: participant.publishes,
        })
        .collect()
}

/// A call grant for `web/voice.mjs`: ids and the token as hex.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsVoiceGrant {
    call: String,
    epoch: u64,
    participation: String,
    token: String,
    endpoint: String,
    fingerprints: Vec<String>,
    directions: JsDirections,
    publication: Option<String>,
    subscriptions: Vec<String>,
    participants: Vec<JsVoiceParticipant>,
    expires_at_ns: u64,
}

/// A call of the room as `web/voice.mjs` lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsCall {
    call: String,
    epoch: u64,
    active: bool,
    allowed: JsDirections,
    app: String,
    record: String,
    channel: String,
    participants: Vec<JsVoiceParticipant>,
}

fn call_state(state: &tilo_relay_protocol::voice::CallState) -> serde_json::Value {
    serde_json::json!({
        "call": hex::encode(state.call),
        "epoch": state.epoch,
        "active": state.active,
        "allowed": JsDirections::from(state.allowed),
    })
}

/// A call event for `web/voice.mjs`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsVoiceEvent {
    Calls {
        room: String,
        calls: Option<Vec<JsCall>>,
        error: Option<String>,
    },
    CallResult {
        room: String,
        request: String,
        call: Option<serde_json::Value>,
        error: Option<String>,
    },
    Granted {
        room: String,
        grant: JsVoiceGrant,
    },
    Ended {
        room: String,
        call: String,
        reason: String,
    },
}

fn room_of(workspace_id: u64, rooms: &RefCell<std::collections::HashMap<u64, String>>) -> String {
    rooms
        .borrow()
        .get(&workspace_id)
        .cloned()
        .unwrap_or_else(|| workspace_id.to_string())
}

fn voice_event(
    event: tilo_browser_peer::VoiceEvent,
    rooms: &RefCell<std::collections::HashMap<u64, String>>,
) -> JsVoiceEvent {
    use tilo_browser_peer::VoiceEvent;
    match event {
        VoiceEvent::Calls {
            workspace_id,
            result,
        } => {
            let (calls, error) = match result {
                Ok(calls) => (
                    Some(
                        calls
                            .into_iter()
                            .map(|summary| JsCall {
                                call: hex::encode(summary.state.call),
                                epoch: summary.state.epoch,
                                active: summary.state.active,
                                allowed: summary.state.allowed.into(),
                                app: summary.app_id,
                                record: summary.record_id,
                                channel: summary.channel_id,
                                participants: voice_participants(summary.participants),
                            })
                            .collect(),
                    ),
                    None,
                ),
                Err(error) => (None, Some(error)),
            };
            JsVoiceEvent::Calls {
                room: room_of(workspace_id, rooms),
                calls,
                error,
            }
        }
        VoiceEvent::CallResult {
            workspace_id,
            request,
            result,
        } => {
            let (call, error) = match result {
                Ok(state) => (Some(call_state(&state)), None),
                Err(error) => (None, Some(error)),
            };
            JsVoiceEvent::CallResult {
                room: room_of(workspace_id, rooms),
                request: hex::encode(request),
                call,
                error,
            }
        }
        VoiceEvent::Granted {
            workspace_id,
            grant,
        } => {
            let grant = *grant;
            JsVoiceEvent::Granted {
                room: room_of(workspace_id, rooms),
                grant: JsVoiceGrant {
                    call: hex::encode(grant.call),
                    epoch: grant.epoch,
                    participation: hex::encode(grant.participation),
                    token: hex::encode(&grant.token),
                    endpoint: grant.endpoint,
                    fingerprints: grant.fingerprints,
                    directions: grant.directions.into(),
                    publication: grant.publication,
                    subscriptions: grant.subscriptions,
                    participants: voice_participants(grant.participants),
                    expires_at_ns: grant.expires_at_ns,
                },
            }
        }
        VoiceEvent::Ended {
            workspace_id,
            call,
            reason,
        } => JsVoiceEvent::Ended {
            room: room_of(workspace_id, rooms),
            call: hex::encode(call),
            reason,
        },
    }
}

/// A change to a call, as `web/voice.mjs` asks for it.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsCallOperation {
    Start {
        app: String,
        record: String,
        channel: String,
        allowed: JsDirections,
    },
    End {
        call: String,
        epoch: u64,
    },
    Restart {
        call: String,
        epoch: u64,
    },
}

impl JsCallOperation {
    fn into_operation(self) -> Result<tilo_relay_protocol::voice::CallOperation, JsError> {
        use tilo_relay_protocol::voice::{CallOperation, Directions};
        Ok(match self {
            Self::Start {
                app,
                record,
                channel,
                allowed,
            } => CallOperation::Start {
                app_id: app,
                record_id: record,
                channel_id: channel,
                allowed: Directions {
                    publish: allowed.publish,
                    subscribe: allowed.subscribe,
                },
            },
            Self::End { call, epoch } => CallOperation::End {
                call: hex32(&call, "call")?,
                epoch,
            },
            Self::Restart { call, epoch } => CallOperation::Restart {
                call: hex32(&call, "call")?,
                epoch,
            },
        })
    }
}

/// A remote-action event for `web/agents.mjs` (and `actionsV1`).
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsRemoteAction {
    /// A frame another device addressed to this one (`frame` is the JSON
    /// `RemoteActionFrame` text).
    RemoteAction { from: String, frame: String },
    /// The host could not deliver a frame this device sent.
    Undeliverable { to: String, reason: String },
}

/// `{agentId, label, color, device, rooms, apps, mayConfirm, expiresAtNs}`
/// of a delegation that verified.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsDelegation {
    agent_id: String,
    label: String,
    color: String,
    device: String,
    rooms: Vec<String>,
    apps: Vec<String>,
    may_confirm: bool,
    expires_at_ns: u64,
}

/// Verify an agent's delegation (hex, as `AgentInfo.delegation` carries it)
/// against the user root the page trusts, at `nowMs`, and return what it
/// grants as JSON. The page calls an agent "yours" only after this.
///
/// # Errors
///
/// Throws for a delegation that does not decode, names another user, does
/// not verify, or has expired.
#[wasm_bindgen(js_name = verifyAgentDelegation)]
pub fn verify_agent_delegation(
    delegation_hex: &str,
    user_public_key_hex: &str,
    now_ms: f64,
) -> Result<String, JsError> {
    let bytes = hex::decode(delegation_hex).map_err(js_error)?;
    let delegation =
        tilo_relay_protocol::agents::AgentDelegation::decode(&bytes).map_err(js_error)?;
    let user: [u8; 32] = hex::decode(user_public_key_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| js_error("user public key must be 32 bytes of hex"))?;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a page clock in milliseconds is well inside u64 nanoseconds"
    )]
    let now_ns = (now_ms.max(0.0) as u64).saturating_mul(1_000_000);
    delegation.verify(&user, now_ns).map_err(js_error)?;
    serde_json::to_string(&JsDelegation {
        agent_id: delegation.agent_id,
        label: delegation.label,
        color: tilo_relay_protocol::agents::color_hex(delegation.color),
        device: hex::encode(delegation.agent_device_public_key),
        rooms: delegation.scope.rooms,
        apps: delegation.scope.apps,
        may_confirm: delegation.scope.may_confirm,
        expires_at_ns: delegation.expires_at_ns,
    })
    .map_err(js_error)
}

/// A running certified peer.
#[wasm_bindgen]
pub struct WasmPeer {
    id: u32,
    device_key: [u8; 32],
    reporter_cert: DeviceCert,
    owner_cert: DeviceCert,
    handle: PeerHandle,
    durable_events: PeerEvents,
    remote_events: PeerEvents,
    voice_events: PeerEvents,
    /// Joined rooms by workspace, to name them in call events.
    voice_rooms: RefCell<std::collections::HashMap<u64, String>>,
    listeners: Rc<RefCell<Vec<Function>>>,
    /// The profile's store and a queue opened at start, until a room is
    /// joined and replicated ([`crate::replication`]).
    replication: RefCell<Option<(PeerEvents, Rc<durable_store::MemoryStore>)>>,
}

#[wasm_bindgen]
impl WasmPeer {
    /// Start a session. `configJson` is `{cert, hostCert}` from
    /// [`certify_device`], optionally with `displayName`. With `namespace`
    /// (`{profile, device}` JSON, whose storage the page has opened) the
    /// peer record lives in that profile's durable store; otherwise `record`
    /// is the stored record bytes (or `undefined`) and `save(bytes)` writes
    /// it. `post(path, session, body)` reaches the host origin.
    /// `channelVault` (32 bytes, from `channel-vault.mjs`) seals the
    /// profile's channel state at rest; a peer with a namespace needs it.
    ///
    /// # Errors
    ///
    /// Throws for a malformed configuration, custody object, namespace or
    /// channel vault, or neither a namespace nor a save callback.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "the session takes shared handles; in the page they stay on its one thread"
    )]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen receives optional JavaScript values only as owned values"
    )]
    pub fn start(
        config_json: &str,
        custody: &JsValue,
        namespace: Option<String>,
        record: Option<Vec<u8>>,
        save: Option<Function>,
        post: Function,
        channel_vault: Option<Vec<u8>>,
    ) -> Result<WasmPeer, JsError> {
        let config: ConnectConfig = serde_json::from_str(config_json).map_err(js_error)?;
        let signer = PageSigner::from_js(custody)?;
        if signer.device_public_key() != config.cert.device_public_key {
            return Err(js_error("custody does not hold the certified device key"));
        }
        let mut profile_store = None;
        let store: Arc<dyn PeerStore> = if let Some(namespace) = namespace {
            let channel_vault: [u8; 32] = channel_vault
                .as_deref()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| js_error("a profile peer needs its 32-byte channel vault"))?;
            let durable =
                DurablePeerStore::open(&namespace, &config.cert.user_public_key, channel_vault)?;
            profile_store = Some(Rc::clone(&durable.store));
            Arc::new(durable)
        } else {
            let record = record
                .map(|bytes| postcard::from_bytes::<PeerRecord>(&bytes))
                .transpose()
                .map_err(js_error)?;
            Arc::new(PageStore {
                current: RefCell::new(record),
                save: save
                    .ok_or_else(|| js_error("a peer needs a namespace or a save callback"))?,
            })
        };
        let device_key = config.cert.device_public_key;
        let reporter_cert = config.cert.clone();
        let owner_cert = config.host_cert.clone();
        install_identity(&config.cert, config.display_name);
        let handle = PeerHandle::start(
            PeerConfig {
                host: Host::Https(Arc::new(PageHttp(post))),
                cert: config.cert,
                host_cert: config.host_cert,
                revocations: RevocationList::new(),
            },
            Arc::new(signer),
            store,
        );
        let id = NEXT_PEER.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1).max(1));
            id
        });
        let replication = profile_store.map(|store| (handle.events(), store));
        let listeners: Rc<RefCell<Vec<Function>>> = Rc::default();
        let changed = Arc::new(tokio::sync::Notify::new());
        handle.on_change({
            let changed = changed.clone();
            Arc::new(move || changed.notify_one())
        });
        wasm_bindgen_futures::spawn_local({
            let listeners = Rc::downgrade(&listeners);
            async move {
                loop {
                    changed.notified().await;
                    let Some(listeners) = listeners.upgrade() else {
                        return;
                    };
                    crate::replication::pump(id);
                    crate::feedback_delivery::pump(id);
                    let current = listeners.borrow().clone();
                    for listener in current {
                        let _ = listener.call0(&JsValue::NULL);
                    }
                }
            }
        });
        Ok(Self {
            id,
            device_key,
            reporter_cert,
            owner_cert,
            durable_events: handle.events(),
            remote_events: handle.events(),
            voice_events: handle.events(),
            voice_rooms: RefCell::default(),
            handle,
            listeners,
            replication: RefCell::new(replication),
        })
    }

    /// Select an existing destination. Admission comes only from the connected
    /// owner, asynchronously; mount after feedbackStatus reports ready.
    /// # Errors
    /// Refuses missing existing membership, record or a repeated configuration.
    #[wasm_bindgen(js_name = configureFeedback)]
    pub fn configure_feedback(&self, destination: &str) -> Result<(), JsError> {
        crate::feedback_delivery::configure(
            self.id,
            self.handle.clone(),
            self.reporter_cert.clone(),
            self.owner_cert.clone(),
            serde_json::from_str(destination).map_err(js_error)?,
        )
        .map_err(js_error)
    }
    #[wasm_bindgen(js_name = feedbackStatus)]
    pub fn feedback_status(&self) -> String {
        crate::feedback_delivery::status(self.id)
    }

    /// `PeerSnapshot` JSON.
    pub fn snapshot(&self) -> String {
        serde_json::to_string(&PeerSnapshot::from_status(&self.handle.status())).unwrap_or_default()
    }

    /// This page's channel state as text: whom each room follows, which
    /// senders may govern it, and each route's membership.
    #[wasm_bindgen(js_name = channelDiagnostics)]
    pub fn channel_diagnostics(&self) -> String {
        self.handle.status().channels
    }

    /// Call `listener()` whenever the snapshot may have changed.
    pub fn subscribe(&self, listener: Function) {
        self.listeners.borrow_mut().push(listener);
    }

    /// Join a room (uuid).
    ///
    /// # Errors
    ///
    /// Throws for a room that is not a uuid, or a closed session.
    #[wasm_bindgen(js_name = joinRoom)]
    pub fn join_room(&self, room: &str) -> Result<(), JsError> {
        let workspace_id = workspace(room)?;
        self.handle.join(workspace_id).map_err(js_error)?;
        self.voice_rooms
            .borrow_mut()
            .insert(workspace_id, room.to_owned());
        if let Some((events, store)) = self.replication.borrow_mut().take() {
            let device = hex::encode(self.device_key);
            crate::replication::open(self.id, device, self.handle.clone(), events, room, store)
                .map_err(js_error)?;
        }
        Ok(())
    }

    /// This peer in the page, for `mountDesktop({peer})`: mounts in its
    /// room replicate their records and desktop through it.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// A live-plane grant for `room` over the current HTTPS session, as
    /// `{token (hex), room, publisher, expiresAtNs}` JSON. With `agent`, the
    /// grant is for that agent of this device: it publishes under the
    /// agent's own namespace, never as the device.
    ///
    /// # Errors
    ///
    /// Rejects when not connected or not admitted to the room.
    /// Administer the host's rooms as this device: `request` is a
    /// `RoomsRequest` as JSON (serde's external tagging, byte arrays as
    /// arrays of numbers); resolves to the `RoomsReply` as JSON.
    ///
    /// # Errors
    ///
    /// Rejects for a malformed request, without a connection, or when the
    /// host is not reached.
    pub async fn rooms(&self, request: String) -> Result<String, JsError> {
        let request: tilo_relay_protocol::rooms::RoomsRequest =
            serde_json::from_str(&request).map_err(js_error)?;
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.handle
            .rooms_then(
                request,
                Box::new(move |answer| {
                    let _ = reply.send(answer);
                }),
            )
            .map_err(js_error)?;
        let answer = receipt
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)?;
        serde_json::to_string(&answer).map_err(js_error)
    }

    /// The device (hex) of the session a node runs beside its relay, the
    /// one that runs the person's apps there; `undefined` from a host that
    /// runs none.
    ///
    /// # Errors
    ///
    /// Rejects without a connection.
    #[wasm_bindgen(js_name = nodeSession)]
    pub async fn node_session(&self) -> Result<Option<String>, JsError> {
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.handle
            .node_session_then(Box::new(move |answer| {
                let _ = reply.send(answer);
            }))
            .map_err(js_error)?;
        Ok(receipt
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)?
            .map(hex::encode))
    }

    /// Pre-admit whoever redeems the invitation this issues and presents
    /// the token whose SHA-256 the grant names: `options` is JSON
    /// `{requestId, room, role, tokenSha256, label, expiresInSeconds,
    /// inviterName?}` (hex ids, `role` `Viewer`/`Commenter`/`Editor`); the
    /// owner's `root` signs the grant. Resolves to the `RoomsReply` as JSON.
    ///
    /// # Errors
    ///
    /// Rejects for malformed options, when the root does not sign, or as
    /// [`Self::rooms`].
    #[wasm_bindgen(js_name = preAdmit)]
    pub async fn pre_admit(&self, root: JsValue, options: String) -> Result<String, JsError> {
        use tilo_browser_peer::DeviceSigner as _;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Options {
            request_id: String,
            room: String,
            role: tilo_relay_protocol::rooms::RoomRole,
            token_sha256: String,
            label: String,
            expires_in_seconds: u64,
            inviter_name: Option<String>,
        }
        let options: Options = serde_json::from_str(&options).map_err(js_error)?;
        let root = PageSigner::from_js(&root)?;
        let now_ns = ui::time::unix_now_ns();
        let mut grant = tilo_relay_protocol::rooms::PreAdmission {
            room: options.room,
            role: options.role,
            token_sha256: device_key(&options.token_sha256)?,
            label: options.label,
            expires_at_ns: now_ns
                .saturating_add(options.expires_in_seconds.saturating_mul(1_000_000_000)),
            owner_user_public_key: root.device_public_key(),
            signature: [0; 64],
        };
        grant.signature = root
            .sign(grant.digest().map_err(js_error)?.to_vec())
            .await
            .map_err(|error| js_error(error.0))?;
        self.rooms(
            serde_json::to_string(&tilo_relay_protocol::rooms::RoomsRequest::PreAdmit {
                request_id: request_id(&options.request_id)?,
                grant,
                inviter_name: options.inviter_name,
            })
            .map_err(js_error)?,
        )
        .await
    }

    #[wasm_bindgen(js_name = liveGrant)]
    pub async fn live_grant(&self, room: String, agent: Option<String>) -> Result<String, JsError> {
        let (reply, receipt) = tokio::sync::oneshot::channel();
        let done: tilo_browser_peer::session::GrantCompletion = Box::new(move |grant| {
            let _ = reply.send(grant);
        });
        let workspace = workspace(&room)?;
        match agent {
            None => self.handle.live_grant_then(workspace, done),
            Some(agent) => self.handle.agent_live_grant_then(workspace, agent, done),
        }
        .map_err(js_error)?;
        let grant = receipt
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)?;
        serde_json::to_string(&JsGrant {
            token: hex::encode(grant.token),
            room: grant.room,
            publisher: grant.publisher,
            expires_at_ns: grant.expires_at_ns,
        })
        .map_err(js_error)
    }

    /// Seal one encoded live frame of `room` (from [`encode_live_presence`]
    /// and friends) with the channel keys of the record it names, for the
    /// page's live plane. Resolves to `null` when the frame names no record
    /// or this device holds none of its keys: such a frame is never sent.
    ///
    /// # Errors
    ///
    /// Rejects for a malformed room or a closed session.
    #[wasm_bindgen(js_name = sealLive)]
    pub async fn seal_live(
        &self,
        room: String,
        frame: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, JsError> {
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.handle
            .seal_live_then(
                workspace(&room)?,
                frame,
                Box::new(move |sealed| {
                    let _ = reply.send(sealed);
                }),
            )
            .map_err(js_error)?;
        receipt.await.map_err(|_| js_error("peer session closed"))
    }

    /// Open one sealed live frame of `room` the live plane delivered.
    /// Resolves to the encoded frame for [`decode_live_frame`], or `null`
    /// when it does not open with keys this device holds.
    ///
    /// # Errors
    ///
    /// Rejects for a malformed room or a closed session.
    #[wasm_bindgen(js_name = openLive)]
    pub async fn open_live(
        &self,
        room: String,
        frame: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, JsError> {
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.handle
            .open_live_then(
                workspace(&room)?,
                frame,
                Box::new(move |opened| {
                    let _ = reply.send(opened);
                }),
            )
            .map_err(js_error)?;
        receipt.await.map_err(|_| js_error("peer session closed"))
    }

    /// Ask the host for `room`'s calls; the answer is a `calls` event of
    /// [`Self::take_voice_events`].
    ///
    /// # Errors
    ///
    /// Throws for a malformed room or a closed session.
    #[wasm_bindgen(js_name = voiceCalls)]
    pub fn voice_calls(&self, room: &str) -> Result<(), JsError> {
        self.handle.voice_calls(workspace(room)?).map_err(js_error)
    }

    /// Start, end or restart a call of `room` (`{kind: 'start', app, record,
    /// channel, allowed}`, `{kind: 'end' | 'restart', call, epoch}` JSON);
    /// its `callResult` event names `request` (16 bytes of hex).
    ///
    /// # Errors
    ///
    /// Throws for a malformed room, operation or request, or a closed
    /// session.
    #[wasm_bindgen(js_name = voiceCall)]
    pub fn voice_call(
        &self,
        room: &str,
        operation_json: &str,
        request: &str,
    ) -> Result<(), JsError> {
        let operation: JsCallOperation = serde_json::from_str(operation_json).map_err(js_error)?;
        let request: [u8; 16] = hex::decode(request)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| js_error("request must be 16 bytes of hex"))?;
        self.handle
            .voice_call(workspace(room)?, request, operation.into_operation()?)
            .map_err(js_error)
    }

    /// Take part in call `call` (hex) of `room` until [`Self::voice_leave`]:
    /// the session keeps the participation alive and its grants arrive as
    /// `granted` events of [`Self::take_voice_events`].
    ///
    /// # Errors
    ///
    /// Throws for a malformed room or call, or a closed session.
    #[wasm_bindgen(js_name = voiceJoin)]
    pub fn voice_join(&self, room: &str, call: &str) -> Result<(), JsError> {
        self.handle
            .voice_join(workspace(room)?, hex32(call, "call")?)
            .map_err(js_error)
    }

    /// Leave the call of `room`.
    ///
    /// # Errors
    ///
    /// Throws for a malformed room or a closed session.
    #[wasm_bindgen(js_name = voiceLeave)]
    pub fn voice_leave(&self, room: &str) -> Result<(), JsError> {
        self.handle.voice_leave(workspace(room)?).map_err(js_error)
    }

    /// Call events since the last call, as a JSON array of `{kind: 'calls',
    /// room, calls | error}`, `{kind: 'callResult', room, request, call |
    /// error}`, `{kind: 'granted', room, grant}` and `{kind: 'ended', room,
    /// call, reason}`.
    #[wasm_bindgen(js_name = takeVoiceEvents)]
    pub fn take_voice_events(&self) -> String {
        let out: Vec<JsVoiceEvent> = self
            .voice_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Voice(event) => Some(voice_event(event, &self.voice_rooms)),
                _ => None,
            })
            .collect();
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
    }

    /// Send `events` (JSON array of `legacy_window::InputEvent`s, externally
    /// tagged: `{"PointerMotion": {"x": 1, "y": 2}}`) for the legacy window
    /// `surface` (16 hex) that `publisher` (64 hex) streams in `room`. With
    /// no events it only asks whether this device may drive the window.
    /// Resolves `false` when it may not.
    ///
    /// # Errors
    ///
    /// Rejects malformed arguments, or when the host is not reached, the
    /// window is gone or its session is not reachable.
    #[wasm_bindgen(js_name = legacyInput)]
    pub async fn legacy_input(
        &self,
        room: String,
        publisher: String,
        surface: String,
        events: String,
    ) -> Result<bool, JsError> {
        let publisher: [u8; 32] = hex::decode(&publisher)
            .ok()
            .and_then(|key| key.try_into().ok())
            .ok_or_else(|| js_error("publisher is not a device key"))?;
        let surface = u64::from_str_radix(&surface, 16).map_err(js_error)?;
        let request = tilo_relay_protocol::legacy_window::InputRequest {
            workspace_id: workspace(&room)?,
            publisher,
            surface,
            events: serde_json::from_str(&events).map_err(js_error)?,
        };
        if !request.valid() {
            return Err(js_error("input out of bounds"));
        }
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.handle
            .legacy_input_then(
                request,
                Box::new(move |sent| {
                    let _ = reply.send(sent);
                }),
            )
            .map_err(js_error)?;
        receipt
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)
    }

    /// Records whose durable change landed since the last call, as a JSON
    /// array of `{kind: 'durable', record}`: the live layer drops their
    /// previews and snaps drags.
    #[wasm_bindgen(js_name = takeDurableEvents)]
    pub fn take_durable_events(&self) -> String {
        let out: Vec<JsDurable> = self
            .durable_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Document(document) => Some(JsDurable::Durable {
                    record: JsRecord {
                        app: document.route.app_id,
                        record: document.route.record,
                    },
                }),
                _ => None,
            })
            .collect();
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
    }

    /// Send one remote-action frame (JSON `RemoteActionFrame` text) to the
    /// device `to` (hex) of `room`; the host's own device key reaches its
    /// app-serving plane (apps and the agent supervisor).
    ///
    /// # Errors
    ///
    /// Throws for a malformed room or device, or a closed session.
    #[wasm_bindgen(js_name = sendRemoteAction)]
    pub fn send_remote_action(&self, room: &str, to: &str, frame: &str) -> Result<(), JsError> {
        let to: [u8; 32] = hex::decode(to)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| js_error("device must be 32 bytes of hex"))?;
        self.handle
            .send_remote_action(workspace(room)?, to, frame.as_bytes().to_vec())
            .map_err(js_error)
    }

    /// Remote-action frames and undeliverable answers since the last call,
    /// as a JSON array of `{kind: 'remoteAction', from, frame}` and
    /// `{kind: 'undeliverable', to, reason}`.
    #[wasm_bindgen(js_name = takeRemoteActionEvents)]
    pub fn take_remote_action_events(&self) -> String {
        let out: Vec<JsRemoteAction> = self
            .remote_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::RemoteAction { from, frame, .. } => Some(JsRemoteAction::RemoteAction {
                    from: hex::encode(from),
                    frame: String::from_utf8(frame).ok()?,
                }),
                PeerEvent::RemoteActionUndeliverable { to, reason, .. } => {
                    Some(JsRemoteAction::Undeliverable {
                        to: hex::encode(to),
                        reason,
                    })
                }
                _ => None,
            })
            .collect();
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
    }

    /// Whether the page holds the room's record `record` of `app`: open in
    /// a mount, or received from the room and stored for whoever opens it.
    #[wasm_bindgen(js_name = hasRecord)]
    pub fn has_record(&self, app: &str, record: &str) -> bool {
        crate::replication::has_record(self.id, app, record)
    }

    /// The room's records this page holds, as JSON `[{app, record}]`, so a
    /// page can hand a guest the record its desktop created.
    #[wasm_bindgen(js_name = roomRecords)]
    pub fn room_records(&self) -> String {
        let records: Vec<JsRecord> = crate::replication::records(self.id)
            .into_iter()
            .map(|(app, record)| JsRecord { app, record })
            .collect();
        serde_json::to_string(&records).unwrap_or_else(|_| "[]".into())
    }

    /// Report the page store's durability of a record (`unknown`,
    /// `pending`, `committing`, `durable`, `failed`).
    #[wasm_bindgen(js_name = setLocalState)]
    pub fn set_local_state(&self, app: &str, room: &str, record: &str, state: &str) {
        let state = match state {
            "pending" => LocalState::Pending,
            "committing" => LocalState::Committing,
            "durable" => LocalState::Durable,
            "failed" => LocalState::Failed,
            _ => LocalState::Unknown,
        };
        self.handle.set_local_state(
            DocumentRoute {
                app_id: app.into(),
                cluster_id: room.into(),
                record: record.into(),
            },
            state,
        );
    }

    /// Resolve once every published document is durable on the host.
    ///
    /// # Errors
    ///
    /// Rejects when the host refused a change or the session closed.
    pub async fn flush(&self) -> Result<(), JsError> {
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.handle
            .barrier_then(Box::new(move |result| {
                let _ = reply.send(result);
            }))
            .map_err(js_error)?;
        receipt
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)
    }

    /// Reconnect now (also after a failure).
    ///
    /// # Errors
    ///
    /// Throws once the session is closed.
    pub fn reconnect(&self) -> Result<(), JsError> {
        self.handle.reconnect().map_err(js_error)
    }

    /// Ask the host for the joined rooms' shared state again.
    ///
    /// # Errors
    ///
    /// Throws once the session is closed.
    pub fn refresh(&self) -> Result<(), JsError> {
        self.handle.refresh().map_err(js_error)
    }

    /// Report a failure of the page store (no `surface`), of desktop surface
    /// `surface`, or of that surface's `asset`, with a `FailureCode`.
    ///
    /// # Errors
    ///
    /// Throws on a code the page may not report.
    #[wasm_bindgen(js_name = reportFailure)]
    pub fn report_failure(
        &self,
        surface: Option<String>,
        asset: Option<String>,
        code: &str,
        message: &str,
    ) -> Result<(), JsError> {
        self.handle
            .report_failure(failure_subject(surface, asset), code, message)
            .map_err(js_error)
    }

    /// Clear the failure `reportFailure` reported for the same subject.
    #[wasm_bindgen(js_name = clearFailure)]
    pub fn clear_failure(&self, surface: Option<String>, asset: Option<String>) {
        self.handle.clear_failure(&failure_subject(surface, asset));
    }

    /// Clear every failure of desktop surface `surface` and its assets.
    #[wasm_bindgen(js_name = clearSurface)]
    pub fn clear_surface(&self, surface: &str) {
        self.handle.clear_surface(surface);
    }

    /// End the session. The page no longer acts as the certified user.
    pub fn dispose(&self) {
        self.listeners.borrow_mut().clear();
        self.handle.close();
        crate::replication::close(self.id);
        ui::local_identity::install_source(None);
    }
}
