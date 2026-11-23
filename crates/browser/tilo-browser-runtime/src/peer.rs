//! The page's certified peer: `tilo_session_client` behind wasm-bindgen for
//! `web/peer.mjs`, reaching the host over HTTPS only.
//!
//! The device key stays in device key (`web/device-key.mjs`): either a
//! non-extractable `WebCrypto` Ed25519 key that signs through a JavaScript
//! callback, or — where `WebCrypto` lacks Ed25519 — a seed unsealed from
//! `IndexedDB` straight into this module, which keeps it only in wasm
//! memory. Nothing here generates or holds a user root: a page that holds
//! one (`web/identity.mjs`) lends it as a signer, the same shape as
//! device key, to [`issue_device_cert`] and [`certify_with_root`].
//!
//! The peer record (certificates and the commit outbox) and the workspaces it
//! holds are kept in the profile's durable store when the page names a profile
//! ([`DurablePeerStore`]), committed to `IndexedDB` with everything else.
//!
//! HTTP goes through the page's `post(path, session, body)` (its `fetch`).
//! Presence — who is here, their pointer, what their shell shows — rides the
//! workspace's live session like every other device's, kept on the session's own
//! board ([`crate::peer_sync`]) and read back as the members answer
//! ([`WasmPeer::take_live_events`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Function, Promise, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use tilo_device_identity::login::{Approved, Asked};
use tilo_live::live::Track;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::https::{HttpClient, HttpFuture};
use tilo_server_client::{Answer, AnswerFuture};
use tilo_server_client::{DeviceSigner, ServerAddress, SignError, SignFuture};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::live::{LiveAnchor, LiveFrame, LiveResource, OpsFrame, PreviewOp};
use tilo_server_protocol::web::{PageHost, PageHostAddress};
use tilo_session::sync::{Cmd, Event, InCall, Outbox};
use tilo_session_client::device::{Device, DeviceRefusal};
use tilo_session_client::{Events, FailureKind, Link, LocalState};
use tilo_session_client::{PeerConfig, PeerEntry, PeerStore};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList, RevocationReason};
use tilo_workspace_keys::VAULT_SECRET_APP as CHANNEL_VAULT_APP;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::peer_sync::{due, hear, members, release, say, set_label};
use crate::presence_value::JsPresence;

pub(crate) fn js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

/// `refusal` as the page reads it: an `Error` named `DeviceRefusal`, with the refusal's `code` and,
/// where the host gave one, its typed `refusal` (`{reason, message, next}`).
pub(crate) fn refusal_error(refusal: &DeviceRefusal) -> JsValue {
    let error = js_sys::Error::new(&refusal.message);
    error.set_name("DeviceRefusal");
    let _ = Reflect::set(&error, &"code".into(), &refusal.code.as_str().into());
    if let Some(typed) = refusal
        .refusal
        .as_ref()
        .and_then(|typed| serde_json::to_string(typed).ok())
        .and_then(|text| js_sys::JSON::parse(&text).ok())
    {
        let _ = Reflect::set(&error, &"refusal".into(), &typed);
    }
    if let Some(wait) = refusal.retry_after_ms {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a wait is far below what a JavaScript number holds exactly"
        )]
        let seconds = wait as f64 / 1000.0;
        let _ = Reflect::set(&error, &"retryAfter".into(), &seconds.into());
    }
    error.into()
}

/// A refusal of a workspace's host, `wire` as it travels (an `ActionCallError`'s JSON), read as
/// every caller reads it ([`DeviceRefusal::of`]): `{code, message, refusal}` JSON, `refusal` the
/// host's typed refusal where it gave one.
///
/// # Errors
///
/// Throws for text that is no refusal of a host's.
/// The colour presence draws the identity `id` in when nothing settled
/// another one for it, as `#rrggbb`: [`ui::presence::user_color`], the one
/// the native desktop uses, so a page never computes a colour of its own.
#[wasm_bindgen(js_name = presenceColor)]
#[must_use]
pub fn presence_color(id: &str) -> String {
    let (r, g, b) = ui::presence::user_color(id);
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[wasm_bindgen(js_name = hostRefusal)]
pub fn host_refusal(wire: &str) -> Result<String, JsError> {
    let error: tilo_actions::remote::ActionCallError =
        serde_json::from_str(wire).map_err(js_error)?;
    serde_json::to_string(&DeviceRefusal::of(&error)).map_err(js_error)
}

/// The page store without a surface; a desktop surface; its asset.
fn failure_kind(surface: Option<String>, asset: Option<String>) -> FailureKind {
    match (surface, asset) {
        (None, _) => FailureKind::Storage,
        (Some(surface), None) => FailureKind::Surface(surface),
        (Some(surface), Some(asset)) => FailureKind::Asset { surface, asset },
    }
}

/// A device signer backed by page device key.
pub(crate) enum PageSigner {
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
    pub(crate) fn from_js(device_key: &JsValue) -> Result<Self, JsError> {
        let seed = Reflect::get(device_key, &"seed".into()).map_err(|_| js_error("device key"))?;
        if !seed.is_undefined() && !seed.is_null() {
            let seed: [u8; 32] = Uint8Array::new(&seed)
                .to_vec()
                .try_into()
                .map_err(|_| js_error("device seed must be 32 bytes"))?;
            return Ok(Self::Seed(ed25519_dalek::SigningKey::from_bytes(&seed)));
        }
        let public_key: [u8; 32] = Uint8Array::new(
            &Reflect::get(device_key, &"publicKey".into()).map_err(|_| js_error("device key"))?,
        )
        .to_vec()
        .try_into()
        .map_err(|_| js_error("device public key must be 32 bytes"))?;
        let sign: Function = Reflect::get(device_key, &"sign".into())
            .map_err(|_| js_error("device key"))?
            .dyn_into()
            .map_err(|_| js_error("device key.sign must be a function"))?;
        Ok(Self::WebCrypto {
            public_key,
            sign,
            this: device_key.clone(),
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
                        .map_err(|_| SignError("device key refused to sign".into()))?
                        .dyn_into()
                        .map_err(|_| SignError("device key.sign must return a promise".into()))?;
                    let signature = JsFuture::from(promise)
                        .await
                        .map_err(|_| SignError("device key signing failed".into()))?;
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
    groups_key: durable_store::StorageKey,
    /// The workspaces the peer holds, beside the record.
    workspaces_key: durable_store::StorageKey,
    /// What the keys of the runs of device key kept beside the workspaces begin with.
    runs_prefix: String,
    /// What the keys of the record's own parts, kept beside the workspaces' index, begin with.
    parts_prefix: String,
    /// Seals the member state in `groups_key`. The page keeps it in the
    /// platform secret store ([`channel_vault`]), never in this one.
    channel_vault: [u8; 32],
}

thread_local! {
    static NEXT_PEER: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

/// The id a page opens as: `keep` when the page it replaces let it go as
/// its session ended, else the next one. A page that certifies its device
/// again opens as the page it was, so the mounts it made keep reaching it.
fn page_id(keep: Option<u32>) -> u32 {
    match keep.filter(|keep| !crate::peer_sync::registered(*keep)) {
        Some(keep) => {
            NEXT_PEER.with(|next| {
                if next.get() <= keep {
                    next.set(keep.wrapping_add(1).max(1));
                }
            });
            keep
        }
        None => NEXT_PEER.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1).max(1));
            id
        }),
    }
}

impl DurablePeerStore {
    /// The key the run of device key `name` is kept under.
    fn run_key(&self, name: &str) -> durable_store::StorageKey {
        durable_store::StorageKey::document(
            PEER_PACKAGE,
            crate::storage::PROFILE_WORKSPACE,
            format!("{}{name}", self.runs_prefix),
        )
    }

    /// The key the record's part `name` is kept under.
    fn part_key(&self, name: &str) -> durable_store::StorageKey {
        durable_store::StorageKey::document(
            PEER_PACKAGE,
            crate::storage::PROFILE_WORKSPACE,
            format!("{}{name}", self.parts_prefix),
        )
    }

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
            key: durable_store::StorageKey::document(
                PEER_PACKAGE,
                crate::storage::PROFILE_WORKSPACE,
                hex::encode(user_public_key),
            ),
            groups_key: durable_store::StorageKey::document(
                PEER_PACKAGE,
                crate::storage::PROFILE_WORKSPACE,
                format!("{}.groups", hex::encode(user_public_key)),
            ),
            workspaces_key: durable_store::StorageKey::document(
                PEER_PACKAGE,
                crate::storage::PROFILE_WORKSPACE,
                format!("{}.workspaces", hex::encode(user_public_key)),
            ),
            runs_prefix: format!("{}.workspaces.", hex::encode(user_public_key)),
            parts_prefix: format!("{}.workspaces.part.", hex::encode(user_public_key)),
            channel_vault,
        })
    }
}

impl PeerStore for DurablePeerStore {
    fn load(&self) -> Option<PeerEntry> {
        use durable_store::DurableStore as _;
        let stored = self.store.read(&self.key).ok()??;
        postcard::from_bytes(&stored.bytes).ok()
    }

