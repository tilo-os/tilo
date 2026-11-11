//! The page's certified peer: `tilo_session_client` behind wasm-bindgen for
//! `web/peer.mjs`, reaching the host over HTTPS only.
//!
//! The device key stays in custody (`web/device-custody.mjs`): either a
//! non-extractable `WebCrypto` Ed25519 key that signs through a JavaScript
//! callback, or — where `WebCrypto` lacks Ed25519 — a seed unsealed from
//! `IndexedDB` straight into this module, which keeps it only in wasm
//! memory. Nothing here generates or holds a user root: a page that holds
//! one (`web/identity.mjs`) lends it as a signer, the same shape as
//! custody, to [`issue_device_cert`] and [`certify_with_root`].
//!
//! The peer record (certificates and the commit outbox) and the workspaces it
//! holds are kept in the profile's durable store when the page names a profile
//! ([`DurablePeerStore`]), committed to `IndexedDB` with everything else.
//! Once started, the certified user becomes the toolkit's local identity
//! (`ui::local_identity`), so mounts attribute edits to that person.
//!
//! HTTP goes through the page's `post(path, session, body)` (its `fetch`).
//! Presence — who is here, their pointer, what their shell shows — rides the
//! workspace's live plane like every other device's, kept on the session's own
//! board ([`crate::peer_sync`]) and read back as the members answer
//! ([`WasmPeer::take_live_events`]). Documents reach the desktop's
//! `CollaborationManager` through [`WasmPeer::collab_transport`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Function, Promise, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use tilo_channels::VAULT_SECRET_APP as CHANNEL_VAULT_APP;
use tilo_live::live::Track;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::https::{HttpClient, HttpFuture};
use tilo_relay_client::{Answer, AnswerFuture};
use tilo_relay_client::{DeviceSigner, Host, SignError, SignFuture};
use tilo_relay_protocol::live::{LiveAnchor, LiveFrame, LiveRecord, OpsFrame, PreviewOp};
use tilo_relay_protocol::web::{PageHost, PageHostAddress};
use tilo_session::sync::{Cmd, Event, Outbox};
use tilo_session_client::device::{Device, HostRefusal};
use tilo_session_client::{Events, FailureSubject, Link, LocalState};
use tilo_session_client::{PeerConfig, PeerRecord, PeerStore};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList, RevocationReason};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::peer_sync::{JsPresence, due, hear, members, release, say, set_label};

pub(crate) fn js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

/// `refusal` as the page reads it: an `Error` named `HostRefusal`, with the refusal's `code` and,
/// where the host gave one, its typed `refusal` (`{reason, message, next}`).
pub(crate) fn refusal_error(refusal: &HostRefusal) -> JsValue {
    let error = js_sys::Error::new(&refusal.message);
    error.set_name("HostRefusal");
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

/// A refusal of a workspace's host, `wire` as it travels (an `ActionHostError`'s JSON), read as
/// every caller reads it ([`HostRefusal::of`]): `{code, message, refusal}` JSON, `refusal` the
/// host's typed refusal where it gave one.
///
/// # Errors
///
/// Throws for text that is no refusal of a host's.
#[wasm_bindgen(js_name = hostRefusal)]
pub fn host_refusal(wire: &str) -> Result<String, JsError> {
    let error: tilo_actions::remote::ActionHostError =
        serde_json::from_str(wire).map_err(js_error)?;
    serde_json::to_string(&HostRefusal::of(&error)).map_err(js_error)
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
    pub(crate) fn from_js(custody: &JsValue) -> Result<Self, JsError> {
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
    /// The workspaces the peer holds, beside the record.
    workspaces_key: durable_store::StorageKey,
    /// Seals the member state in `channels_key`. The page keeps it in the
    /// platform secret store ([`channel_vault`]), never in this one.
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
            workspaces_key: durable_store::StorageKey::record(
                PEER_PACKAGE,
                crate::storage::PROFILE_WORKSPACE,
                format!("{}.rooms", hex::encode(user_public_key)),
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

    fn load_rooms(&self) -> Option<Vec<u8>> {
        use durable_store::DurableStore as _;
        Some(self.store.read(&self.workspaces_key).ok()??.bytes)
    }

    fn save_rooms(&self, bytes: &[u8]) -> Result<(), String> {
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
                .and_then(|value| tilo_relay_client::https::retry_after(&value));
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

/// The store a peer replicates its workspace into, and where it lives.
type ReplicaStore = (Rc<durable_store::MemoryStore>, crate::replication::Kept);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectConfig {
    cert: DeviceCert,
    host_cert: DeviceCert,
    /// Where the host's iroh endpoint is dialled: the session and its planes
    /// reach it there.
    address: PageHostAddress,
    /// The person's chosen name, for history and presence; empty for none.
    #[serde(default)]
    display_name: String,
    /// The revocations this person's root signed, as the page kept them: the
    /// session announces them to its host with its certificate, so a host
    /// refuses a device of theirs, or a stand-in it holds for them, from the
    /// moment each entry names.
    #[serde(default)]
    revocations: Vec<RevocationEntry>,
    /// The workspace this device enters through its door as its session
    /// opens ([`door_entry`]), when the host has not admitted it yet.
    #[serde(default)]
    entering: Option<tilo_relay_protocol::entering::Entering>,
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
    let (status, bytes) = post.post(&path, body).await.map_err(js_error)?;
    if status != 200 {
        return Err(js_error(format!("host answered {status}")));
    }
    postcard::from_bytes(&bytes).map_err(js_error)
}

/// The host `address` names (`hostInfo().address`, JSON).
pub(crate) fn host_at(address: &str) -> Result<Host, JsError> {
    Ok(dialled(serde_json::from_str(address).map_err(js_error)?))
}

/// The host at `address`, as the peer dials it.
fn dialled(address: PageHostAddress) -> Host {
    Host::published(&address.into())
}

/// What the host publishes before certification: its certificate. Resolves
/// to JSON.
#[wasm_bindgen(js_name = hostInfo)]
pub async fn host_info(post: Function) -> Result<String, JsError> {
    let info: tilo_relay_protocol::web::HostInfo =
        post_decoded(&PageHttp(post), "/host", Vec::new()).await?;
    serde_json::to_string(&PageHost::from(info)).map_err(js_error)
}

/// Whether the owner of `workspace` (its id) deleted it, as the host `post` reaches says to
/// any device that names it ([`tilo_relay_client::https::workspace_deleted`]).
#[wasm_bindgen(js_name = workspaceDeleted)]
pub async fn workspace_deleted(post: Function, workspace: String) -> Result<bool, JsError> {
    tilo_relay_client::https::workspace_deleted(&PageHttp(post), &workspace)
        .await
        .map_err(js_error)
}

/// Certify this page's device key with the host its address names, as the
/// answer to the pairing invite `inviteHex` (16 bytes as hex) the person's
/// link named, if any. Calls `onPairingCode(code)` once the host queued the
/// request. Resolves to `{cert, hostCert}` JSON for [`WasmPeer::start`].
#[wasm_bindgen(js_name = certifyDevice)]
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "the page's HTTP client stays on its one thread"
)]
pub async fn certify_device(
    user_public_key_hex: String,
    label: String,
    invite_hex: Option<String>,
    custody: JsValue,
    on_pairing_code: Function,
    address: String,
) -> Result<String, JsError> {
    let signer = PageSigner::from_js(&custody)?;
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
    let host = host_at(&address)?;
    let (cert, host_cert) =
        tilo_relay_client::certify(&host, user, &label, invite, &signer, |code| {
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
        "browser" => tilo_relay_client::IssuedRole::Browser,
        "node" => tilo_relay_client::IssuedRole::Node,
        "session" => tilo_relay_client::IssuedRole::Session,
        _ => return Err(js_error("role must be browser, node or session")),
    };
    let root = PageSigner::from_js(&root)?;
    let cert = tilo_relay_client::issue_device_cert(
        &root,
        device_key(&device_public_key_hex)?,
        &label,
        role,
    )
    .await
    .map_err(js_error)?;
    serde_json::to_string(&cert).map_err(js_error)
}

fn join_reply_json(reply: tilo_relay_protocol::workspaces::JoinReply) -> serde_json::Value {
    use tilo_relay_protocol::workspaces::JoinReply;
    match reply {
        JoinReply::Invitation {
            workspace,
            workspace_id,
            name,
            role,
            expires_at_ns,
            inviter,
            members,
        } => serde_json::json!({
            "kind": "invitation", "room": workspace, "workspaceId": workspace_id.to_string(),
            "name": name, "role": role, "expiresAtNs": expires_at_ns.to_string(),
            "members": members,
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
            "kind": "admitted", "room": workspace, "workspaceId": workspace_id.to_string(),
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

/// The short link of `invitation` (its signed text, as the host made it) for
/// the page at `base`: the invitation sealed under a key the link alone
/// carries and left on that site's shelf. `host` is the origin of the host
/// it admits into, which the link names so whoever follows it knows where the
/// workspace is. Resolves to `https://<site>/i/<handle>#<key>`.
///
/// # Errors
///
/// `base` is not a web address, `invitation` does not verify, or the site
/// could not be reached to keep it — the invitation's own text still works
/// then, which the caller hands over instead.
#[wasm_bindgen(js_name = sealedInvitationLink)]
pub async fn sealed_invitation_link(
    base: String,
    host: String,
    invitation: String,
) -> Result<String, JsError> {
    let origin = base.trim().trim_end_matches('/');
    let site = tilo_workspace_directory::web_origin(&format!("{origin}/")).map_err(js_error)?;
    let join = tilo_workspace_join::decode_and_verify_invitation_text(
        &invitation,
        None,
        &RevocationList::new(),
        ui::time::unix_now_ns(),
    )
    .map_err(js_error)?;
    // The shelf this leaves it on lives as long as the invitation: the link
    // is used, not kept, so no caller takes it back from here.
    let store = tilo_workspace_directory::Store::default();
    store
        .shelve_soon(
            &hex::encode(join.invite_id()),
            join.signed_invitation().to_vec(),
            join.grant().expires_at_ns,
            &site,
            Some(&host),
        )
        .await
        .map(|shelved| shelved.link)
        .map_err(js_error)
}

/// The invitation the link's `secret` names (its handle and the key in the
/// link's fragment), as the page takes it: fetched from the shelf of `site`
/// and opened with that key. An invitation's own text is answered as it came,
/// so every build's links join the same way. Resolves to JSON:
/// `{invitation, host?}`, the invitation's text as [`inspect_invitation`] and
/// [`invitation_entry`] take it, and the origin of the host it admits into where
/// the device that left it named one.
///
/// # Errors
///
/// The shelf could not be reached, holds nothing under the handle any more,
/// or the key does not open what it answered with.
#[wasm_bindgen(js_name = redeemInvitation)]
pub async fn redeem_invitation(site: String, secret: String) -> Result<String, JsError> {
    let sealed = tilo_workspace_directory::sealed_invitation(&site, &secret)
        .await
        .map_err(js_error)?;
    serde_json::to_string(&sealed).map_err(|error| js_error(error.to_string()))
}

/// What `invitation` (the signed invitation's bytes) offers at the host
/// `post` reaches: `{kind: 'invitation', room, workspaceId, name, role,
/// expiresAtNs}`, or `{kind: 'refused', reason}`. Resolves to JSON.
#[wasm_bindgen(js_name = inspectInvitation)]
pub async fn inspect_invitation(post: Function, invitation: Vec<u8>) -> Result<String, JsError> {
    let reply = tilo_relay_client::https::invitation(&PageHttp(post), &invitation)
        .await
        .map_err(js_error)?;
    Ok(join_reply_json(reply).to_string())
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
    host_cert: &str,
    with_root: bool,
) -> Result<String, JsError> {
    use tilo_session_client::device::{Held, reconnecting};
    let host: DeviceCert = serde_json::from_str(host_cert).map_err(js_error)?;
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
        ui::time::unix_now_ns(),
    );
    serde_json::to_value(answer)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| js_error("the answer has no name"))
}

/// What a page says as it enters a workspace ([`door_entry`], [`invitation_entry`]): the
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

/// What this page's device (`custody`) enters the workspace `invitation` (its signed bytes)
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
/// Rejects with an `Error` named `HostRefusal` whose `code` is `no_certificate`, or
/// `not_admitted` for an invitation that names no workspace.
#[wasm_bindgen(js_name = invitationEntry)]
pub async fn invitation_entry(
    host_cert: String,
    invitation: Vec<u8>,
    custody: JsValue,
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
    let host_cert: DeviceCert = serde_json::from_str(&host_cert).map_err(js_error)?;
    let device = PageSigner::from_js(&custody)?;
    let root = if root.is_undefined() || root.is_null() {
        None
    } else {
        Some(PageSigner::from_js(&root)?)
    };
    let (cert, entering) = tilo_session_client::device::invitation_entry(
        &host_cert,
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
        "certificate": {"cert": cert, "hostCert": host_cert},
        "entering": entering,
    })
    .to_string())
}

/// What this page's device (`custody`) enters `workspace` through its door with, on the host whose
/// certificate is `hostCert` (JSON), through the one entry every device takes
/// ([`tilo_session_client::device::door_entry`]): the certificate it enters under (the one it holds
/// from that host where it still serves, else one the person's root `root` certifies now) and the
/// entry the session it starts asks for as it opens. `options` is [`Accepting`] JSON. Resolves to
/// `{certificate: {cert, hostCert}, entering}` JSON; [`WasmPeer::start`] takes both, and the host's
/// answer arrives in the session's snapshot (`entry`).
///
/// # Errors
///
/// Rejects with an `Error` named `HostRefusal` whose `code` is `no_certificate`.
#[wasm_bindgen(js_name = doorEntry)]
pub async fn door_entry(
    host_cert: String,
    workspace: String,
    custody: JsValue,
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
    let host_cert: DeviceCert = serde_json::from_str(&host_cert).map_err(js_error)?;
    let device = PageSigner::from_js(&custody)?;
    let root = if root.is_undefined() || root.is_null() {
        None
    } else {
        Some(PageSigner::from_js(&root)?)
    };
    let (cert, entering) = tilo_session_client::device::door_entry(
        &host_cert,
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
        "certificate": {"cert": cert, "hostCert": host_cert},
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

/// Certify this page's device key (`custody`) with the user root the page
/// holds, for the host at `post`, whose certificate must be a host of that
/// root. Resolves to `{cert, hostCert}` JSON, as [`certify_device`] does.
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
        tilo_relay_client::certify_with_root(&PageHttp(post), &root, &signer, &label)
            .await
            .map_err(js_error)?;
    serde_json::to_string(&Certified {
        cert: &cert,
        host_cert: &host_cert,
    })
    .map_err(js_error)
}

/// `refusal` as the page reads it: an `Error` named `LoginCodeError`, with the host's HTTP
/// `status` (0 unreached) and the kind of refusal as `code`
/// ([`tilo_relay_client::login_code::CodeRefusal::code`]).
fn code_error(refusal: &tilo_relay_client::login_code::CodeRefusal) -> JsValue {
    let error = js_sys::Error::new(&refusal.message);
    error.set_name("LoginCodeError");
    let _ = Reflect::set(&error, &"code".into(), &refusal.code().into());
    let _ = Reflect::set(&error, &"status".into(), &refusal.status.into());
    error.into()
}

/// Make a login code at the host `post` reaches, naming `link` (this tab's pairing link, setting
/// up through an agent) or nothing (asking to be signed in by a device of the person). Resolves
/// to `{code, follow, expires_at_ms}` JSON.
///
/// # Errors
///
/// Rejects with a `LoginCodeError` ([`code_error`]).
#[wasm_bindgen(js_name = createLoginCode)]
pub async fn create_login_code(post: Function, link: Option<String>) -> Result<String, JsValue> {
    let made = tilo_relay_client::login_code::create(&PageHttp(post), link)
        .await
        .map_err(|refusal| code_error(&refusal))?;
    serde_json::to_string(&made).map_err(|error| js_error(error).into())
}

/// Where the code this tab follows with the secret `follow` is: the host's `Polled`, as JSON.
///
/// # Errors
///
/// Rejects with a `LoginCodeError` ([`code_error`]).
#[wasm_bindgen(js_name = followLoginCode)]
pub async fn follow_login_code(post: Function, follow: String) -> Result<String, JsValue> {
    let polled = tilo_relay_client::login_code::follow(&PageHttp(post), &follow)
        .await
        .map_err(|refusal| code_error(&refusal))?;
    serde_json::to_string(&polled).map_err(|error| js_error(error).into())
}

/// Give the person whose root the page holds (`root`) a stand-in on the host `post` reaches
/// ([`tilo_relay_client::login_code::keep_stand_in`]). Resolves to the stand-in's key, hex.
///
/// # Errors
///
/// Rejects with a `LoginCodeError` ([`code_error`]).
#[wasm_bindgen(js_name = keepStandIn)]
pub async fn keep_stand_in(post: Function, root: JsValue) -> Result<String, JsValue> {
    let root = PageSigner::from_js(&root)?;
    tilo_relay_client::login_code::keep_stand_in(&PageHttp(post), &root)
        .await
        .map_err(|refusal| code_error(&refusal))
}

/// Have the host `post` reaches forget the person's stand-in, signed by their root (`root`).
///
/// # Errors
///
/// Rejects with a `LoginCodeError` ([`code_error`]).
#[wasm_bindgen(js_name = retireStandIn)]
pub async fn retire_stand_in(post: Function, root: JsValue) -> Result<(), JsValue> {
    let root = PageSigner::from_js(&root)?;
    tilo_relay_client::login_code::retire_stand_in(&PageHttp(post), &root)
        .await
        .map_err(|refusal| code_error(&refusal))
}

/// Revoke `keyHex` — a device key of this person's, or the stand-in approver
/// their host holds — with the root the page holds (`root`): resolves to the
/// root-signed revocation as JSON, which the page keeps beside the person. It
/// is announced to every session of theirs at once, so their hosts refuse
/// what the key signs from now on and keep valid what it signed before.
#[wasm_bindgen(js_name = revokeDevice)]
pub async fn revoke_device(root: JsValue, key_hex: String) -> Result<String, JsError> {
    let root = PageSigner::from_js(&root)?;
    let entry = tilo_relay_client::revoke_device(
        &root,
        device_key(&key_hex)?,
        tilo_clock::unix_now_ns(),
        RevocationReason::Retired,
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
    Session(tilo_session_client::SessionMemberHost),
    /// From an endpoint bound for pairing alone: the page holds no session
    /// with this host yet.
    Own(tilo_relay_client::MemberApprover),
}

impl tilo_relay_client::MemberHost for PairingReach {
    async fn ask(
        &self,
        approval: &tilo_relay_protocol::certify::MemberApproval,
    ) -> Result<tilo_relay_protocol::certify::ApproverReply, tilo_relay_client::CertifyError> {
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
        Ok(Self(PairingReach::Own(
            tilo_relay_client::MemberApprover::new(host_at(address)?),
        )))
    }

    /// The new devices waiting here for the member root the page holds
    /// (`root`) to pair them: `[{digest, pairingCode, label, device, invite,
    /// requestedAtMs}]` JSON, keys, digest and invite as hex (`invite` null
    /// for a request no invite asked for).
    ///
    /// # Errors
    /// The host was not reached, or turned the root away.
    pub async fn pending(&self, root: JsValue) -> Result<String, JsError> {
        let root = PageSigner::from_js(&root)?;
        let pending = tilo_relay_client::pending_pairings(&self.0, &root)
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
        let issued = tilo_relay_client::answer_pairing(&self.0, &root, digest, approved)
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

pub(crate) fn workspace_number(text: &str) -> Result<u64, JsError> {
    tilo_relay_protocol::room_workspace_id(text).map_err(|_| js_error("not a workspace uuid"))
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is its own field the page reads: publishes, camera, screen, muted, deafened"
)]
struct JsVoiceParticipant {
    device: String,
    user: String,
    publishes: bool,
    camera: bool,
    /// It shares a screen.
    screen: bool,
    /// Its microphone is off, or it may not publish.
    muted: bool,
    /// It hears nothing of the call.
    deafened: bool,
}

fn voice_participants(
    participants: Vec<tilo_relay_protocol::voice::VoiceParticipant>,
) -> Vec<JsVoiceParticipant> {
    participants
        .into_iter()
        .map(|participant| JsVoiceParticipant {
            device: hex::encode(participant.device_public_key),
            user: hex::encode(participant.user_public_key),
            publishes: participant.publishes,
            camera: participant.showing.camera,
            screen: participant.showing.screen,
            muted: participant.showing.muted,
            deafened: participant.showing.deafened,
        })
        .collect()
}

/// A call grant for `web/voice.mjs`: ids and the token as hex.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsVoiceGrant {
    call: String,
    epoch: u64,
    directions: JsDirections,
    participants: Vec<JsVoiceParticipant>,
}

/// A call of the workspace as `web/voice.mjs` lists it.
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
    /// One frame of a call's voice, as the device `from` published it on the
    /// workspace's plane: the page's app plays it.
    Heard {
        room: String,
        from: String,
        sequence: u64,
        timestamp48k: u64,
        opus: String,
    },
    /// One picture of a call on the workspace's plane: the tilo-video/1
    /// object of `from`'s camera or screen, in hex, as the app's decoder
    /// draws it.
    Picture {
        room: String,
        from: String,
        source: &'static str,
        keyframe: bool,
        object: String,
    },
    /// What the call now shows of this page's pictures: the layers to encode
    /// of each source, and the sources to start over at a keyframe.
    Demand {
        room: String,
        wanted: Vec<JsWanted>,
        keyframes: Vec<&'static str>,
    },
}

/// The layers of one source a call shows.
#[derive(Serialize)]
struct JsWanted {
    source: &'static str,
    layers: Vec<&'static str>,
}

/// The name `web/voice-video.mjs` gives `source`.
fn js_source(source: tilo_live::video::Source) -> &'static str {
    match source {
        tilo_live::video::Source::Camera => "camera",
        tilo_live::video::Source::Screen => "screen",
    }
}

/// The name `web/voice-video.mjs` gives `layer`.
fn js_layer(layer: tilo_live::video::Layer) -> &'static str {
    match layer {
        tilo_live::video::Layer::Low => "low",
        tilo_live::video::Layer::Mid => "mid",
        tilo_live::video::Layer::High => "high",
    }
}

/// The source a name of `web/voice-video.mjs` is.
fn source_of_name(name: &str) -> Option<tilo_live::video::Source> {
    match name {
        "camera" => Some(tilo_live::video::Source::Camera),
        "screen" => Some(tilo_live::video::Source::Screen),
        _ => None,
    }
}

/// The layer a name of `web/voice-video.mjs` is.
fn layer_of_name(name: &str) -> Option<tilo_live::video::Layer> {
    match name {
        "low" => Some(tilo_live::video::Layer::Low),
        "mid" => Some(tilo_live::video::Layer::Mid),
        "high" => Some(tilo_live::video::Layer::High),
        _ => None,
    }
}

fn workspace_of(
    workspace_id: u64,
    workspaces: &RefCell<std::collections::HashMap<u64, String>>,
) -> String {
    workspaces
        .borrow()
        .get(&workspace_id)
        .cloned()
        .unwrap_or_else(|| workspace_id.to_string())
}

fn voice_event(
    event: tilo_session::sync::VoiceEvent,
    workspaces: &RefCell<std::collections::HashMap<u64, String>>,
) -> JsVoiceEvent {
    use tilo_session::sync::VoiceEvent;
    match event {
        VoiceEvent::Calls {
            ws: workspace_id,
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
                room: workspace_of(workspace_id, workspaces),
                calls,
                error,
            }
        }
        VoiceEvent::CallResult {
            ws: workspace_id,
            request,
            result,
        } => {
            let (call, error) = match result {
                Ok(state) => (Some(call_state(&state)), None),
                Err(error) => (None, Some(error)),
            };
            JsVoiceEvent::CallResult {
                room: workspace_of(workspace_id, workspaces),
                request: hex::encode(request),
                call,
                error,
            }
        }
        VoiceEvent::Granted {
            ws: workspace_id,
            grant,
        } => {
            let grant = *grant;
            JsVoiceEvent::Granted {
                room: workspace_of(workspace_id, workspaces),
                grant: JsVoiceGrant {
                    call: hex::encode(grant.call),
                    epoch: grant.epoch,
                    directions: grant.directions.into(),
                    participants: voice_participants(grant.participants),
                },
            }
        }
        VoiceEvent::Ended {
            ws: workspace_id,
            call,
            reason,
        } => JsVoiceEvent::Ended {
            room: workspace_of(workspace_id, workspaces),
            call: hex::encode(call),
            reason,
        },
    }
}

/// A change to a call, as `web/voice.mjs` asks for it.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum JsCallOperation {
    Open {
        app: String,
        record: String,
        channel: String,
    },
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
            Self::Open {
                app,
                record,
                channel,
            } => CallOperation::Open {
                app_id: app,
                record_id: record,
                channel_id: channel,
            },
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

/// The revocations of `entries` the person `cert` certifies signed: a
/// session announces only its own person's, since a host reads a hello's
/// entries as its sender's root's and refuses one signed by another.
fn own_revocations(
    entries: Vec<RevocationEntry>,
    cert: &DeviceCert,
) -> Result<RevocationList, JsError> {
    let mut revocations = RevocationList::new();
    for entry in entries {
        if entry.user_public_key == cert.user_public_key {
            revocations.insert(entry).map_err(js_error)?;
        }
    }
    Ok(revocations)
}

/// A running certified peer.
#[wasm_bindgen]
pub struct WasmPeer {
    id: u32,
    device_key: [u8; 32],
    reporter_cert: DeviceCert,
    owner_cert: DeviceCert,
    handle: Link,
    /// This page's device in its person's workspaces, over the same session: how it enters them.
    device: Device,
    durable_events: Events,
    live_events: Events,
    voice_events: Events,
    /// Joined workspaces by workspace, to name them in call events.
    voice_workspaces: RefCell<std::collections::HashMap<u64, String>>,
    listeners: Rc<RefCell<Vec<Function>>>,
    /// Where the workspace's documents are kept (the profile's store, or memory
    /// for a peer in no profile) and a queue opened at start, until a workspace
    /// is joined and replicated ([`crate::replication`]).
    replication: RefCell<Option<(Events, ReplicaStore)>>,
    /// The workspace the person takes part in through this page, once joined
    /// as a person: the one that names them.
    speaking_workspace: RefCell<Option<String>>,
}

/// A page peer's record store, and the profile store it lives in when it has one.
type KeptPeer = (Arc<dyn PeerStore>, Option<Rc<durable_store::MemoryStore>>);

/// Where a page's peer keeps its record: the profile `namespace`'s durable store, sealed under
/// its `channelVault`, with that store returned too for the workspace's documents; else the
/// stored `record` bytes, saved through `save`.
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
) -> Result<KeptPeer, JsError> {
    if let Some(namespace) = namespace {
        let channel_vault: [u8; 32] = channel_vault
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| js_error("a profile peer needs its 32-byte channel vault"))?;
        let durable = DurablePeerStore::open(&namespace, user, channel_vault)?;
        let kept = Rc::clone(&durable.store);
        return Ok((Arc::new(durable), Some(kept)));
    }
    let record = record
        .map(|bytes| postcard::from_bytes::<PeerRecord>(&bytes))
        .transpose()
        .map_err(js_error)?;
    let save = save.ok_or_else(|| js_error("a peer needs a namespace or a save callback"))?;
    Ok((
        Arc::new(PageStore {
            current: RefCell::new(record),
            save,
        }),
        None,
    ))
}