    fn save(&self, record: &PeerEntry) -> Result<(), String> {
        use durable_store::DurableStore as _;
        let bytes = postcard::to_stdvec(record).map_err(|error| error.to_string())?;
        self.store
            .write(&self.key, durable_store::WriteOp::Replace(bytes), None)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        use durable_store::DurableStore as _;
        Some(self.store.read(&self.groups_key).ok()??.bytes)
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        use durable_store::DurableStore as _;
        self.store
            .write(
                &self.groups_key,
                durable_store::WriteOp::Replace(bytes.to_vec()),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn load_workspaces(&self) -> Option<Vec<u8>> {
        use durable_store::DurableStore as _;
        Some(self.store.read(&self.workspaces_key).ok()??.bytes)
    }

    fn keeps_workspaces(&self) -> bool {
        true
    }

    fn save_workspaces(&self, bytes: &[u8]) -> Result<(), String> {
        use durable_store::DurableStore as _;
        self.store
            .write(
                &self.workspaces_key,
                durable_store::WriteOp::Replace(bytes.to_vec()),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn save_workspaces_run(&self, name: &str, bytes: Option<&[u8]>) -> Result<(), String> {
        use durable_store::DurableStore as _;
        // The store forgets no key: a run let go keeps its key, empty.
        self.store
            .write(
                &self.run_key(name),
                durable_store::WriteOp::Replace(bytes.unwrap_or_default().to_vec()),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn load_workspaces_run(&self, name: &str) -> Option<Vec<u8>> {
        use durable_store::DurableStore as _;
        Some(self.store.read(&self.run_key(name)).ok()??.bytes).filter(|bytes| !bytes.is_empty())
    }

    fn load_workspaces_part(&self, name: &str) -> Option<Vec<u8>> {
        use durable_store::DurableStore as _;
        Some(self.store.read(&self.part_key(name)).ok()??.bytes).filter(|bytes| !bytes.is_empty())
    }

    fn save_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        use durable_store::DurableStore as _;
        self.store
            .write(
                &self.part_key(name),
                durable_store::WriteOp::Replace(bytes.to_vec()),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn append_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        use durable_store::DurableStore as _;
        self.store
            .write(
                &self.part_key(name),
                durable_store::WriteOp::Append(bytes.to_vec()),
                None,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn keeps_workspaces_parts(&self) -> bool {
        true
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.channel_vault)
    }
}

/// The peer record in page storage: loaded before start, saved through a
/// callback that writes it to `IndexedDB`.
struct PageStore {
    current: RefCell<Option<PeerEntry>>,
    save: Function,
}

impl PeerStore for PageStore {
    fn load(&self) -> Option<PeerEntry> {
        self.current.borrow().clone()
    }

    fn save(&self, record: &PeerEntry) -> Result<(), String> {
        let bytes = postcard::to_stdvec(record).map_err(|error| error.to_string())?;
        *self.current.borrow_mut() = Some(record.clone());
        self.save
            .call1(&JsValue::NULL, &Uint8Array::from(bytes.as_slice()).into())
            .map(|_| ())
            .map_err(|_| "the page store refused the write".to_string())
    }
}

/// HTTP through the page: `post(path, body)` resolves to
/// `{status, body, retryAfter}` (`retryAfter` the header's text, when the answer carries one),
/// rejecting only when the host was not reached.
struct PageHttp(Function);

impl HttpClient for PageHttp {
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        let answer = self.post_answer(path, body);
        Box::pin(async move {
            let answer = answer.await?;
            Ok((answer.status, answer.body))
        })
    }

    fn post_answer(&self, path: &str, body: Vec<u8>) -> AnswerFuture<'_> {
        let called = self.0.call2(
            &JsValue::NULL,
            &path.into(),
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
            let retry_after_secs = Reflect::get(&answer, &"retryAfter".into())
                .ok()
                .and_then(|value| value.as_string())
                .and_then(|value| tilo_server_client::https::retry_after(&value));
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an HTTP status is a small positive integer"
            )]
            Ok(Answer {
                status: status as u16,
                retry_after_secs,
                body: Uint8Array::new(&body).to_vec(),
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectConfig {
    cert: DeviceCert,
    #[serde(rename = "hostCert")]
    server_cert: DeviceCert,
    /// Where the server's iroh endpoint is dialled: the session and its live sessions
    /// reach it there.
    address: PageHostAddress,
    /// The person's chosen name, for history and presence; empty for none.
    #[serde(default)]
    display_name: String,
    /// The revocations this person's root signed, as the page kept them: the
    /// session announces them to its host with its certificate, so a host
    /// refuses a device of theirs from the moment each entry names.
    #[serde(default)]
    revocations: Vec<RevocationEntry>,
    /// The workspace this device enters through its entry as its session
    /// opens ([`entry_request`]), when the host has not admitted it yet.
    #[serde(default)]
    entering: Option<tilo_server_protocol::entering::Entering>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Certified<'a> {
    cert: &'a DeviceCert,
    #[serde(rename = "hostCert")]
    server_cert: &'a DeviceCert,
    /// The name the server gave the device, which the page shows for it.
    label: &'a str,
}

async fn post_decoded<T: serde::de::DeserializeOwned>(
    post: &PageHttp,
    route: &str,
    body: Vec<u8>,
) -> Result<T, JsError> {
    let path = format!("{}{route}", tilo_server_protocol::web::API_PREFIX);
    let (status, bytes) = post.post(&path, body).await.map_err(js_error)?;
    if status != 200 {
        return Err(js_error(format!("host answered {status}")));
    }
    postcard::from_bytes(&bytes).map_err(js_error)
}

/// The host `address` names (`hostInfo().address`, JSON).
pub(crate) fn server_at(address: &str) -> Result<ServerAddress, JsError> {
    Ok(dialled(serde_json::from_str(address).map_err(js_error)?))
}

/// The host at `address`, as the peer dials it.
fn dialled(address: PageHostAddress) -> ServerAddress {
    ServerAddress::published(&address.into())
}

/// What the host publishes before certification: its certificate. Resolves
/// to JSON.
#[wasm_bindgen(js_name = hostInfo)]
pub async fn server_info(post: Function) -> Result<String, JsError> {
    let info: tilo_server_protocol::web::ServerInfo =
        post_decoded(&PageHttp(post), "/host", Vec::new()).await?;
    serde_json::to_string(&PageHost::from(info)).map_err(js_error)
}

/// Whether the owner of `workspace` (its id) deleted it, as the host `post` reaches says to
/// any device that names it ([`tilo_server_client::https::workspace_deleted`]).
#[wasm_bindgen(js_name = workspaceDeleted)]
pub async fn workspace_deleted(post: Function, workspace: String) -> Result<bool, JsError> {
    tilo_server_client::https::workspace_deleted(&PageHttp(post), &workspace)
        .await
        .map_err(js_error)
}

/// Certify this page's device key with the host its address names, as the
/// answer to the pairing invite `inviteHex` (16 bytes as hex) the person's
/// link named, if any. Calls `onPairingCode(code)` once the host queued the
/// request. Resolves to `{cert, hostCert, label}` JSON for
/// [`WasmPeer::start`], `label` being the name the host gave the device.
#[wasm_bindgen(js_name = certifyDevice)]
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "the page's HTTP client stays on its one thread"
)]
pub async fn certify_device(
    user_public_key_hex: String,
    label: String,
    invite_hex: Option<String>,
    device_key: JsValue,
    on_pairing_code: Function,
    address: String,
) -> Result<String, JsError> {
    let signer = PageSigner::from_js(&device_key)?;
    let user: [u8; 32] = hex::decode(user_public_key_hex)
        .map_err(js_error)?
        .try_into()
        .map_err(|_| js_error("user public key must be 32 bytes"))?;
    let invite: [u8; 16] = match invite_hex {
        Some(text) => hex::decode(text)
            .map_err(js_error)?
            .try_into()
            .map_err(|_| js_error("a pairing invite is 16 bytes"))?,
        None => [0; 16],
    };
    let host = server_at(&address)?;
    let (cert, server_cert, label) =
        tilo_enrollment::requester::certify(&host, user, &label, invite, &signer, |code| {
            let _ = on_pairing_code.call1(&JsValue::NULL, &code.into());
        })
        .await
        .map_err(js_error)?;
    serde_json::to_string(&Certified {
        cert: &cert,
        server_cert: &server_cert,
        label: &label,
    })
    .map_err(js_error)
}

pub(crate) fn device_key(text: &str) -> Result<[u8; 32], JsError> {
    hex::decode(text)
        .map_err(js_error)?
        .try_into()
        .map_err(|_| js_error("device public key must be 32 bytes"))
}

/// The key that seals the profile's channel state at rest, kept by the platform
/// secret store: the same store the session's MLS vault keeps its master key
/// in, so a page that runs the session finds its vault already open.
///
/// `namespace` names the profile and the device it belongs to.
///
/// # Errors
/// The secret store is unavailable, or the stored key is not 32 bytes.
#[wasm_bindgen(js_name = channelVault)]
pub async fn channel_vault(namespace: String) -> Result<Vec<u8>, JsError> {
    // The store answers from page memory once its secrets are loaded there; the entry page asks
    // before anything else has loaded them.
    tilo_secrets::web::mirror()
        .await
        .map_err(|error| js_error(error.to_string()))?;
    let mut candidate = [0_u8; 32];
    getrandom::fill(&mut candidate).map_err(|error| js_error(error.to_string()))?;
    let stored = tilo_secrets::web::get_or_create(
        CHANNEL_VAULT_APP,
        &format!("mls-master-{namespace}"),
        Some(&candidate),
    )
    .await
    .map_err(|error| js_error(error.to_string()))?;
    let bytes = stored.expose();
    if bytes.len() != 32 {
        return Err(js_error("the stored channel vault has the wrong length"));
    }
    Ok(bytes.to_vec())
}

/// Certify `devicePublicKeyHex` with the user root the page holds (`root`:
/// `{publicKey, sign}` from `identity.mjs`) as `role`: `browser` (a
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
        "browser" => tilo_enrollment::certification::IssuedRole::Browser,
        "node" | "session" => tilo_enrollment::certification::IssuedRole::Device,
        _ => return Err(js_error("role must be browser, node or session")),
    };
    let root = PageSigner::from_js(&root)?;
    let cert = tilo_enrollment::certification::issue_device_cert(
        &root,
        device_key(&device_public_key_hex)?,
        &label,
        role,
    )
    .await
    .map_err(js_error)?;
    serde_json::to_string(&cert).map_err(js_error)
}

fn join_reply_json(reply: tilo_server_protocol::workspaces::JoinReply) -> serde_json::Value {
    use tilo_server_protocol::workspaces::JoinReply;
    match reply {
        JoinReply::Invitation {
            workspace,
            workspace_id,
            name,
            role,
            expires_at_ns,
            inviter,
            members,
            administrator,
            owner,
            wait_list,
        } => serde_json::json!({
            "kind": "invitation", "workspace": workspace, "workspaceId": workspace_id.to_string(),
            "name": name, "role": role, "expiresAtNs": expires_at_ns.to_string(),
            "members": members, "administrator": administrator, "owner": owner, "waitList": wait_list,
            "inviter": inviter.map(|inviter| serde_json::json!({
                "name": inviter.name, "userPublicKey": hex::encode(inviter.user_public_key),
            })),
        }),
        JoinReply::Admitted {
            workspace,
            workspace_id,
            name,
            role,
        } => serde_json::json!({
            "kind": "admitted", "workspace": workspace, "workspaceId": workspace_id.to_string(),
            "name": name, "role": role,
        }),
        JoinReply::Refused(reason) => serde_json::json!({"kind": "refused", "reason": reason}),
    }
}

fn request_id(text: &str) -> Result<[u8; 32], JsError> {
    hex::decode(text)
        .map_err(js_error)?
        .try_into()
        .map_err(|_| js_error("request id must be 32 bytes"))
}

/// The invitation a link's `secret` names, as the page takes it: the
/// invitation `workspace` keeps at the host `host` (its web origin), asked of
/// that host by the secret a short link carries. An invitation's own text is
/// answered as it came, so every link joins the same way. Resolves to the
/// invitation's text, as [`inspect_invitation`] and [`invitation_entry`] take
/// it.
///
/// # Errors
///
/// The host could not be reached, or keeps no invitation under the secret
/// any more: spent, lapsed or taken back.
#[wasm_bindgen(js_name = openInvitation)]
pub async fn open_invitation(
    host: String,
    workspace: String,
    secret: String,
) -> Result<String, JsError> {
    tilo_workspace_directory::kept_invitation(&host, &workspace, &secret)
        .await
        .map_err(js_error)
}

/// What `invitation` (the signed invitation's bytes) offers at the host
/// `post` reaches: `{kind: 'invitation', workspace, workspaceId, name, role,
/// administrator, expiresAtNs}`, or `{kind: 'refused', reason}`. Resolves
/// to JSON. `administrator` says the invitation also makes its holder an
/// administrator, read from its own signed layout: the host it admits into
/// checks it whole.
#[wasm_bindgen(js_name = inspectInvitation)]
pub async fn inspect_invitation(post: Function, invitation: Vec<u8>) -> Result<String, JsError> {
    let reply = tilo_server_client::https::invitation(&PageHttp(post), &invitation)
        .await
        .map_err(js_error)?;
    let mut answer = join_reply_json(reply);
    if let Some(fields) = answer.as_object_mut()
        && fields.get("kind").and_then(serde_json::Value::as_str) == Some("invitation")
    {
        fields.insert(
            "administrator".into(),
            tilo_workspace_join::makes_administrator(&invitation).into(),
        );
    }
    Ok(answer.to_string())
}

/// Which certificate the page's session starts with as it comes back to the host whose own
/// certificate is `hostCert` (JSON), through the one rule every device keeps
/// ([`tilo_session_client::device::reconnecting`]): `kept` is the `{cert, hostCert}` this device
/// kept, `admitted` the one an entry just admitted it under, and `withRoot` whether the page holds
/// the person's root to certify anew. Answers `admitted`, `adopt-host`, `kept`, `certify` or
/// `host-changed`. Kept text that is no certificate is no certificate kept.
///
/// # Errors
///
/// Throws for a host certificate that is none, or an admitted certificate that is none.
#[wasm_bindgen(js_name = reconnectCertificate)]
pub fn reconnect_certificate(
    kept: Option<String>,
    admitted: Option<String>,
    server_cert: &str,
    with_root: bool,
) -> Result<String, JsError> {
    use tilo_session_client::device::{Held, reconnecting};
    let host: DeviceCert = serde_json::from_str(server_cert).map_err(js_error)?;
    let kept: Option<Held> = kept.and_then(|text| serde_json::from_str(&text).ok());
    let admitted: Option<Held> = admitted
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(js_error)?;
    let answer = reconnecting(
        kept.as_ref(),
        admitted.as_ref(),
        &host,
        with_root,
        tilo_clock::unix_now_ns(),
    );
    serde_json::to_value(answer)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| js_error("the answer has no name"))
}

/// What a page says as it enters a workspace ([`entry_request`], [`invitation_entry`]): the
/// certificate this device holds (`{cert, hostCert}` as the page keeps it, or as pairing certified
/// it), the label a certificate made now carries, the name the workspace's people see, and the
/// pre-admission's one-time token an invitation was issued under.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Accepting {
    #[serde(default)]
    held: Option<String>,
    label: String,
    name: String,
    #[serde(default)]
    admission_token: Option<String>,
}

/// What this page's device (`device key`) enters the workspace `invitation` (its signed bytes)
/// admits into with, on the host whose certificate is `hostCert` (JSON), through the one entry
/// every device takes ([`tilo_session_client::device::invitation_entry`]): the certificate it enters
/// under (the one it holds from that host where it still serves, else one the person's root
/// `root` certifies now) and the entry the session it starts asks for as it opens, carrying the
/// invitation and `options.admissionToken`. `options` is [`Accepting`] JSON. Resolves to
/// `{certificate: {cert, hostCert}, entering}` JSON; [`WasmPeer::start`] takes both, and the
/// host's answer arrives in the session's snapshot (`entry`).
///
/// # Errors
///
/// Rejects with an `Error` named `DeviceRefusal` whose `code` is `no_certificate`, or
/// `not_admitted` for an invitation that names no workspace.
#[wasm_bindgen(js_name = invitationEntry)]
pub async fn invitation_entry(
    server_cert: String,
    invitation: Vec<u8>,
    device_key: JsValue,
    root: JsValue,
    options: String,
) -> Result<String, JsValue> {
    use tilo_session_client::device::{Entering, Held};
    let options: Accepting = serde_json::from_str(&options).map_err(js_error)?;
    let held: Option<Held> = options
        .held
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(js_error)?;
    let server_cert: DeviceCert = serde_json::from_str(&server_cert).map_err(js_error)?;
    let device = PageSigner::from_js(&device_key)?;
    let root = if root.is_undefined() || root.is_null() {
        None
    } else {
        Some(PageSigner::from_js(&root)?)
    };
    let (cert, entering) = tilo_session_client::device::invitation_entry(
        &server_cert,
        &invitation,
        &device,
        Entering {
            held: held.as_ref(),
            root: root.as_ref().map(|root| root as &dyn DeviceSigner),
            label: &options.label,
            name: &options.name,
            token: options.admission_token.as_deref().map(str::as_bytes),
        },
    )
    .await
    .map_err(|refusal| refusal_error(&refusal))?;
    Ok(serde_json::json!({
        "certificate": {"cert": cert, "hostCert": server_cert},
        "entering": entering,
    })
    .to_string())
}

/// What this page's device (`device key`) enters `workspace` through its entry with, on the host whose
/// certificate is `hostCert` (JSON), through the one entry every device takes
/// ([`tilo_session_client::device::entry_request`]): the certificate it enters under (the one it holds
/// from that host where it still serves, else one the person's root `root` certifies now) and the
/// entry the session it starts asks for as it opens. `options` is [`Accepting`] JSON. Resolves to
/// `{certificate: {cert, hostCert}, entering}` JSON; [`WasmPeer::start`] takes both, and the host's
/// answer arrives in the session's snapshot (`entry`).
///
/// # Errors
///
/// Rejects with an `Error` named `DeviceRefusal` whose `code` is `no_certificate`.
#[wasm_bindgen(js_name = entryRequest)]
pub async fn entry_request(
    server_cert: String,
    workspace: String,
    device_key: JsValue,
    root: JsValue,
    options: String,
) -> Result<String, JsValue> {
    use tilo_session_client::device::{Entering, Held};
    let options: Accepting = serde_json::from_str(&options).map_err(js_error)?;
    let held: Option<Held> = options
        .held
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(js_error)?;
    let server_cert: DeviceCert = serde_json::from_str(&server_cert).map_err(js_error)?;
    let device = PageSigner::from_js(&device_key)?;
    let root = if root.is_undefined() || root.is_null() {
        None
    } else {
        Some(PageSigner::from_js(&root)?)
    };
    let (cert, entering) = tilo_session_client::device::entry_request(
        &server_cert,
        &workspace,
        &device,
        Entering {
            held: held.as_ref(),
            root: root.as_ref().map(|root| root as &dyn DeviceSigner),
            label: &options.label,
            name: &options.name,
            token: None,
        },
    )
    .await
    .map_err(|refusal| refusal_error(&refusal))?;
    Ok(serde_json::json!({
        "certificate": {"cert": cert, "hostCert": server_cert},
        "entering": entering,
    })
    .to_string())
}

/// The canonical encoding of a certificate (JSON in): what a native device
/// keeps on disk.
#[wasm_bindgen(js_name = deviceCertCanonical)]
pub fn device_cert_canonical(cert_json: &str) -> Result<Vec<u8>, JsError> {
    let cert: DeviceCert = serde_json::from_str(cert_json).map_err(js_error)?;
    tilo_user_identity::canonical::canonical_bytes(&cert).map_err(js_error)
}

/// Certify this page's device key (`device key`) with the user root the page
/// holds, for the host whose certificate the page's own `hostInfo` named
/// (`host_cert`, JSON), which must be a host of that root. Resolves to
/// `{cert, hostCert}` JSON, as [`certify_device`] does.
#[wasm_bindgen(js_name = certifyWithRoot)]
pub async fn certify_with_root(
    root: JsValue,
    device_key: JsValue,
    label: String,
    host_cert: String,
) -> Result<String, JsError> {
    let root = PageSigner::from_js(&root)?;
    let signer = PageSigner::from_js(&device_key)?;
    let server_cert: DeviceCert = serde_json::from_str(&host_cert).map_err(js_error)?;
    let (cert, server_cert) =
        tilo_enrollment::certification::certify_with_root(&root, &signer, &label, &server_cert)
            .await
            .map_err(js_error)?;
    serde_json::to_string(&Certified {
        cert: &cert,
        server_cert: &server_cert,
        label: &label,
    })
    .map_err(js_error)
}

/// Revoke `keyHex`, a device key of this person's, with the root the page
/// holds (`root`): resolves to the root-signed revocation as JSON, which the
/// page keeps beside the person. It
/// is announced to every session of theirs at once, so their hosts refuse
/// what the key signs from now on and keep valid what it signed before.
/// `reason` is why: `retired`, `lost`, `stolen` or `compromised`.
#[wasm_bindgen(js_name = revokeDevice)]
pub async fn revoke_device(
    root: JsValue,
    key_hex: String,
    reason: String,
) -> Result<String, JsError> {
    let root = PageSigner::from_js(&root)?;
    let reason = match reason.as_str() {
        "retired" => RevocationReason::Retired,
        "lost" => RevocationReason::Lost,
        "stolen" => RevocationReason::Stolen,
        "compromised" => RevocationReason::Compromised,
        _ => {
            return Err(JsError::new(
                "a device's access ends as retired, lost, stolen or compromised",
            ));
        }
    };
    let entry = tilo_server_client::revoke_device(
        &root,
        device_key(&key_hex)?,
        tilo_clock::unix_now_ns(),
        reason,
    )
    .await
    .map_err(js_error)?;
    for handle in crate::peer_sync::handles_of(&hex::encode(entry.user_public_key)) {
        handle.send(Cmd::Revoke(entry.clone())).map_err(js_error)?;
    }
    serde_json::to_string(&entry).map_err(js_error)
}

/// The host a page pairs its person's new devices through, as that person's
/// root: over the session the page holds there ([`WasmPeer::pairing`]), or,
/// before it holds one, from an endpoint of its own it keeps while it pairs.
#[wasm_bindgen(js_name = PairingHost)]
pub struct PairingHost(PairingReach);

/// Where a page's pairing commands reach the host.
enum PairingReach {
    /// Over the session the page already holds with the host.
    Session(tilo_session_client::SessionMemberServer),
    /// From an endpoint bound for pairing alone: the page holds no session
    /// with this host yet. Boxed: it carries its endpoint's state, where the
    /// session is a handle.
    Own(Box<tilo_server_client::MemberApprover>),
}

impl tilo_server_client::MemberServer for PairingReach {
    async fn ask(
        &self,
        approval: &tilo_server_protocol::certify::MemberApproval,
    ) -> Result<tilo_server_protocol::certify::ApproverReply, tilo_server_client::CertifyError>
    {
        match self {
            Self::Session(session) => session.ask(approval).await,
            Self::Own(approver) => approver.ask(approval).await,
        }
    }
}

#[wasm_bindgen(js_class = PairingHost)]
impl PairingHost {
    /// The host `address` names (`hostInfo().address`, JSON), for a page
    /// that holds no session there.
    ///
    /// # Errors
    /// `address` is not a host's address.
    #[wasm_bindgen(constructor)]
    pub fn new(address: &str) -> Result<Self, JsError> {
        Ok(Self(PairingReach::Own(Box::new(
            tilo_server_client::MemberApprover::new(server_at(address)?),
        ))))
    }

    /// The new devices waiting here for the member root the page holds
    /// (`root`) to pair them: `[{digest, pairingCode, label, device, invite,
    /// requestedAtMs, signIn, capability}]` JSON, keys, digest and invite as hex
    /// (`invite` null for a request no invite asked for). `signIn` is set for a
    /// tab a page's sign-in ticket expects, and `capability` then says what
    /// certifying it makes it (`null` for any other device).
    ///
    /// # Errors
    /// The host was not reached, or turned the root away.
    pub async fn pending(&self, root: JsValue) -> Result<String, JsError> {
        let root = PageSigner::from_js(&root)?;
        let pending = tilo_server_client::pending_pairings(&self.0, &root)
            .await
            .map_err(js_error)?;
        Ok(serde_json::Value::Array(
            pending
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "digest": hex::encode(entry.request_digest),
                        "pairingCode": entry.pairing_code,
                        "label": entry.request.device_label,
                        "device": hex::encode(entry.request.device_public_key),
                        "invite": (entry.request.invite != [0; 16])
                            .then(|| hex::encode(entry.request.invite)),
                        "requestedAtMs": entry.requested_at_ns / 1_000_000,
                        "signIn": entry.sign_in,
                        "capability": entry
                            .sign_in
                            .then(tilo_device_identity::login::tab_capability),
                    })
                })
                .collect(),
        )
        .to_string())
    }

    /// Pair (`approved`) or turn away the waiting device `digest_hex`, with
    /// the member root the page holds. Resolves to the certificate issued
    /// (JSON), or null when turned away.
    ///
    /// # Errors
    /// The host was not reached, the device no longer waits, or the host
    /// refused the answer.
    pub async fn answer(
        &self,
        root: JsValue,
        digest_hex: String,
        approved: bool,
    ) -> Result<Option<String>, JsError> {
        let root = PageSigner::from_js(&root)?;
        let digest: [u8; 32] = hex::decode(&digest_hex)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| JsError::new("a request digest is 64 hex characters"))?;
        let issued =
            tilo_enrollment::certification::answer_pairing(&self.0, &root, digest, approved)
                .await
                .map_err(js_error)?;
        issued
            .map(|cert| serde_json::to_string(&cert).map_err(js_error))
            .transpose()
    }

    /// Let go of the host: the page pairs nothing more through it.
    pub async fn close(&self) {
        if let PairingReach::Own(approver) = &self.0 {
            approver.close().await;
        }
    }
}

// ---- Previews: what a member's editor shows before the change lands -----

/// One record a live event names, as its wire carries it.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsRecord {
    pub(crate) app: String,
    pub(crate) record: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum JsPreviewOp {
    Insert { at: String, text: String },
    Delete { at: String, len: u32 },
}

fn record_to_js(record: LiveResource) -> JsRecord {
    JsRecord {
        app: record.app,
        record: record.record,
    }
}

fn record_from_js(record: JsRecord) -> LiveResource {
    LiveResource {
        app: record.app,
        record: record.record,
    }
}

fn anchor(text: &str) -> Result<LiveAnchor, JsError> {
    Ok(LiveAnchor {
        cursor: hex::decode(text).map_err(js_error)?,
    })
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

pub(crate) fn workspace_id(text: &str) -> Result<WorkspaceId, JsError> {
    tilo_server_protocol::WorkspaceId::parse(text).ok_or_else(|| js_error("not a workspace uuid"))
}

/// One live (non-presence) message another member published.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsLiveEvent {
    Preview {
        publisher: String,
        record: JsRecord,
        seq: u64,
        ops: Vec<JsPreviewOp>,
    },
    PreviewEnd {
        publisher: String,
        record: JsRecord,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsDurable {
    Durable { record: JsRecord },
}

/// Encode preview operations (JSON array) for a record (JSON).
///
/// # Errors
///
/// Throws for operations outside the wire bounds.
fn encode_live_preview(record_json: &str, seq: u64, ops_json: &str) -> Result<Vec<u8>, JsError> {
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
fn encode_live_preview_end(record_json: &str) -> Result<Vec<u8>, JsError> {
    let record = record_from_js(serde_json::from_str(record_json).map_err(js_error)?);
    LiveFrame::Ops(OpsFrame::End { record })
        .encode()
        .map_err(js_error)
}

#[path = "peer_js_state.rs"]
mod js_state;
use js_state::{
    JsVoiceEvent, JsWanted, is_durable, is_live, is_voice, js_channels, js_layer, js_source,
    layer_of_name, own_revocations, source_of_name, workspace_of,
};

/// A running certified peer.
#[wasm_bindgen]
pub struct WasmPeer {
    id: u32,
    device_key: [u8; 32],
    /// This tab's device signer, as the session signs: what approves a computer a ticket named.
    signer: Arc<dyn DeviceSigner>,
    /// This tab's device certificate, signed by the person's root: the approver of a computer.
    own_cert: DeviceCert,
    pub(crate) handle: Link,
    /// This page's device in its person's workspaces, over the same session: how it enters them.
    device: Device,
    durable_events: Events,
    live_events: Events,
    voice_events: Events,
    /// The person's workspace list as their host sends it: what the
    /// switcher offers without asking the host again.
    personal: tilo_session_client::PersonalWorkspaces,
    /// Joined workspaces by workspace, to name them in call events.
    voice_workspaces: RefCell<std::collections::HashMap<WorkspaceId, String>>,
    listeners: Rc<RefCell<Vec<Function>>>,
    /// The workspace the person takes part in through this page, once joined
    /// as a person: the one that names them.
    speaking_workspace: RefCell<Option<String>>,
}

/// Where a page's peer keeps its record: the profile `namespace`'s durable store, sealed under
/// its `channelVault`; else the stored `record` bytes, saved through `save`.
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "the session takes shared handles; in the page they stay on its one thread"
)]
fn peer_store(
    namespace: Option<String>,
    record: Option<Vec<u8>>,
    save: Option<Function>,
    channel_vault: Option<&[u8]>,
    user: &[u8; 32],
) -> Result<Arc<dyn PeerStore>, JsError> {
    if let Some(namespace) = namespace {
        let channel_vault: [u8; 32] = channel_vault
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| js_error("a profile peer needs its 32-byte channel vault"))?;
        return Ok(Arc::new(DurablePeerStore::open(
            &namespace,
            user,
            channel_vault,
        )?));
    }
    let record = record
        .map(|bytes| postcard::from_bytes::<PeerEntry>(&bytes))
        .transpose()
        .map_err(js_error)?;
    let save = save.ok_or_else(|| js_error("a peer needs a namespace or a save callback"))?;
    Ok(Arc::new(PageStore {
        current: RefCell::new(record),
        save,
    }))
}