/// Tell `workspace` its member, the page's person, goes by `name` from now on:
/// what its member list and everyone's person lists show for them. Asked
/// as the page joins and whenever the person renames themself, so a name
/// the workspace recorded earlier is corrected on the next visit. A person who
/// chose no name keeps the one they were admitted with.
fn set_member_name(handle: &Link, workspace: &str, name: &str) {
    if name.trim().is_empty() {
        return;
    }
    let request = tilo_relay_protocol::workspaces::WorkspacesRequest::Name {
        workspace: workspace.to_owned(),
        name: name.to_owned(),
    };
    let _ = handle.ask_workspaces(request);
}

impl WasmPeer {
    /// Ask the host `request`, and resolve to its answer.
    pub(crate) async fn rooms_request(
        &self,
        request: tilo_relay_protocol::workspaces::WorkspacesRequest,
    ) -> Result<tilo_relay_protocol::workspaces::WorkspacesReply, JsError> {
        self.handle
            .ask_workspaces(request)
            .map_err(js_error)?
            .await
            .map_err(|_| js_error("peer session closed"))?
            .map_err(js_error)
    }
}

#[wasm_bindgen]
impl WasmPeer {
    /// Start a session. `configJson` is `{cert, hostCert}` from
    /// [`certify_device`], optionally with `displayName`. With `namespace`
    /// (`{profile, device}` JSON, whose storage the page has opened) the
    /// peer record lives in that profile's durable store; otherwise `record`
    /// is the stored record bytes (or `undefined`) and `save(bytes)` writes
    /// it. `post(path, session, body)` reaches the host origin.
    /// `channelVault` (32 bytes, from [`channel_vault`]) seals the profile's
    /// channel state at rest; a peer with a namespace needs it.
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
        channel_vault: Option<Vec<u8>>,
    ) -> Result<WasmPeer, JsError> {
        let config: ConnectConfig = serde_json::from_str(config_json).map_err(js_error)?;
        let signer = PageSigner::from_js(custody)?;
        if signer.device_public_key() != config.cert.device_public_key {
            return Err(js_error("custody does not hold the certified device key"));
        }
        let signer: Arc<dyn DeviceSigner> = Arc::new(signer);
        let (store, profile_store) = peer_store(
            namespace,
            record,
            save,
            channel_vault.as_deref(),
            &config.cert.user_public_key,
        )?;
        let device_key = config.cert.device_public_key;
        let reporter_cert = config.cert.clone();
        let owner_cert = config.host_cert.clone();
        let display_name = config.display_name;
        let revocations = own_revocations(config.revocations, &config.cert)?;
        let handle = tilo_session_client::start_session(
            PeerConfig {
                host: dialled(config.address),
                cert: config.cert,
                host_cert: config.host_cert,
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
        let id = NEXT_PEER.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1).max(1));
            id
        });
        crate::peer_sync::register(id, &handle, &reporter_cert, &owner_cert, &display_name);
        // A lobby started beside the workspace's peer does not rename the
        // page's person.
        if crate::page_person::claim(id) {
            install_identity(&reporter_cert, display_name);
        }
        // Every peer replicates the workspace it joins, so a desktop mounted
        // through it always has a replica to attach to. A peer kept in no
        // profile (a page's, whose device lives only in memory) keeps the
        // workspace's documents in memory for as long as the page.
        let replication_store = match profile_store {
            Some(store) => (store, crate::replication::Kept::InProfile),
            None => (
                Rc::new(durable_store::MemoryStore::new()),
                crate::replication::Kept::InPage,
            ),
        };
        let replication = RefCell::new(Some((handle.events(), replication_store)));
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
                    crate::feedback_code::pump(id);
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
            live_events: handle.events(),
            voice_events: handle.events(),
            voice_workspaces: RefCell::default(),
            handle,
            device,
            listeners,
            replication,
            speaking_workspace: RefCell::default(),
        })
    }

    /// Select an existing destination. Admission comes only from the connected
    /// owner, asynchronously; mount after feedbackStatus reports ready.
    /// # Errors
    /// Refuses missing existing membership or a repeated configuration.
    #[wasm_bindgen(js_name = configureFeedback)]
    pub fn configure_feedback(&self, destination: &str) -> Result<(), JsError> {
        crate::feedback_code::configure(
            self.id,
            self.handle.clone(),
            self.reporter_cert.clone(),
            self.owner_cert.clone(),
            serde_json::from_str(destination).map_err(js_error)?,
        )
        .map_err(js_error)
    }
    /// Send the reports of desktops mounted with this peer through the
    /// page's peer `carrier` (its `id`), which is in the destination's workspace:
    /// reports reach that workspace's channel from whichever workspace the page is in.
    #[wasm_bindgen(js_name = sendFeedbackThrough)]
    pub fn send_feedback_through(&self, carrier: u32) {
        crate::feedback_code::send_through(self.id, carrier);
    }
    #[wasm_bindgen(js_name = feedbackStatus)]
    pub fn feedback_status(&self) -> String {
        crate::feedback_code::status(self.id)
    }

    /// `PeerSnapshot` JSON.
    pub fn snapshot(&self) -> String {
        serde_json::to_string(&self.handle.snapshot()).unwrap_or_default()
    }

    /// This page's channel state as text: whom each workspace follows, which
    /// senders may govern it, and each route's membership.
    #[wasm_bindgen(js_name = channelDiagnostics)]
    pub fn channel_diagnostics(&self) -> String {
        self.handle.status().channels
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
        let workspace_id = workspace_number(workspace)?;
        let name = name.filter(|name| !name.trim().is_empty());
        self.handle
            .join_placed(workspace, name.as_deref().unwrap_or("Workspace"))
            .map_err(js_error)?;
        self.voice_workspaces
            .borrow_mut()
            .insert(workspace_id, workspace.to_owned());
        if let Some((events, store)) = self.replication.borrow_mut().take() {
            let device = hex::encode(self.device_key);
            crate::replication::open(
                self.id,
                device,
                self.handle.clone(),
                events,
                workspace,
                store,
            )
            .map_err(js_error)?;
            if crate::page_person::speaks(self.id) {
                if let Some(identity) = ui::local_identity::load() {
                    set_member_name(&self.handle, workspace, &identity.display_name);
                }
                *self.speaking_workspace.borrow_mut() = Some(workspace.to_owned());
            }
        }
        Ok(())
    }

    /// This peer in the page, for `mountDesktop({peer})`: mounts in its
    /// workspace replicate their records and desktop through it.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The session token the host's fetch proxy (`/peer/v1/fetch`) takes,
    /// while this page is connected to a host that serves one; `undefined`
    /// otherwise. It changes when the page reconnects, so the page's fetch
    /// proxy global reads it for every request.
    #[wasm_bindgen(js_name = fetchProxySession)]
    pub fn fetch_proxy_session(&self) -> Option<String> {
        self.handle.fetch_proxy_session()
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
    /// person's root custody the page holds: over this session, with no
    /// connection of its own.
    pub fn pairing(&self) -> PairingHost {
        PairingHost(PairingReach::Session(tilo_session_client::member_host(
            &self.handle,
        )))
    }

    /// Answer what this person's agents on this session's host ask them,
    /// over this session, as the person whose root custody the page holds
    /// (`root`), on the question sheet over the desktop. Once per session.
    ///
    /// # Errors
    /// `root` is no custody.
    #[wasm_bindgen(js_name = answerAgentQuestions)]
    pub fn answer_agent_questions(&self, root: &JsValue) -> Result<(), JsError> {
        crate::questions::answer_agent_questions(self.id, root)
    }

    #[wasm_bindgen(js_name = preAdmit)]
    pub async fn pre_admit(&self, root: JsValue, options: String) -> Result<String, JsError> {
        use tilo_relay_client::DeviceSigner as _;
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
        let seconds = crate::rooms::seconds(Some(options.expires_in_seconds))?;
        let root = PageSigner::from_js(&root)?;
        let now_ns = ui::time::unix_now_ns();
        let mut grant = tilo_relay_protocol::workspaces::PreAdmission {
            workspace: options.workspace,
            role: crate::rooms::wire_role(&options.role)?,
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
            .rooms_request(
                tilo_relay_protocol::workspaces::WorkspacesRequest::PreAdmit {
                    request_id: request_id(&options.request_id)?,
                    grant,
                    inviter_name: options.inviter_name,
                },
            )
            .await?;
        crate::rooms::invitation_text(reply)
    }

    /// Say this page's `value` of itself in `workspace` (the live layer's
    /// value, JSON: `{participant, place, layout, focus, selection, pointer,
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
        let workspace_id = workspace_number(workspace)?;
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
        let workspace_id = workspace_number(workspace)?;
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
            .send(Cmd::SealLive(workspace_number(&workspace)?, frame, sealed))
            .map_err(js_error)?;
        receipt.await.map_err(|_| js_error("peer session closed"))
    }

    /// Ask the host for `workspace`'s calls; the answer is a `calls` event of
    /// [`Self::take_voice_events`].
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or a closed session.
    #[wasm_bindgen(js_name = voiceCalls)]
    pub fn voice_calls(&self, workspace: &str) -> Result<(), JsError> {
        self.handle
            .send(Cmd::VoiceCalls(workspace_number(workspace)?))
            .map_err(js_error)
    }

    /// Start, end or restart a call of `workspace` (`{kind: 'start', app, record,
    /// channel, allowed}`, `{kind: 'end' | 'restart', call, epoch}` JSON);
    /// its `callResult` event names `request` (16 bytes of hex).
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace, operation or request, or a closed
    /// session.
    #[wasm_bindgen(js_name = voiceCall)]
    pub fn voice_call(
        &self,
        workspace: &str,
        operation_json: &str,
        request: &str,
    ) -> Result<(), JsError> {
        let operation: JsCallOperation = serde_json::from_str(operation_json).map_err(js_error)?;
        let request: [u8; 16] = hex::decode(request)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| js_error("request must be 16 bytes of hex"))?;
        self.handle
            .send(Cmd::VoiceCall(
                workspace_number(workspace)?,
                request,
                operation.into_operation()?,
            ))
            .map_err(js_error)
    }

    /// Take part in call `call` (hex) of `workspace` until [`Self::voice_leave`]:
    /// the session keeps the participation alive and its grants arrive as
    /// `granted` events of [`Self::take_voice_events`].
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or call, or a closed session.
    #[wasm_bindgen(js_name = voiceJoin)]
    pub fn voice_join(&self, workspace: &str, call: &str) -> Result<(), JsError> {
        self.handle
            .send(Cmd::VoiceJoin(
                workspace_number(workspace)?,
                hex32(call, "call")?,
            ))
            .map_err(js_error)
    }

    /// Leave the call of `workspace`.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or a closed session.
    #[wasm_bindgen(js_name = voiceLeave)]
    pub fn voice_leave(&self, workspace: &str) -> Result<(), JsError> {
        self.handle
            .send(Cmd::VoiceLeave(workspace_number(workspace)?))
            .map_err(js_error)
    }

    /// Show this session's camera (`camera`) or a shared screen (`screen`)
    /// besides its voice in the call of `workspace`, or stop, whether its
    /// microphone is off (`muted`) and whether it hears the call
    /// (`deafened`): everyone in the call learns it from the workspace.
    ///
    /// # Errors
    ///
    /// Throws for a malformed workspace or a closed session.
    #[wasm_bindgen(js_name = voiceShow)]
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "the page's call says each of what it shows as its own flag, as `Showing` holds them"
    )]
    pub fn voice_show(
        &self,
        workspace: &str,
        camera: bool,
        screen: bool,
        muted: bool,
        deafened: bool,
    ) -> Result<(), JsError> {
        self.handle
            .send(Cmd::VoiceShow(
                workspace_number(workspace)?,
                tilo_relay_protocol::voice::Showing {
                    camera,
                    screen,
                    muted,
                    deafened,
                },
            ))
            .map_err(js_error)
    }

    /// Call events since the last call, as a JSON array of `{kind: 'calls',
    /// room, calls | error}`, `{kind: 'callResult', room, request, call |
    /// error}`, `{kind: 'granted', room, grant}`, `{kind: 'ended', room,
    /// call, reason}` and `{kind: 'heard', room, from, sequence,
    /// timestamp48k, opus}` (`opus` in hex), which the app plays.
    #[wasm_bindgen(js_name = takeVoiceEvents)]
    pub fn take_voice_events(&self) -> String {
        let out: Vec<JsVoiceEvent> = self
            .voice_events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                Event::Voice(event) => Some(voice_event(event, &self.voice_workspaces)),
                Event::CallAudio {
                    ws: workspace_id,
                    from,
                    sequence,
                    timestamp_48k,
                    opus,
                } => Some(JsVoiceEvent::Heard {
                    room: workspace_of(workspace_id, &self.voice_workspaces),
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
                    room: workspace_of(workspace_id, &self.voice_workspaces),
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
                        room: workspace_of(workspace_id, &self.voice_workspaces),
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
    /// on `workspace`'s live plane as this page's voice, in the order given:
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
            .send(Cmd::CallAudio(workspace_number(workspace)?, frames))
            .map_err(js_error)
    }

    /// Publish `pictures` this page's camera and screen encoded on the plane
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
            .send(Cmd::CallPictures(workspace_number(workspace)?, pictures))
            .map_err(js_error)
    }

    /// Show the devices' layers of `shown` on the plane of `workspace`, as
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
            .send(Cmd::CallShowing(workspace_number(workspace)?, shown))
            .map_err(js_error)
    }

    /// Ask `device` (hex) for `source` (`camera` or `screen`) afresh on the
    /// plane of `workspace`: its next picture starts a group this page can
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
                workspace_number(workspace)?,
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
    /// members' place, or `null` while it has a plane: no plane could be
    /// reached.
    #[wasm_bindgen(js_name = liveNote)]
    pub fn live_note(&self) -> Option<String> {
        self.handle.live_note()
    }

    /// How many changes this page made to its workspace: records it sent, and
    /// windows the person opened, closed or moved on its desktop.
    #[wasm_bindgen(js_name = authoredChanges)]
    pub fn authored_changes(&self) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a count far below 2^53, read by JavaScript as a number"
        )]
        let count = crate::replication::authored(self.id) as f64;
        count
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
                Event::LivePlane {
                    ws: workspace_id,
                    from,
                    track: Track::Presence,
                    body,
                    ..
                } if from != self.device_key => {
                    hear(self.id, workspace_id, from, &body);
                    None
                }
                // A window's page: the display list the device hosting it publishes, drawn in the
                // tab of that window.
                Event::LivePlane {
                    ws: workspace_id,
                    from,
                    track: Track::Page(tab),
                    body,
                    ..
                } if from != self.device_key => {
                    crate::pages::heard(self.id, workspace_id, tab, &body);
                    None
                }
                Event::Live {
                    ws: workspace_id,
                    from,
                    frame,
                } if from != self.device_key => {
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
                        // An agent's narrated work is drawn on this page's
                        // windows once its board admits the publisher.
                        LiveFrame::Ops(OpsFrame::Semantic { event, .. }) => {
                            crate::peer_sync::narrated(self.id, workspace_id, from, &event);
                            return None;
                        }
                        // Presence no longer comes this way.
                        LiveFrame::RetiredPresence => return None,
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
                        record: route.record,
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
        let number = workspace_number(workspace)?;
        let door = crate::peer_sync::actions_of(self.id, number)
            .ok_or_else(|| js_error("this page holds no session with a host"))?;
        let caller =
            tilo_actions::remote::Caller::local_person(person.to_owned()).in_workspace(workspace);
        tilo_session_core::remote::ask(&door, &caller, message)
            .await
            .map_err(|error| js_error(&error))
    }

    /// Have this page's device in the person's own workspace on the host, asked on the session it
    /// holds there: made the first time, answered again after, another for each `name`, the one
    /// called `room` (a UUID) when it is named ([`Device::enter_own_workspace`]). This very device
    /// redeems the invitation on that session, shown to the workspace's people as `shownAs`,
    /// unless `entered(workspace)` says it is in already. Resolves to the workspace's listing,
    /// `{workspace, workspace_id, name, generation}` JSON.
    ///
    /// # Errors
    ///
    /// Rejects with an `Error` named `HostRefusal` whose `code` says how the host refused
    /// ([`HostRefusal`]: `unknown_action` for a host that keeps no workspaces of people's own) and
    /// whose `refusal` is the host's typed refusal where it gave one.
    #[wasm_bindgen(js_name = enterOwnWorkspace)]
    pub async fn enter_own_workspace(
        &self,
        name: Option<String>,
        room: Option<String>,
        shown_as: String,
        entered: Function,
    ) -> Result<String, JsValue> {
        let listing = self
            .device
            .enter_own_workspace(name.as_deref(), room.as_deref(), &shown_as, |id| {
                entered
                    .call1(&JsValue::NULL, &JsValue::from_str(id))
                    .is_ok_and(|answer| answer.is_truthy())
            })
            .await
            .map_err(|refusal| refusal_error(&refusal))?;
        serde_json::to_string(&listing).map_err(|error| js_error(error).into())
    }

    /// Whether the page holds the workspace's record `record` of `app`: open in
    /// a mount, or received from the workspace and stored for whoever opens it.
    #[wasm_bindgen(js_name = hasRecord)]
    pub fn has_record(&self, app: &str, record: &str) -> bool {
        crate::replication::has_record(self.id, app, record)
    }

    /// The name of the workspace in front on the workspace's shared desktop, as
    /// its people last set it; `undefined` until the workspace delivered it.
    #[wasm_bindgen(js_name = workspaceName)]
    pub fn workspace_name(&self) -> Option<String> {
        crate::replication::workspace_name(self.id)
    }

    /// The workspace's records this page holds, as JSON `[{app, record}]`, so a
    /// page can hand a guest the record its desktop created.
    #[wasm_bindgen(js_name = workspaceRecords)]
    pub fn workspace_records(&self) -> String {
        let records: Vec<JsRecord> = crate::replication::records(self.id)
            .into_iter()
            .map(|(app, record)| JsRecord { app, record })
            .collect();
        serde_json::to_string(&records).unwrap_or_else(|_| "[]".into())
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
                record: record.into(),
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

    /// The person's chosen name from now on, for history and presence
    /// (empty for none).
    #[wasm_bindgen(js_name = setDisplayName)]
    pub fn set_display_name(&self, name: String) {
        set_label(self.id, &name);
        if let Some(workspace) = self.speaking_workspace.borrow().as_deref() {
            set_member_name(&self.handle, workspace, &name);
        }
        if crate::page_person::speaks(self.id) {
            install_identity(&self.reporter_cert, name);
        }
    }

    /// End the session. The page no longer acts as the certified user; a
    /// lobby disposed beside the workspace's peer leaves the person's identity
    /// and conversations to that one.
    pub fn dispose(&self) {
        self.listeners.borrow_mut().clear();
        self.handle.close();
        release(self.id);
        crate::replication::close(self.id);
        if crate::page_person::release(self.id) {
            ui::local_identity::install_source(None);
        }
    }
}