/// Tell the server of `workspace` its member, the page's person, goes by `name` from now on, the
/// name they gave themselves: asked only when they give one, never with one the page drew.
fn set_member_name(handle: &Link, workspace: &str, name: &str) {
    if name.trim().is_empty() {
        return;
    }
    let request = tilo_server_protocol::workspaces::WorkspacesRequest::Name {
        workspace: workspace.to_owned(),
        name: name.to_owned(),
    };
    let _ = handle.ask_workspaces(request);
}

impl WasmPeer {
    /// Ask the host `request`, and resolve to its answer.
    pub(crate) async fn workspaces_request(
        &self,
        request: tilo_server_protocol::workspaces::WorkspacesRequest,
    ) -> Result<tilo_server_protocol::workspaces::WorkspacesReply, JsError> {
        self.handle
            .ask_workspaces(request)
            .map_err(js_error)?
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)
    }

    /// The person's workspaces as their host lists them, in the page's shape: what every device
    /// of theirs reads ([`tilo_session_client::PersonalWorkspaces`]), so a page offers every
    /// workspace of theirs, not only the ones this browser's storage still remembers.
    pub(crate) async fn personal_workspaces(&self) -> Result<serde_json::Value, JsError> {
        let list = self
            .personal
            .read(&self.device)
            .await
            .map_err(|refusal| js_error(refusal.message))?;
        serde_json::to_value(&list).map_err(js_error)
    }
}

#[wasm_bindgen]
impl WasmPeer {
    /// Approve the computer that asked with a ticket this tab's host serves: sign the member
    /// certificate the host built for it with this tab's root (`root`), and sign the question with
    /// this tab's device key. Resolves to the [`Approved`] JSON the host checks with
    /// `check_approval` before it answers the computer.
    ///
    /// `asked` is the [`Asked`] JSON the host handed the page.
    ///
    /// # Errors
    ///
    /// The question is malformed, the root is not the person's, or a signature failed.
    #[wasm_bindgen(js_name = approveAsked)]
    pub async fn approve_asked(&self, root: JsValue, asked: String) -> Result<String, JsError> {
        let asked: Asked = serde_json::from_str(&asked).map_err(js_error)?;
        if asked.user_public_key != self.own_cert.user_public_key {
            return Err(js_error("this tab approves computers of the person it is"));
        }
        let root = PageSigner::from_js(&root)?;
        if root.device_public_key() != asked.user_public_key {
            return Err(js_error(
                "the root that signs the certificate is not the person's",
            ));
        }
        let mut cert = asked.cert.clone();
        tilo_user_identity::sign_device_cert_async(&mut cert, &root)
            .await
            .map_err(js_error)?;
        let signature = self
            .signer
            .sign(asked.digest.to_vec())
            .await
            .map_err(|error| js_error(error.0))?;
        serde_json::to_string(&Approved {
            cert,
            approver: self.own_cert.clone(),
            signature,
        })
        .map_err(js_error)
    }

    /// Start a session. `configJson` is `{cert, hostCert}` from
    /// [`certify_device`], optionally with `displayName`. With `namespace`
    /// (`{profile, device}` JSON, whose storage the page has opened) the
    /// peer record lives in that profile's durable store; otherwise `record`
    /// is the stored record bytes (or `undefined`) and `save(bytes)` writes
    /// it. `post(path, session, body)` reaches the host origin.
    /// `channelVault` (32 bytes, from [`channel_vault`]) seals the profile's
    /// channel state at rest; a peer with a namespace needs it. `keep` is the
    /// page a session that replaces another opens as, when that page certifies
    /// its device again: its mounts name the page by one id for as long as it
    /// is open, and the session under it may be replaced.
    ///
    /// # Errors
    ///
    /// Throws for a malformed configuration, device key object, namespace or
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
        device_key: &JsValue,
        namespace: Option<String>,
        record: Option<Vec<u8>>,
        save: Option<Function>,
        channel_vault: Option<Vec<u8>>,
        keep: Option<u32>,
    ) -> Result<WasmPeer, JsError> {
        let config: ConnectConfig = serde_json::from_str(config_json).map_err(js_error)?;
        let signer = PageSigner::from_js(device_key)?;
        if signer.device_public_key() != config.cert.device_public_key {
            return Err(js_error(
                "device key does not hold the certified device key",
            ));
        }
        let signer: Arc<dyn DeviceSigner> = Arc::new(signer);
        let kept_signer = Arc::clone(&signer);
        let store = peer_store(
            namespace,
            record,
            save,
            channel_vault.as_deref(),
            &config.cert.user_public_key,
        )?;
        let device_key = config.cert.device_public_key;
        let reporter_cert = config.cert.clone();
        let owner_cert = config.server_cert.clone();
        let display_name = config.display_name;
        let revocations = own_revocations(config.revocations, &config.cert)?;
        let handle = tilo_session_client::start_session(
            PeerConfig {
                host: dialled(config.address),
                cert: config.cert,
                server_cert: config.server_cert,
                revocations,
                entering: config.entering,
            },
            signer,
            store,
        );
        let device = Device::of(
            handle.clone(),
            reporter_cert.clone(),
            owner_cert.clone(),
            &reporter_cert.device_label,
        );
        // A page that certifies its device again opens as the page it was, so
        // the mounts it made keep reaching it; its earlier session let the id
        // go with its registration before this one starts.
        let id = page_id(keep);
        crate::peer_sync::register(id, &handle, &reporter_cert, &owner_cert, &display_name);
        // A lobby started beside the workspace's peer does not speak for the
        // page's person.
        crate::page_person::claim(id);
        // The person's lists as the host sends them: read beside what this
        // page remembered, never asked for again.
        let personal = tilo_session_client::PersonalWorkspaces::follow(&handle);
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
            signer: kept_signer,
            own_cert: reporter_cert,
            durable_events: handle.events_where(is_durable),
            live_events: handle.events_where(is_live),
            voice_events: handle.events_where(is_voice),
            personal,
            voice_workspaces: RefCell::default(),
            handle,
            device,
            listeners,
            speaking_workspace: RefCell::default(),
        })
    }

    /// `PeerSnapshot` JSON.
    pub fn snapshot(&self) -> String {
        serde_json::to_string(&self.handle.snapshot()).unwrap_or_default()
    }

    /// This page's channel state as text: whom each workspace follows, which
    /// senders may govern it, and each route's membership.
    #[wasm_bindgen(js_name = channelDiagnostics)]
    pub fn channel_diagnostics(&self) -> String {
        self.handle.status().key_state
    }

    /// Call `listener()` whenever the snapshot may have changed.
    pub fn subscribe(&self, listener: Function) {
        self.listeners.borrow_mut().push(listener);
    }

    /// Join a workspace (uuid). The page joins it once its host governs it,
    /// creating it there as `name` (the workspace's name, `Workspace` when the
    /// page knows none) unless the host has it.
    #[wasm_bindgen(js_name = joinWorkspace)]
    pub fn join_workspace(&self, workspace: &str, name: Option<String>) -> Result<(), JsError> {
        let workspace_id = workspace_id(workspace)?;
        let name = name.filter(|name| !name.trim().is_empty());
        self.handle
            .join_placed(workspace, name.as_deref().unwrap_or("Workspace"))
            .map_err(js_error)?;
        self.voice_workspaces
            .borrow_mut()
            .insert(workspace_id, workspace.to_owned());
        // The name the page shows its person by is never written into the workspace here: the
        // workspace names them by the name they gave themselves, which their server keeps
        // ([`Self::name_self`]); one the page drew from their key names nobody.
        if crate::page_person::speaks(self.id) {
            *self.speaking_workspace.borrow_mut() = Some(workspace.to_owned());
        }
        Ok(())
    }

    /// This peer in the page, for `mountDesktop({peer})`: mounts in its
    /// workspace replicate their records and desktop through it.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Pre-admit whoever redeems the invitation this issues and presents
    /// the token whose SHA-256 the grant names: `options` is JSON
    /// `{requestId, workspace, role, tokenSha256, label, expiresInSeconds,
    /// inviterName?}` (hex ids, `role` `viewer`/`commenter`/`editor`/
    /// `moderator`); the owner's `root` signs the grant. Resolves to the
    /// invitation's text.
    ///
    /// # Errors
    ///
    /// Rejects for malformed options, when the root does not sign, or when the
    /// host refuses.
    /// Pair this person's new devices through this session's host, as the
    /// person's root device key the page holds: over this session, with no
    /// connection of its own.
    pub fn pairing(&self) -> PairingHost {
        PairingHost(PairingReach::Session(tilo_session_client::member_server(
            &self.handle,
        )))
    }

    #[wasm_bindgen(js_name = preAdmit)]
    pub async fn pre_admit(&self, root: JsValue, options: String) -> Result<String, JsError> {
        use tilo_server_client::DeviceSigner as _;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Options {
            request_id: String,
            workspace: String,
            role: String,
            token_sha256: String,
            label: String,
            expires_in_seconds: u64,
            inviter_name: Option<String>,
        }
        let options: Options = serde_json::from_str(&options).map_err(js_error)?;
        if options.label.trim().is_empty() {
            return Err(js_error("a pre-admission needs a label"));
        }
        let seconds = crate::workspaces::seconds(Some(options.expires_in_seconds))?;
        let root = PageSigner::from_js(&root)?;
        let now_ns = tilo_clock::unix_now_ns();
        let mut grant = tilo_server_protocol::workspaces::PreAdmission {
            workspace: options.workspace,
            role: crate::workspaces::wire_role(&options.role)?,
            token_sha256: device_key(&options.token_sha256)?,
            label: options.label,
            expires_at_ns: now_ns.saturating_add(seconds.saturating_mul(1_000_000_000)),
            owner_user_public_key: root.device_public_key(),
            signature: [0; 64],
        };
        grant.signature = root
            .sign(grant.digest().map_err(js_error)?.to_vec())
            .await
            .map_err(|error| js_error(error.0))?;
        let reply = self
            .workspaces_request(
                tilo_server_protocol::workspaces::WorkspacesRequest::PreAdmit {
                    request_id: request_id(&options.request_id)?,
                    grant,
                    inviter_name: options.inviter_name,
                },
            )
            .await?;
        crate::workspaces::invitation_text(reply)
    }

    /// Say this page's `value` of itself in `workspace` (the live layer's
    /// value, JSON: `{member, place, layout, focus, selection, pointer,
    /// chat, reaction, wave, following}`): the board keeps what changed and
    /// publishes it on its own schedule, as every device's presence goes.
    /// Resolves to when it next has something to say, in `Date.now()` ms
    /// (`0` for nothing).
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace, or a value outside the wire.
    #[wasm_bindgen(js_name = sayPresence)]
    pub fn say_presence(&self, workspace: &str, value: &str) -> Result<f64, JsError> {
        let value: JsPresence = serde_json::from_str(value).map_err(js_error)?;
        say(self.id, workspace, &value)
    }

    /// Publish what this page's board for `workspace` owes now (keepalives,
    /// repeats), and resolve when it next has something to say (`0` for
    /// nothing).
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace.
    #[wasm_bindgen(js_name = presenceDue)]
    pub fn presence_due(&self, workspace: &str) -> Result<f64, JsError> {
        due(self.id, workspace)
    }

    /// Everyone here in `workspace`, as this page's board merged their
    /// words: a JSON array of `{publisher, epoch, agent, label, activity,
    /// colour, user, device, focus, place, following, pointer, selection,
    /// chat, reaction, layout, availability, call}`, or `null` while this
    /// page takes no part there.
    #[wasm_bindgen(js_name = presenceMembers)]
    pub fn presence_members(&self, workspace: &str) -> String {
        members(self.id, workspace).map_or_else(
            || "null".into(),
            |members| serde_json::to_string(&members).unwrap_or_else(|_| "null".into()),
        )
    }

    /// Leave `workspace`: the members take this page down now.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace.
    #[wasm_bindgen(js_name = leavePresence)]
    pub fn leave_presence(&self, workspace: &str) -> Result<(), JsError> {
        crate::peer_sync::leave(self.id, workspace)
    }

    /// Publish ephemeral `ops` (JSON array of `{kind: 'insert'|'delete', at,
    /// text|len}`) for `record` (JSON) in `workspace`: the keystroke preview
    /// the members' editors show before the change lands, sealed with the
    /// record's channel keys. Resolves `false` when this device holds none.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace, record or operation, or a closed
    /// session.
    #[wasm_bindgen(js_name = publishPreview)]
    pub async fn publish_preview(
        &self,
        workspace: &str,
        record: &str,
        seq: u64,
        ops: &str,
    ) -> Result<bool, JsError> {
        let workspace_id = workspace_id(workspace)?;
        let frame = encode_live_preview(record, seq, ops)?;
        let Some(sealed) = self.seal_live(workspace.to_owned(), frame).await? else {
            return Ok(false);
        };
        self.handle
            .send(Cmd::Live(workspace_id, sealed))
            .map_err(js_error)?;
        Ok(true)
    }

    /// End this device's previews of `record` in `workspace`: the members
    /// drop the ops they have not seen land.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or record, or a closed session.
    #[wasm_bindgen(js_name = endPreview)]
    pub async fn end_preview(&self, workspace: &str, record: &str) -> Result<(), JsError> {
        let workspace_id = workspace_id(workspace)?;
        let frame = encode_live_preview_end(record)?;
        if let Some(sealed) = self.seal_live(workspace.to_owned(), frame).await? {
            self.handle
                .send(Cmd::Live(workspace_id, sealed))
                .map_err(js_error)?;
        }
        Ok(())
    }

    /// Seal one encoded live frame of `workspace` with the channel keys of the
    /// record it names. `None` when the frame names no record or this device
    /// holds none of its keys: such a frame is never sent.
    async fn seal_live(
        &self,
        workspace: String,
        frame: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, JsError> {
        let (sealed, receipt) = tilo_session::sync::answer();
        self.handle
            .send(Cmd::SealLive(workspace_id(&workspace)?, frame, sealed))
            .map_err(js_error)?;
        receipt.await.map_err(|_| js_error("peer session closed"))
    }

    /// Join the call in voice channel `channel` of `workspace` (`name` what the channel is
    /// called, when the page knows): this page's presence there says it is in it, so every device
    /// of the workspace lists it in the channel, and the call's voice and pictures are the
    /// workspace's live session, which only its members open. Being in another call leaves it.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or a page that is not started.
    #[wasm_bindgen(js_name = callJoin)]
    pub fn call_join(
        &self,
        workspace: &str,
        channel: &str,
        name: Option<String>,
    ) -> Result<(), JsError> {
        let call = InCall {
            call: collab_types::presence::CallPresence {
                channel: channel.to_owned(),
                name: name.unwrap_or_default(),
                bridge: None,
            },
            camera: false,
            screen: false,
            muted: false,
            deafened: false,
        };
        crate::peer_sync::call(self.id, workspace, Some(call))
    }

    /// Leave the call of `workspace`: this page's presence there says it is in none.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or a page that is not started.
    #[wasm_bindgen(js_name = callLeave)]
    pub fn call_leave(&self, workspace: &str) -> Result<(), JsError> {
        crate::peer_sync::call(self.id, workspace, None)
    }

    /// Say what this page shows in its call of `workspace`: its camera (`camera`), a shared
    /// screen (`screen`), whether its microphone is off (`muted`) and whether it hears the call
    /// (`deafened`). Everyone in the workspace reads it from this page's presence. Nothing while
    /// the page is in no call there.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or a page that is not started.
    #[wasm_bindgen(js_name = callShow)]
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "the page's call says each of what it shows as its own flag, as its presence holds them"
    )]
    pub fn call_show(
        &self,
        workspace: &str,
        camera: bool,
        screen: bool,
        muted: bool,
        deafened: bool,
    ) -> Result<(), JsError> {
        let Some(call) = crate::peer_sync::own_call(self.id, workspace) else {
            return Ok(());
        };
        crate::peer_sync::call(
            self.id,
            workspace,
            Some(InCall {
                camera,
                screen,
                muted,
                deafened,
                ..call
            }),
        )
    }

    /// The voice channels of `workspace` anyone is in, as JSON `[{channel, name, members:
    /// [{device, user, publishes, camera, screen, muted, deafened}]}]` (`user` the person's root
    /// as hex, empty while nobody named them), this page among them while it is in a call there;
    /// `null` while the page takes no part in `workspace`.
    #[wasm_bindgen(js_name = callChannels)]
    pub fn call_channels(&self, workspace: &str) -> String {
        crate::peer_sync::channels(self.id, workspace).map_or_else(
            || "null".into(),
            |channels| {
                serde_json::to_string(&js_channels(channels)).unwrap_or_else(|_| "null".into())
            },
        )
    }

    /// What the calls this page is in carry, since the last call, as a JSON array of `{kind:
    /// 'heard', workspace, from, sequence, timestamp48k, opus}` (`opus` in hex), which the app
    /// plays, `{kind: 'picture', workspace, from, source, keyframe, object}` and `{kind:
    /// 'demand', workspace, wanted, keyframes}`.
    #[wasm_bindgen(js_name = takeVoiceEvents)]
    pub fn take_voice_events(&self) -> String {
        let out: Vec<JsVoiceEvent> = self
            .voice_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                Event::CallAudio {
                    ws: workspace_id,
                    from,
                    sequence,
                    timestamp_48k,
                    opus,
                } => Some(JsVoiceEvent::Heard {
                    workspace: workspace_of(workspace_id, &self.voice_workspaces),
                    from: hex::encode(from),
                    sequence,
                    timestamp48k: timestamp_48k,
                    opus: hex::encode(opus),
                }),
                Event::CallPicture {
                    ws: workspace_id,
                    from,
                    source,
                    keyframe,
                    object,
                } => Some(JsVoiceEvent::Picture {
                    workspace: workspace_of(workspace_id, &self.voice_workspaces),
                    from: hex::encode(from),
                    source: js_source(source),
                    keyframe,
                    object: hex::encode(object),
                }),
                Event::CallDemand {
                    ws: workspace_id,
                    wanted,
                    keyframes,
                } => {
                    let mut by_source: Vec<JsWanted> = Vec::new();
                    for (source, layer) in wanted {
                        let name = js_source(source);
                        match by_source.iter_mut().find(|entry| entry.source == name) {
                            Some(entry) => entry.layers.push(js_layer(layer)),
                            None => by_source.push(JsWanted {
                                source: name,
                                layers: vec![js_layer(layer)],
                            }),
                        }
                    }
                    Some(JsVoiceEvent::Demand {
                        workspace: workspace_of(workspace_id, &self.voice_workspaces),
                        wanted: by_source,
                        keyframes: keyframes.into_iter().map(js_source).collect(),
                    })
                }
                _ => None,
            })
            .collect();
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
    }

    /// Publish `frames` (JSON array of `{timestamp48k, opus}`, `opus` in hex)
    /// on `workspace`'s live session as this page's voice, in the order given:
    /// one voice object per frame, heard by every device in the call.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or frames, or a closed session.
    #[wasm_bindgen(js_name = callAudio)]
    pub fn call_audio(&self, workspace: &str, frames: &str) -> Result<(), JsError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Frame {
            timestamp48k: u64,
            opus: String,
        }
        let frames: Vec<Frame> = serde_json::from_str(frames).map_err(js_error)?;
        let frames: Vec<tilo_session::sync::CallFrame> = frames
            .into_iter()
            .filter_map(|frame| {
                Some(tilo_session::sync::CallFrame {
                    timestamp_48k: frame.timestamp48k,
                    opus: hex::decode(frame.opus).ok()?,
                })
            })
            .collect();
        self.handle
            .send(Cmd::CallAudio(workspace_id(workspace)?, frames))
            .map_err(js_error)
    }

    /// Publish `pictures` this page's camera and screen encoded on the live session
    /// of `workspace`, as JSON: `[{source, layer, sequence, keyframe,
    /// timestampUs, width, height, payload}]`, `source` and `layer` as
    /// `web/voice-video.mjs` names them and `payload` in hex. Every device
    /// showing the layer draws it.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or picture, or a closed session.
    #[wasm_bindgen(js_name = callPictures)]
    pub fn call_pictures(&self, workspace: &str, pictures: &str) -> Result<(), JsError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Picture {
            source: String,
            layer: String,
            sequence: u64,
            keyframe: bool,
            timestamp_us: u64,
            width: u16,
            height: u16,
            payload: String,
        }
        let pictures: Vec<Picture> = serde_json::from_str(pictures).map_err(js_error)?;
        let pictures: Vec<tilo_session::sync::CallPicture> = pictures
            .into_iter()
            .filter_map(|picture| {
                Some(tilo_session::sync::CallPicture {
                    source: source_of_name(&picture.source)?,
                    layer: layer_of_name(&picture.layer)?,
                    sequence: picture.sequence,
                    keyframe: picture.keyframe,
                    timestamp_us: picture.timestamp_us,
                    width: picture.width,
                    height: picture.height,
                    payload: hex::decode(picture.payload).ok()?,
                })
            })
            .collect();
        self.handle
            .send(Cmd::CallPictures(workspace_id(workspace)?, pictures))
            .map_err(js_error)
    }

    /// Show the devices' layers of `shown` on the live session of `workspace`, as
    /// JSON: `[{device, camera: [layer], screen: [layer]}]`, `device` in hex.
    /// The devices left out are no longer read, and their owners stop encoding
    /// the layers nobody shows.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or entry, or a closed session.
    #[wasm_bindgen(js_name = callShowing)]
    pub fn call_showing(&self, workspace: &str, shown: &str) -> Result<(), JsError> {
        #[derive(serde::Deserialize)]
        struct Shown {
            device: String,
            camera: Vec<String>,
            screen: Vec<String>,
        }
        let shown: Vec<Shown> = serde_json::from_str(shown).map_err(js_error)?;
        let shown: Vec<tilo_session::sync::CallShown> = shown
            .into_iter()
            .filter_map(|entry| {
                let device: [u8; 32] = hex::decode(&entry.device).ok()?.try_into().ok()?;
                Some(tilo_session::sync::CallShown {
                    device,
                    camera: entry
                        .camera
                        .iter()
                        .filter_map(|layer| layer_of_name(layer))
                        .collect(),
                    screen: entry
                        .screen
                        .iter()
                        .filter_map(|layer| layer_of_name(layer))
                        .collect(),
                })
            })
            .collect();
        self.handle
            .send(Cmd::CallShowing(workspace_id(workspace)?, shown))
            .map_err(js_error)
    }

    /// Ask `device` (hex) for `source` (`camera` or `screen`) afresh on the
    /// live session of `workspace`: its next picture starts a group this page can
    /// begin decoding at.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace, device or source, or a closed
    /// session.
    #[wasm_bindgen(js_name = callPictureRestart)]
    pub fn call_picture_restart(
        &self,
        workspace: &str,
        device: &str,
        source: &str,
    ) -> Result<(), JsError> {
        let device: [u8; 32] = hex::decode(device)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| js_error("device must be 32 bytes of hex"))?;
        let source =
            source_of_name(source).ok_or_else(|| js_error("source must be camera or screen"))?;
        self.handle
            .send(Cmd::CallPictureRestart(
                workspace_id(workspace)?,
                device,
                source,
            ))
            .map_err(js_error)
    }

    /// This page's device key, hex: how the workspace names it in presence
    /// and in the members' lists.
    #[wasm_bindgen(js_name = deviceKey)]
    pub fn device_key(&self) -> String {
        hex::encode(self.device_key)
    }

    /// Why this page shows no live presence, in the words it shows in the
    /// members' place, or `null` while it has a live session: no live session could be
    /// reached.
    #[wasm_bindgen(js_name = liveNote)]
    pub fn live_note(&self) -> Option<String> {
        self.handle.live_note()
    }

    /// The members' ephemeral previews of their records since the last call,
    /// as a JSON array (`{kind: 'preview'|'previewEnd', publisher, record,
    /// seq?, ops?}`, `publisher` the publishing device's key in hex). Their
    /// presence is merged into this page's boards as it arrives, never
    /// handed on. This page's own messages are left out.
    #[wasm_bindgen(js_name = takeLiveEvents)]
    pub fn take_live_events(&self) -> String {
        let events: Vec<serde_json::Value> = self
            .live_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                Event::LiveSession {
                    ws: workspace_id,
                    from,
                    track: Track::Presence,
                    body,
                    ..
                } if from != self.device_key => {
                    hear(self.id, workspace_id, from, &body);
                    None
                }
                Event::Live { ws: _, from, frame } if from != self.device_key => {
                    let publisher = hex::encode(from);
                    let event = match LiveFrame::decode(&frame).ok()? {
                        LiveFrame::Ops(OpsFrame::Preview { seq, record, ops }) => {
                            JsLiveEvent::Preview {
                                publisher,
                                record: record_to_js(record),
                                seq,
                                ops: ops_to_js(ops),
                            }
                        }
                        LiveFrame::Ops(OpsFrame::End { record }) => JsLiveEvent::PreviewEnd {
                            publisher,
                            record: record_to_js(record),
                        },
                        LiveFrame::Ops(OpsFrame::Semantic { .. }) => return None,
                    };
                    serde_json::to_value(event).ok()
                }
                _ => None,
            })
            .collect();
        serde_json::to_string(&events).unwrap_or_else(|_| "[]".into())
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
                Event::Landed { route, .. } => Some(JsDurable::Durable {
                    record: JsRecord {
                        app: route.app_id,
                        record: route.document,
                    },
                }),
                _ => None,
            })
            .collect();
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
    }

    /// Ask one action of the workspace's host: `message` is a JSON `RemoteActionMessage`, `person`
    /// the person this page acts for, and the answer resolves to the JSON answer the host gave.
    /// The session carries the request there and its answer back, so the page holds no frames of
    /// its own.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace, device or message, a page with no session with a host,
    /// and a device whose answer never came.
    #[wasm_bindgen(js_name = actionMessage)]
    pub async fn action_message(
        &self,
        workspace: &str,
        person: &str,
        message: &str,
    ) -> Result<String, JsError> {
        let number = workspace_id(workspace)?;
        let api = crate::peer_sync::actions_of(self.id, number)
            .ok_or_else(|| js_error("this page holds no session with a host"))?;
        let caller =
            tilo_actions::remote::Caller::local_person(person.to_owned()).in_workspace(workspace);
        tilo_session_actions::ask(&api, &caller, message)
            .await
            .map_err(|error| js_error(&error))
    }

    /// Resolves `true` once the workspace's server says a write landed in the workspace's documents
    /// ([`tilo_actions::remote::ActionProvider::changed`], a request the server holds for ten seconds
    /// at most), and `false` once `until` settles first. The wait is let go of when this
    /// resolves, so a server that never answers costs this page nothing: the session forgets a
    /// wait nobody reads any more.
    ///
    /// It says that something was written and never what: what changed is read from the server's
    /// feed of changes, which answers each caller with what it may read.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace, a page with no session with a server, and a wait the
    /// server refused or the session could not carry, with why.
    #[wasm_bindgen(js_name = writeLanded)]
    pub async fn write_landed(&self, workspace: &str, until: Promise) -> Result<bool, JsError> {
        use std::future::Future as _;
        use std::pin::Pin;
        use std::task::Poll;
        let number = workspace_id(workspace)?;
        let api = crate::peer_sync::actions_of(self.id, number)
            .ok_or_else(|| js_error("this page holds no session with a server"))?;
        let mut landed = api.wait_for_change(tilo_actions::remote::WatchScope::Records);
        let mut over = JsFuture::from(until);
        std::future::poll_fn(move |context| {
            if let Poll::Ready(woken) = Pin::new(&mut landed).poll(context) {
                return Poll::Ready(match woken {
                    Some(Ok(())) => Ok(true),
                    Some(Err(refused)) => Err(js_error(refused)),
                    None => Ok(false),
                });
            }
            Pin::new(&mut over).poll(context).map(|_| Ok(false))
        })
        .await
    }

    /// Have this page's device in the person's own workspace on the host, asked on the session it
    /// holds there: made the first time, under an id the host chooses, answered again after, another
    /// for each `name` ([`Device::enter_own_workspace`]). This very device
    /// redeems the invitation on that session, shown to the workspace's people as `shownAs`,
    /// unless `entered(workspace)` says it is in already. Resolves to the workspace's listing,
    /// `{workspace, workspace_id, name, generation}` JSON. `invocation` is the caller's own id for
    /// this make (a UUID the page keeps while its dialog asks again after no answer), so the next
    /// ask is answered the workspace the first made, never a second one; `null` asks under a fresh
    /// id.
    ///
    /// # Errors
    ///
    /// Rejects with an `Error` named `DeviceRefusal` whose `code` says how the host refused
    /// ([`DeviceRefusal`]: `unknown_action` for a host that keeps no workspaces of people's own) and
    /// whose `refusal` is the server's typed refusal where it gave one; a text that is no UUID is
    /// refused before the server is asked.
    #[wasm_bindgen(js_name = enterOwnWorkspace)]
    pub async fn enter_own_workspace(
        &self,
        name: Option<String>,
        shown_as: String,
        entered: Function,
        invocation: Option<String>,
    ) -> Result<String, JsValue> {
        let invocation = invocation
            .map(|text| text.parse::<tilo_actions::InvocationId>().map_err(js_error))
            .transpose()?;
        let listing = self
            .device
            .enter_own_workspace(
                &ui::i18n::I18nManager::window_locale(),
                name.as_deref(),
                &shown_as,
                |id| {
                    entered
                        .call1(&JsValue::NULL, &JsValue::from_str(id))
                        .is_ok_and(|answer| answer.is_truthy())
                },
                invocation,
            )
            .await
            .map_err(|refusal| refusal_error(&refusal))?;
        serde_json::to_string(&listing).map_err(|error| js_error(error).into())
    }

    /// Report the page store's durability of a record (`unknown`,
    /// `pending`, `committing`, `durable`, `failed`).
    #[wasm_bindgen(js_name = setLocalState)]
    pub fn set_local_state(&self, app: &str, workspace: &str, record: &str, state: &str) {
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
                cluster_id: workspace.into(),
                document: record.into(),
            },
            state,
        );
    }

    /// Resolve once every published document is durable on the host.
    pub async fn flush(&self) -> Result<(), JsError> {
        self.handle
            .barrier()
            .map_err(js_error)?
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)
    }

    /// Reconnect now (also after a failure).
    pub fn reconnect(&self) -> Result<(), JsError> {
        self.handle.send(Cmd::Reconnect).map_err(js_error)
    }

    /// The network is back: a session waiting to reconnect tries now; a
    /// connected one stays as it is.
    #[wasm_bindgen(js_name = retryNow)]
    pub fn retry_now(&self) -> Result<(), JsError> {
        self.handle.send(Cmd::Retry).map_err(js_error)
    }

    /// Ask the host for the joined workspaces' shared state again.
    pub fn refresh(&self) -> Result<(), JsError> {
        self.handle.send(Cmd::Refresh).map_err(js_error)
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
            .report_failure(failure_kind(surface, asset), code, message)
            .map_err(js_error)
    }

    /// Clear the failure `reportFailure` reported for the same kind.
    #[wasm_bindgen(js_name = clearFailure)]
    pub fn clear_failure(&self, surface: Option<String>, asset: Option<String>) {
        self.handle.clear_failure(&failure_kind(surface, asset));
    }

    /// Clear every failure of desktop surface `surface` and its assets.
    #[wasm_bindgen(js_name = clearSurface)]
    pub fn clear_surface(&self, surface: &str) {
        self.handle.clear_surface(surface);
    }

    /// The name the page shows its person by from now on, for history and presence (empty for
    /// none): the one they gave themselves, or one drawn from their key until they give one.
    #[wasm_bindgen(js_name = setDisplayName)]
    pub fn set_display_name(&self, name: &str) {
        set_label(self.id, name);
    }

    /// The person named themselves `name`: their server keeps it as theirs, with their profile,
    /// and names them by it in every workspace it keeps, this one included.
    #[wasm_bindgen(js_name = nameSelf)]
    pub fn name_self(&self, name: &str) {
        if let Some(workspace) = self.speaking_workspace.borrow().as_deref() {
            set_member_name(&self.handle, workspace, name);
        }
    }

    /// End the session. The page no longer acts as the certified user; a
    /// lobby disposed beside the workspace's peer leaves the person's identity
    /// and conversations to that one.
    pub fn dispose(&self) {
        self.listeners.borrow_mut().clear();
        self.handle.close();
        release(self.id);
        crate::page_person::release(self.id);
    }
}
